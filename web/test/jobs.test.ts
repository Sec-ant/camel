import "./browser.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { setImmediate as tick } from "node:timers/promises";
import type { Session } from "../src/session.ts";
import { start } from "../src/state.ts";
import { cancelJob, currentJob, jobRunning, runOperation } from "../src/pipeline/jobs.ts";

class FakeWorker {
  static current: FakeWorker;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  runId = 0;
  constructor() { FakeWorker.current = this; }
  postMessage(message: { id: number; type: string }): void {
    if (message.type === "run") this.runId = message.id;
    else if (message.type === "cancel") this.reply({ id: message.id, kind: "ok", result: null });
  }
  reply(data: unknown): void { queueMicrotask(() => this.onmessage?.({ data })); }
  terminate(): void {}
}

Object.assign(globalThis, { Worker: FakeWorker, addEventListener: () => undefined, removeEventListener: () => undefined, location: { hash: "" } });
const project = { name: "book", source: "", font: "font", pages: [] };
let pendingPoll: PromiseWithResolvers<typeof project> | null = null;
const fake = {
  location: "overlay", index: [], font: { bytes: new ArrayBuffer(0) }, comic: null, comicId: null,
  dispose: () => undefined,
  files: { store: { descriptor: {}, list: async () => [] } },
  project: () => project, poll: async () => pendingPoll ? pendingPoll.promise : project, changeCount: async () => 0,
};
await start(fake as unknown as Session);
const spec = { kind: "check_terms", options: { output: "reports/terms.json" } } as const;

async function cancelledRun(reply: Record<string, unknown>) {
  const operation = runOperation(spec, "Check terms");
  await tick();
  const worker = FakeWorker.current;
  assert.ok(worker.runId);
  cancelJob();
  worker.reply({ id: worker.runId, ...reply });
  await operation;
  return currentJob.value!;
}

test("completion after a cancellation request remains a completed job", async () => {
  const job = await cancelledRun({ kind: "ok", result: { message: "complete", output: null } });
  assert.equal(job.status, "done");
  assert.equal(job.result?.message, "complete");
});

test("a failure after a cancellation request remains a failure", async () => {
  const job = await cancelledRun({ kind: "error", error: "disk full", name: "Error" });
  assert.equal(job.status, "failed");
  assert.equal(job.error, "disk full");
});

test("an acknowledged cancellation is reported as cancelled", async () => {
  const job = await cancelledRun({ kind: "error", error: "cancelled", name: "CamelCancelled" });
  assert.equal(job.status, "cancelled");
});


test("a job stays active until its project refresh finishes", async () => {
  pendingPoll = Promise.withResolvers<typeof project>();
  const operation = runOperation(spec, "Check terms");
  await tick();
  const worker = FakeWorker.current;
  worker.reply({ id: worker.runId, kind: "ok", result: { message: "complete", output: null } });
  await tick();
  assert.equal(currentJob.value?.status, "finalizing");
  assert.equal(jobRunning(), true);
  await assert.rejects(runOperation(spec, "Check terms"), /Another job is running/);
  pendingPoll.resolve(project);
  await operation;
  pendingPoll = null;
  assert.equal(currentJob.value?.status, "done");
});
