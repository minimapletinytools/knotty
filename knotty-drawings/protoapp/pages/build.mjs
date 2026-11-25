import { build } from "esbuild";
import { cp, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import "../monaco/build.mjs";

const app = fileURLToPath(new URL("../", import.meta.url));
const out = path.resolve(app, "../../dist");
const demos = [
  ["lantern-waddle", "The Lantern Waddle", "examples/lantern-waddle.knot"],
  ["bezier", "Bézier gradient", "example-drawing.knot"],
  ["makers-fair", "Makers Fair", "examples/makers-fair.knot"],
  ["circle-creation", "Circle creation", "examples/mapped-creation.knot"],
];
const text = name => readFile(name, "utf8");
await mkdir(path.join(out, "assets"), { recursive: true });
const assets = path.join(out, "assets");
for (const file of ["app.js", "paint.js", "geometry.js", "reconcile.js", "source.js", "style.css", "source-editor"]) {
  await cp("web/" + file, path.join(assets, file), { recursive: true });
}
await cp("pages/transport.js", path.join(assets, "transport.js"));
await build({ entryPoints: ["pages/runtime.worker.js"], bundle: true, format: "esm", minify: true,
  outfile: path.join(assets, "runtime.worker.js") });
execFileSync("cargo", ["build", "--locked", "--release", "--lib", "--target", "wasm32-wasip1"], { stdio: "inherit" });
await cp("target/wasm32-wasip1/release/knottydrawings.wasm", path.join(assets, "drawing.wasm"));
const names = ["KnottyDrawings", "DrawingBack", "Requests", "Scene", "Types"];
const libs = await Promise.all(names.map(async name => [name + ".knot", await text("../" + name + ".knot")]));
await writeFile(path.join(assets, "libraries.json"), JSON.stringify(Object.fromEntries(libs)));
const drawingHtml = await text("web/index.html");
for (const [slug, title, source] of demos) {
  const dir = path.join(out, "drawings", slug);
  await mkdir(dir, { recursive: true });
  await cp("../" + source, path.join(dir, "source.knot"));
  const svg = execFileSync("cargo", ["run", "--locked", "--quiet", "--release", "--bin", "knottydrawings",
    "--", "--export-svg", "../" + source], { maxBuffer: 16 * 1024 * 1024 });
  await writeFile(path.join(dir, "preview.svg"), svg);
  await writeFile(path.join(dir, "index.html"), drawingHtml
    .replace("<head>", '<head>\n  <base href="../../assets/">')
    .replace("<body>", `<body data-demo="${slug}" data-source-path="knotty-drawings/${source}">`)
    .replace("<title>Knotty Drawings", `<title>${title} · Knotty Drawings`)
    .replace('data-src="/tangle?embed=1"', 'data-src="../tangle/?embed=1"')
    .replace('<a href="/tangle" class="back-link">Full editor</a>', '<a href="../" class="back-link">← Back to demos</a>')
    .replace('<script src="app.js">', '<script src="transport.js"></script>\n  <script src="app.js">'));
}
const stories = execFileSync("cargo", ["run", "--locked", "--quiet", "--release", "--bin", "knottydrawings", "--", "--export-stories"],
  { maxBuffer: 32 * 1024 * 1024 });
const dir = path.join(out, "tangle");
await mkdir(dir, { recursive: true });
await writeFile(path.join(dir, "stories.json"), stories);
await cp("web/tangle/editor.js", path.join(dir, "editor.js"));
await cp("web/tangle/editor.css", path.join(dir, "editor.css"));
await writeFile(path.join(dir, "index.html"), (await text("web/tangle/index.html"))
  .replace('<html lang="en"', '<html data-catalog="./stories.json" data-monaco="../assets/source-editor/" lang="en"')
  .replace('href="/tangle.css"', 'href="./editor.css"').replace('src="/tangle.js"', 'src="./editor.js"')
  .replace('href="/tangle/stories"', 'href="./"').replace('href="/"', 'href="../"'));
const cards = demos.map(([slug, title]) =>
  `<a class="drawing" href="./drawings/${slug}/"><img src="./drawings/${slug}/preview.svg" alt=""><span>${title} ↗</span></a>`);
await writeFile(path.join(out, "index.html"), (await text("pages/index.html")).replace("<!-- drawings -->", cards.join("\n")).replace("REPOSITORY_SOURCE", "https://github.com/minimapletinytools/knotty/tree/" + (process.env.GITHUB_SHA || "main")));
await cp("pages/gallery.css", path.join(out, "gallery.css"));
await cp("../../LICENSE", path.join(out, "LICENSE"));
await writeFile(path.join(out, ".nojekyll"), "");


// Ship the notices alongside the code actually distributed in the browser.
const notices = path.join(out, "licenses");
const copyNotices = async (name, source) => {
  const dir = path.join(notices, name);
  await mkdir(dir, { recursive: true });
  for (const filename of await readdir(source)) {
    if (/^(license|licence|copying|copyright|thirdpartynotices|notice)([-._]|$)/i.test(filename)) {
      await cp(path.join(source, filename), path.join(dir, filename), { recursive: true });
    }
  }
};
for (const name of ["monaco-editor", "@bjorn3/browser_wasi_shim"]) {
  await copyNotices(name, path.join("node_modules", name));
}
const meta = JSON.parse(execFileSync("cargo", ["metadata", "--locked", "--format-version", "1", "--filter-platform", "wasm32-wasip1"]));
for (const pkg of meta.packages.filter(pkg => pkg.source)) {
  await copyNotices(pkg.name + "-" + pkg.version, path.dirname(pkg.manifest_path));
}
