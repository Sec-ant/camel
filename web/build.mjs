// Bundles the workbench into dist/: index.html, assets/app.{js,css}, assets/worker.js and the
// camel-web wasm binary. `node build.mjs --serve` rebuilds on change and serves dist on
// http://localhost:5173 (a secure context, as the File System Access API requires).
import { access, copyFile, mkdir } from "node:fs/promises";
import { context } from "esbuild";

const serve = process.argv.includes("--serve");
const GLUE = "src/wasm/camel_web.js";
const WASM = "src/wasm/camel_web_bg.wasm";

const exists = (path) =>
  access(path).then(
    () => true,
    () => false,
  );

const haveWasm = (await exists(GLUE)) && (await exists(WASM));
if (!haveWasm) {
  console.warn(
    `warning: ${GLUE} / ${WASM} not found; building with a stub (build crates/camel-web into web/src/wasm). The open screen works, opening a project does not.`,
  );
}

// Stands in for the wasm-bindgen glue until crates/camel-web has been built.
const wasmStub = {
  name: "camel-web-stub",
  setup(build) {
    build.onResolve({ filter: /wasm\/camel_web\.js$/ }, () => ({ path: "camel-web-stub", namespace: "stub" }));
    build.onLoad({ filter: /.*/, namespace: "stub" }, () => ({
      contents: `export default async function init() { throw new Error("The camel-web module has not been built (web/src/wasm is empty)"); }
export const Comic = null, Workbench = null, applyEdits = null, repairPlan = null, summarize = null, verifyPage = null, zipFiles = null;`,
    }));
  },
};

const copyStatic = {
  name: "copy-static",
  setup(build) {
    build.onEnd(async () => {
      await copyFile("index.html", "dist/index.html");
      if (haveWasm) await copyFile(WASM, "dist/assets/camel_web_bg.wasm");
    });
  },
};

await mkdir("dist/assets", { recursive: true });
const ctx = await context({
  entryPoints: [
    { in: "src/main.tsx", out: "app" },
    { in: "src/worker.ts", out: "worker" },
    { in: "src/styles.css", out: "app" },
  ],
  outdir: "dist/assets",
  bundle: true,
  minify: true,
  format: "esm",
  target: ["chrome120", "safari17", "firefox121"],
  jsx: "automatic",
  jsxImportSource: "preact",
  charset: "utf8",
  legalComments: "none",
  logLevel: "info",
  plugins: haveWasm ? [copyStatic] : [wasmStub, copyStatic],
});

if (serve) {
  await ctx.watch();
  const { port } = await ctx.serve({ servedir: "dist", port: 5173, host: "localhost" });
  console.log(`serving http://localhost:${port}/`);
} else {
  await ctx.rebuild();
  await ctx.dispose();
}
