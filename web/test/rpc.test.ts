import assert from "node:assert/strict";
import { test } from "node:test";
import { serve, WorkerClient } from "../src/rpc.ts";

/** Stands in for a dedicated worker: records posted messages, never answers on its own. */
class FakeWorker {
  static last: FakeWorker;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  onmessageerror: (() => void) | null = null;
  posted: unknown[] = [];
  terminated = false;

  constructor() {
    FakeWorker.last = this;
  }

  postMessage(message: unknown): void {
    this.posted.push(message);
  }

  terminate(): void {
    this.terminated = true;
  }
}

Object.assign(globalThis, { Worker: FakeWorker });

type Requests = { echo: { request: string; result: string } };

function client(): { client: WorkerClient<Requests>; worker: FakeWorker } {
  const created = new WorkerClient<Requests>(new URL("file:///worker.js"), "test worker");
  return { client: created, worker: FakeWorker.last };
}

test("calls resolve with the worker's answer", async () => {
  const { client: rpc, worker } = client();
  const answer = rpc.call("echo", "hi");
  worker.onmessage!({ data: { id: 1, kind: "ok", result: "hi back" } });
  assert.equal(await answer, "hi back");
  assert.equal(rpc.failure, null);
});

test("a worker error fails pending and later calls and stops the worker", async () => {
  const { client: rpc, worker } = client();
  const pending = rpc.call("echo", "a");
  worker.onerror!({ message: "boom" });
  await assert.rejects(pending, /The test worker stopped: boom/);
  await assert.rejects(rpc.call("echo", "b"), /The test worker stopped: boom/);
  assert.equal(worker.posted.length, 1);
  assert.equal(worker.terminated, true);
  assert.match(rpc.failure!.message, /boom/);
});

test("an unreadable message ends the client", async () => {
  const { client: rpc, worker } = client();
  const pending = rpc.call("echo", "a");
  worker.onmessageerror!();
  await assert.rejects(pending, /could not be read/);
  await assert.rejects(rpc.call("echo", "b"), /could not be read/);
});

test("terminate rejects pending and later calls", async () => {
  const { client: rpc } = client();
  const pending = rpc.call("echo", "a");
  rpc.terminate();
  await assert.rejects(pending, /stopped/);
  await assert.rejects(rpc.call("echo", "b"), /stopped/);
});

test("a fatal response rejects every request and terminates the worker", async () => {
  const { client: rpc, worker } = client();
  const pending = [rpc.call("echo", "a"), rpc.call("echo", "b")];
  worker.onmessage!({ data: { id: 1, kind: "fatal", error: "camel-web panicked: invalid state" } });
  for (const request of pending) await assert.rejects(request, /camel-web panicked: invalid state/);
  await assert.rejects(rpc.call("echo", "c"), /camel-web panicked: invalid state/);
  assert.equal(worker.terminated, true);
});

test("a WASM panic or trap makes the server reject later requests without entering WASM again", async () => {
  for (const error of [new Error("camel-web panicked: invalid state"), new WebAssembly.RuntimeError("unreachable")]) {
    const replies: { kind: string; error: string }[] = [];
    const scope = { onmessage: null as ((event: { data: unknown }) => void) | null, postMessage: (message: typeof replies[number]) => replies.push(message) };
    Object.assign(globalThis, { self: scope });
    let calls = 0;
    serve<Requests>({ echo: () => { calls++; throw error; } }, Promise.resolve());
    scope.onmessage!({ data: { id: 1, type: "echo", payload: "first" } });
    await new Promise(setImmediate);
    scope.onmessage!({ data: { id: 2, type: "echo", payload: "second" } });
    await new Promise(setImmediate);
    assert.equal(calls, 1);
    assert.deepEqual(replies.map(({ kind, error }) => ({ kind, error })), [
      { kind: "fatal", error: error.message }, { kind: "fatal", error: error.message },
    ]);
  }
});
