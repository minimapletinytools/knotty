import { build } from "esbuild";
await build({
  entryPoints: { editor: "monaco/editor.js", "editor.worker": "monaco-editor/editor/editor.worker.js" },
  outdir: "web/source-editor", format: "esm", bundle: true, minify: true, loader: { ".ttf": "dataurl" },
});
