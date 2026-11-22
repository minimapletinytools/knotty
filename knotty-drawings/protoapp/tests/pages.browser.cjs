// Serve the built site beneath a repository prefix, as on GitHub Pages.
const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { chromium } = require("playwright");
const root = path.resolve(__dirname, "../../../dist");
const types = { ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml",
  ".html": "text/html", ".json": "application/json", ".wasm": "application/wasm" };
const server = createServer(async (req, res) => {
  try {
    const pathname = new URL(req.url, "http://localhost").pathname;
    if (!pathname.startsWith("/knotty/")) throw Error();
    const file = path.join(root, pathname.slice(8), pathname.endsWith("/") ? "index.html" : "");
    const bytes = await readFile(file);
    res.writeHead(200, { "Content-Type": types[path.extname(file)] || "text/plain" }).end(bytes);
  } catch { res.writeHead(404).end(); }
});

(async () => {
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const browser = await chromium.launch();
  try {
    const base = `http://127.0.0.1:${server.address().port}/knotty/`;
    const page = await browser.newPage({ viewport: { width: 1440, height: 960 } });
    const errors = [];
    page.on("pageerror", e => errors.push(e.message));
    page.on("response", r => { if (r.status() >= 400) errors.push(r.url()); });
    await page.goto(base);
    const demos = await page.locator("a.drawing").evaluateAll(links => links.map(link => link.href));
    assert.equal(demos.length, 3);
    for (const demo of demos) {
      await page.goto(demo);
      await page.locator("#stage svg").first().waitFor();
      await page.locator("#source-editor .monaco-editor").waitFor();
      assert.match(await page.locator("#file-status").textContent(), /Up to date/);
    }
    await page.goto(base + "drawings/bezier/");
    await page.locator("#select-curve").click();
    const density = page.getByRole("slider", { name: "Shape density" });
    await density.focus();
    await density.press("ArrowLeft");
    await page.waitForFunction(() => document.querySelector(".view-lines")?.innerText.replaceAll("\u00a0", " ").includes("shapeCount = (63)"));
    await page.goto(base + "drawings/circle-creation/");
    await page.locator("#stage svg circle").first().click();
    const code = page.locator("#source-editor .view-lines");
    const original = await code.innerText();
    for (const finish of ["cancel", "ok"]) {
      await page.locator("#field-r").click();
      await page.locator("dialog input").fill("42");
      await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 42);
      assert.match((await code.innerText()).replaceAll("\u00a0", " "), /radius = \(42/);
      await page.locator(`dialog [data-action=${finish}]`).click();
      await page.locator("dialog").waitFor({ state: "detached" });
      if (finish === "ok") await page.locator("#undo-edit").click();
      await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 24);
      assert.equal(await code.innerText(), original);
    }
    await page.locator("#stage svg").first().click({position: { x: 4, y: 4 }});
    await page.locator("#add-circle").click();
    await page.waitForFunction(() => circleTool?.ready);
    const points = await page.locator("#stage svg").first().evaluate(svg =>
      [280, 300].map(x => {
        const p = new DOMPoint(x, 200).matrixTransform(svg.getScreenCTM());
        return { x: p.x, y: p.y };
      }));
    await page.mouse.move(points[0].x, points[0].y);
    await page.mouse.down();
    await page.mouse.move(points[1].x, points[1].y, { steps: 5 });
    await page.mouse.up();
    await page.waitForFunction(() => document.querySelectorAll("#stage svg circle").length === 3);
    assert((await code.innerText()).replaceAll("\u00a0", " ").includes("growMap defaultCircleInput"));
    await page.locator("#undo-edit").click();
    await page.waitForFunction(() => document.querySelectorAll("#stage svg circle").length === 2);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await page.locator("#stage svg").first().waitFor();
    assert((await page.locator("#canvas").boundingBox()).width > 300);
    assert(await page.locator("#properties-panel").isHidden());
    assert(await page.locator("#tree-panel").isHidden());
    assert((await page.locator("#source-editor").boundingBox()).height > 100);
    await page.goto(base + "tangle/?definition=incomplete_case");
    await page.getByText("⚠ Incomplete match").waitFor();
    const stories = page.locator("#stories a");
    assert.equal(await stories.count(), 63);
    await stories.filter({ hasText: "unreachable case" }).click();
    await page.getByText("⚠ Unreachable arm").waitFor();
    assert.deepEqual(errors, []);
    console.log("Static drawing, live Monaco, preview/cancel/accept/undo, mobile and storybook checks passed.");
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; }).finally(() => server.close());
