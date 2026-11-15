// Analyzer worker: one recognition engine (wasm `Analyzer`, single-threaded) recognizing the
// pages the main thread hands it, one at a time.

import { type Handlers, serve } from "../rpc.ts";
import { openStore } from "../storage/store.ts";
import init, { Analyzer, type JsHost } from "../wasm/camel_web.js";
import { Cancellation, createHost } from "./host.ts";
import type { AnalyzerRequests, PipelineEvent } from "./protocol.ts";

let session: { readonly analyzer: Analyzer; readonly host: JsHost } | null = null;
// Events go to the call in flight; a worker analyzes one page at a time.
let emitEvent: (event: PipelineEvent) => void = () => undefined;

const handlers: Handlers<AnalyzerRequests, PipelineEvent> = {
  async init({ store, detector, recognizer }) {
    const models = await Promise.all([detector, recognizer].map(async (file) => new Uint8Array(await file.arrayBuffer())));
    session?.analyzer.free();
    session = {
      analyzer: new Analyzer(models[0]!, models[1]!),
      host: createHost(openStore(store), new Cancellation(), (event) => emitEvent(event)),
    };
    return [null];
  },
  async page({ number, total, bytes }, emit) {
    if (!session) throw new Error("The analyzer was not initialized");
    emitEvent = emit;
    await session.analyzer.analyzePage(session.host, number, total, new Uint8Array(bytes));
    return [null];
  },
};

serve<AnalyzerRequests, PipelineEvent>(handlers, init({ module_or_path: new URL("camel_web_bg.wasm", import.meta.url) }));
