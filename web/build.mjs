// Bundles the workbench into dist/: index.html, assets/app.{js,css}, the workers
// (assets/worker.js: page engine, assets/pipeline-worker.js: long operations,
// assets/analyzer-worker.js: one recognition engine each) and the camel-web wasm binary,
// which must have been built into src/wasm first (`just web`).
// `node build.mjs --serve` rebuilds on change and serves dist on http://localhost:5173 (a
// secure context, as the File System Access API requires).
// Targets: Chromium 120, Firefox 124 and Safari 17.4, the first releases with every API the
// code uses (AbortSignal.any, Promise.withResolvers).
import { access, copyFile, mkdir } from "node:fs/promises";
import { context } from "esbuild";

const serve = process.argv.includes("--serve");
const GLUE = "src/wasm/camel_web.js";
const WASM = "src/wasm/camel_web_bg.wasm";

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
