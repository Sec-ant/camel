import assert from "node:assert/strict";
import { test } from "node:test";
import { setImmediate as tick } from "node:timers/promises";
import { idbGet, idbSet } from "../src/storage/idb.ts";

/** A minimal IndexedDB: the test decides when requests succeed and transactions finish. */
class FakeRequest {
  result: unknown = undefined;
  error: Error | null = null;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;

  succeed(result: unknown): void {
    this.result = result;
    this.onsuccess?.();
  }

  fail(error: Error): void {
    this.error = error;
    this.onerror?.();
  }
}

class FakeTransaction {
  error: Error | null = null;
  oncomplete: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  readonly requests: FakeRequest[] = [];
  readonly values = new Map<string, unknown>();

  objectStore(): object {
    const request = (): FakeRequest => {
      const created = new FakeRequest();
      this.requests.push(created);
      return created;
    };
    return {
      put: (value: unknown, key: string) => {
        this.values.set(key, value);
        return request();
      },
      get: () => request(),
      delete: () => request(),
    };
  }
}

const transactions: FakeTransaction[] = [];
const opens: FakeRequest[] = [];
const database = {
  onversionchange: null,
  onclose: null,
  transaction(): FakeTransaction {
    const transaction = new FakeTransaction();
    transactions.push(transaction);
    return transaction;
  },
  close(): void {},
};

Object.assign(globalThis, {
  indexedDB: {
    open(): FakeRequest {
      const request = new FakeRequest();
      opens.push(request);
      return request;
    },
  },
});

async function settled(promise: Promise<unknown>): Promise<boolean> {
  let done = false;
  promise.then(
    () => (done = true),
    () => (done = true),
  );
  await tick();
  return done;
}

test("a failed open is not kept: the next request opens again", async () => {
  const first = idbGet("a");
  opens[0]!.fail(new Error("blocked"));
  await assert.rejects(first, /blocked/);
  const second = idbGet("a");
  await tick();
  assert.equal(opens.length, 2);
  opens[1]!.succeed(database);
  await tick();
  transactions.at(-1)!.requests[0]!.succeed("value");
  assert.equal(await second, "value");
});

test("a write resolves when its transaction commits, not when the request succeeds", async () => {
  const write = idbSet("key", "value");
  await tick();
  const transaction = transactions.at(-1)!;
  transaction.requests[0]!.succeed(undefined);
  assert.equal(await settled(write), false);
  transaction.oncomplete!();
  await write;
});

test("a write rejects when its transaction aborts after the request succeeded", async () => {
  const write = idbSet("key", "value");
  await tick();
  const transaction = transactions.at(-1)!;
  transaction.requests[0]!.succeed(undefined);
  transaction.error = new Error("QuotaExceededError");
  transaction.onabort!();
  await assert.rejects(write, /QuotaExceededError/);
});
