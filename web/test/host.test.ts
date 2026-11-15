import assert from "node:assert/strict";
import { test } from "node:test";
import { Cancellation, createHost, requestError } from "../src/pipeline/host.ts";
import type { PipelineEvent } from "../src/pipeline/protocol.ts";
import { HostError, type KeyValue, OverlayStore } from "../src/storage/store.ts";

function store(): OverlayStore {
  const map = new Map<string, unknown>();
  const kv: KeyValue = {
    get: <T>(key: string) => Promise.resolve(map.get(key) as T | undefined),
    set: (key, value) => Promise.resolve(void map.set(key, value)),
    delete: (key) => Promise.resolve(void map.delete(key)),
    keys: (prefix) => Promise.resolve([...map.keys()].filter((key) => key.startsWith(prefix))),
  };
  return new OverlayStore("p", new Map(), kv);
}

test("host file methods map onto the store with compare-and-swap writes", async () => {
  const host = createHost(store(), new Cancellation(), () => undefined);
  assert.equal(await host.read("00001.json"), null);
  await host.write("00001.json", new TextEncoder().encode("a"), { kind: "create_new" });
  const revision = await host.revision("00001.json");
  await assert.rejects(
    host.write("00001.json", new TextEncoder().encode("b"), { kind: "replace_if_revision", revision: "other" }),
    (error: unknown) => error instanceof HostError && error.kind === "conflict",
  );
  await host.write("00001.json", new TextEncoder().encode("b"), { kind: "replace_if_revision", revision: revision! });
  assert.equal(new TextDecoder().decode((await host.read("00001.json"))!), "b");
});

test("host forwards reported events and observes cancellation", async () => {
  const events: PipelineEvent[] = [];
  const cancellation = new Cancellation();
  const host = createHost(store(), cancellation, (event) => events.push(event));
  host.report(JSON.stringify({ type: "warning", page: null, message: "slow" }));
  assert.deepEqual(events, [{ type: "warning", page: null, message: "slow" }]);
  assert.equal(host.cancelled(), false);
  const sleeping = host.sleep(60_000);
  cancellation.cancel();
  await sleeping;
  assert.equal(host.cancelled(), true);
});

test("failed requests are transient network errors with a reason", () => {
  const timeout = requestError(new DOMException("timed out", "TimeoutError"), 120_000, false);
  assert.equal(timeout.kind, "network");
  assert.equal(timeout.message, "no response within 120 s");
  assert.equal(requestError(new DOMException("aborted", "AbortError"), 1000, true).message, "cancelled");
  assert.match(requestError(new TypeError("Failed to fetch"), 1000, false).message, /could not be reached \(Failed to fetch\)/);
});
