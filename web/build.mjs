// Bundles the workbench into dist/: index.html, assets/app.{js,css}, the workers
// (assets/worker.js: page engine, assets/pipeline-worker.js: long operations,
// assets/analyzer-worker.js: one recognition engine each) and the camel-web wasm binary,
// which must have been built into src/wasm first (`just web`).
// `node build.mjs --serve` rebuilds on change and serves dist on http://localhost:5173 (a
// secure context, as the File System Access API requires).
// Targets: Chromium 120, Firefox 124 and Safari 17.4, the first releases with every API the
// code uses (AbortSignal.any, Promise.withResolvers).
import { createHash } from "node:crypto";
import { access, cp, copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { context } from "esbuild";

const serve = process.argv.includes("--serve");
const GLUE = "src/wasm/camel_web.js";
const WASM = "src/wasm/camel_web_bg.wasm";
const models = [
  { name: "LXGWWenKai-Medium.ttf", url: "https://github.com/lxgw/LxgwWenKai/releases/download/v1.530/LXGWWenKai-Medium.ttf", sha256: "f4b8a34d1f838fe411540cb7c97b1c384ac9738a876c4104500bc42508b2312a" },
  { name: "detector.onnx", url: "https://huggingface.co/ogkalu/comic-text-and-bubble-detector/resolve/16e8a622f91fabc6b5b65c96d32d1183f8843546/detector-v4-s_int8.onnx", sha256: "5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79" },
  { name: "recognizer.onnx", url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.10.0/onnx/PP-OCRv5/rec/en_PP-OCRv5_rec_mobile.onnx", sha256: "c3461add59bb4323ecba96a492ab75e06dda42467c9e3d0c18db5d1d21924be8" },
];

// Download the pinned upstream models once; only the verified files enter the site build.
await mkdir("models", { recursive: true });
for (const model of models) {
  const path = `models/${model.name}`;
  let bytes = await readFile(path).catch((error) => {
    if (error.code === "ENOENT") return null;
    throw error;
  });
  const digest = (data) => createHash("sha256").update(data).digest("hex");
  if (!bytes || digest(bytes) !== model.sha256) {
    console.log(`Downloading ${model.name}…`);
    const response = await fetch(model.url);
    if (!response.ok) throw new Error(`Download ${model.name}: HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    if (digest(bytes) !== model.sha256) throw new Error(`Invalid SHA-256 for ${model.name}`);
    await writeFile(path, bytes);
  }
}

for (const path of [GLUE, WASM]) {
  await access(path).catch(() => {
    console.error(`error: ${path} not found; build crates/camel-web into web/src/wasm first (just web)`);
    process.exit(1);
  });
}

const copyStatic = {
  name: "copy-static",
  setup(build) {
    build.onEnd(async () => {
      await copyFile("index.html", "dist/index.html");
      await copyFile(WASM, "dist/assets/camel_web_bg.wasm");
      await mkdir("dist/assets/models", { recursive: true });
      for (const model of models) await copyFile(`models/${model.name}`, `dist/assets/models/${model.name}`);
      await cp("sample", "dist/assets/sample", { recursive: true });
    });
  },
};

await mkdir("dist/assets", { recursive: true });
const ctx = await context({
  entryPoints: [
    { in: "src/main.tsx", out: "app" },
    { in: "src/worker.ts", out: "worker" },
    { in: "src/pipeline/worker.ts", out: "pipeline-worker" },
    { in: "src/pipeline/analyzer.ts", out: "analyzer-worker" },
    { in: "src/styles.css", out: "app" },
  ],
  outdir: "dist/assets",
  bundle: true,
  minify: true,
  format: "esm",
  target: ["chrome120", "safari17.4", "firefox124"],
  jsx: "automatic",
  jsxImportSource: "preact",
  charset: "utf8",
  legalComments: "none",
  logLevel: "info",
  plugins: [copyStatic],
});

if (serve) {
  await ctx.watch();
  const { port } = await ctx.serve({ servedir: "dist", port: 5173, host: "localhost" });
  console.log(`serving http://localhost:${port}/`);
} else {
  await ctx.rebuild();
  await ctx.dispose();
}
