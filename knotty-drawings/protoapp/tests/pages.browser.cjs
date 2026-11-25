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

async function checkLanternWaddle(page) {
  const stage = page.locator("#stage");
  const leadScarf = stage.locator("#lead-scarf");
  const moon = stage.locator("#moon");
  const chicks = stage.locator("#chicks > g");
  const snow = stage.locator("#snow > circle");
  const source = () => page.evaluate(async () =>
    (await (await window.drawingGraphHost.request("/api/scene")).json()).source);
  const chickSlider = page.getByRole("slider", { name: "Trailing chicks" });
  await page.getByRole("heading", { name: "Application properties" }).waitFor();
  assert.equal(await page.getByRole("combobox", { name: "Locked fields" }).count(), 0);
  await page.getByRole("tab", { name: "Options" }).click();
  await page.getByLabel("Locked fields").selectOption("lock");
  assert(await page.locator("#properties").evaluate(el => el.classList.contains("locked-lock")));
  await page.getByRole("tab", { name: "Log" }).click();
  await page.getByRole("log").getByText(/Loaded lantern-waddle/).first().waitFor();
  await page.getByRole("tab", { name: "Properties" }).click();
  const toolbar = page.locator("#shape-tools");
  await toolbar.locator("summary").click();
  assert(await toolbar.getByRole("button", { name: "Rectangle" }).isHidden());
  await toolbar.locator("summary").click();
  const beforeShapes = await source();
  for (const kind of ["rect", "ellipse"]) {
    await page.locator("#add-" + kind).click();
    await page.waitForFunction(() => shapeTool?.ready || document.querySelector("#creation-status").classList.contains("error"));
    assert(await page.evaluate(() => shapeTool.ready), await page.locator("#creation-status").innerText());
    const canvas = await page.locator("#canvas").boundingBox();
    const x = canvas.x + canvas.width * 0.62, y = canvas.y + canvas.height * 0.58;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x - 32, y - 18, { steps: 4 });
    await page.locator("#creation-overlay " + kind).waitFor();
    await page.mouse.up();
    await page.waitForFunction(() => !shapeTool);
    assert.equal(await page.evaluate(() => findNode(state.selected)?.kind), kind);
    const added = await source();
    assert.notEqual(added, beforeShapes);
    const dimension = kind === "rect" ? "width" : "rx";
    await page.locator("#field-" + dimension).click();
    await page.locator("dialog input").fill("18");
    await page.waitForFunction(name => Number(findNode(state.selected)?.props[name]) === 18, dimension);
    await page.locator("dialog [data-action=ok]").click();
    await page.locator("dialog").waitFor({ state: "detached" });
    await page.locator("#undo-edit").click();
    await page.waitForFunction(name => Number(findNode(state.selected)?.props[name]) !== 18, dimension);
    assert.equal(await source(), added);
    await page.locator("#undo-edit").click();
    await chickSlider.waitFor();
    assert.equal(await source(), beforeShapes);
  }
  assert.equal(await chickSlider.inputValue(), "3");
  await leadScarf.click();
  assert.equal(await chickSlider.count(), 0);
  await page.keyboard.press("Escape");
  await chickSlider.press("End");
  await chicks.nth(3).waitFor();
  assert.match(await source(), /chickCount = \(4\)/);
  assert.equal(await page.getByRole("slider", { name: "Snowflakes" }).count(), 0);

  await moon.click();
  const snowSlider = page.getByRole("slider", { name: "Snowflakes" });
  await snowSlider.press("ArrowRight");
  await snow.nth(28).waitFor();
  const beforeGust = await source();
  await snowSlider.press("ArrowRight");
  await snow.nth(29).waitFor();
  const afterGust = await source();
  assert.match(afterGust, /gusts = \(5\)/);
  assert.match(afterGust, /strays = \(0\)/);
  assert(afterGust.includes("snowCount = snowfall gusts strays"));
  await page.locator("#undo-edit").click();
  await snow.nth(29).waitFor({ state: "detached" });
  assert.equal(await source(), beforeGust);
  await snowSlider.press("Home");
  await snow.first().waitFor({ state: "detached" });
  assert.equal(await chicks.count(), 4);
  assert.equal(await page.getByRole("slider", { name: "Trailing chicks" }).count(), 0);

  // The first chick and the scout wear the negative of the leader's yarn.
  const original = await source();
  await chicks.first().locator("rect").first().click();
  await page.locator("#field-fill-paint").click();
  await page.locator(".paint-hex").fill("#c08040");
  await page.waitForFunction(() => document.querySelector("#lead-scarf").getAttribute("fill") === "rgb(63,127,191)");
  await page.locator("dialog [data-action=ok]").click();
  await page.locator("dialog").waitFor({ state: "detached" });
  assert.equal(await stage.locator("#scout > rect").first().getAttribute("fill"), "rgb(192,128,64)");
  assert.notEqual(await source(), original);
  await page.locator("#undo-edit").click();
  await page.waitForFunction(() => document.querySelector("#lead-scarf").getAttribute("fill") === "rgb(238,114,85)");
  assert.equal(await source(), original);

  // Changing the scout's halo from 13 to 16 sets flame to 12. The leader's
  // glass grows to 14, the chick's to 8, and each halo stays five units wider.
  const halo = stage.locator("#scout > circle[fill-opacity]");
  await halo.scrollIntoViewIfNeeded();
  // The glass covers the centre, so select a point on the exposed rim.
  const point = await halo.evaluate(el => {
    const point = new DOMPoint(el.cx.baseVal.value, el.cy.baseVal.value + el.r.baseVal.value - 2)
      .matrixTransform(el.getScreenCTM());
    return { x: point.x, y: point.y };
  });
  await page.mouse.click(point.x, point.y);
  await page.locator("#field-r").click();
  await page.locator("dialog input").fill("16");
  await page.waitForFunction(() => Number(document.querySelector("#leader > circle:last-of-type").getAttribute("r")) === 14);
  await page.locator("dialog [data-action=ok]").click();
  await page.locator("dialog").waitFor({ state: "detached" });
  assert.equal(Number(await chicks.first().locator("circle:last-of-type").getAttribute("r")), 8);
  assert.equal(Number(await halo.getAttribute("r")), 16);
  assert.match(await source(), /flame = \(12(?:\.0)?\)/);
  await page.locator("#undo-edit").click();
  await page.waitForFunction(() => Number(document.querySelector("#leader > circle:last-of-type").getAttribute("r")) === 10);
  assert.equal(await source(), original);
}

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
    const storyLink = page.getByRole("link", { name: "Open storybook" });
    for (const width of [390, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      const button = await storyLink.boundingBox(), card = await page.locator("a.drawing").first().boundingBox();
      assert(button.height >= 44 && button.y + button.height < card.y && button.y + button.height < 900);
    }
    const demos = await page.locator("a.drawing").evaluateAll(links => links.map(link => link.href));
    assert.equal(demos.length, 4);
    for (const demo of demos) {
      await page.goto(demo);
      await page.locator("#stage svg").first().waitFor();
      await page.locator("#source-editor .monaco-editor").waitFor();
      assert.match(await page.locator("#file-status").textContent(), /Up to date/);
    }
    await page.goto(base + "drawings/lantern-waddle/");
    await checkLanternWaddle(page);
    await page.goto(base + "drawings/bezier/");
    const density = page.getByRole("slider", { name: "Shape density" });
    await page.locator("#select-curve").click();
    assert.equal(await density.inputValue(), "64");
    await page.keyboard.press("Escape");
    assert.equal(await density.count(), 0);
    // Click a painted ribbon pixel, clear of the dashed stroke and decorations.
    const ribbon = page.locator("#stage #snake-ribbon");
    const hit = await ribbon.evaluate(path => {
      const length = path.getTotalLength(), transform = path.getScreenCTM();
      for (let n = 1; n < 100; n++) {
        const p = path.getPointAtLength(length * n / 100);
        for (const dx of [-2, 0, 2]) {
          const { x, y } = new DOMPoint(p.x + dx, p.y).matrixTransform(transform);
          if (document.elementFromPoint(x, y) === path) return { x, y };
        }
      }
    });
    assert.ok(hit, "a visible section of ribbon is selectable");
    await page.mouse.click(hit.x, hit.y);
    assert.match(await page.locator("#properties-title").textContent(), /snake-ribbon/);
    assert.equal(await density.inputValue(), "64");
    await density.focus();
    await density.press("ArrowLeft");
    await page.waitForFunction(() => document.querySelector(".view-lines")?.innerText.replaceAll("\u00a0", " ").includes("shapeCount = (63)"));
    await page.locator("#toggle-tangle").click();
    const graph = page.frameLocator("#drawing-graph");
    const picker = graph.getByRole("combobox", { name: "Drawing definition" });
    await graph.getByRole("region", { name: "program scope", exact: true }).waitFor();
    await picker.selectOption("shapeCount");
    const number = graph.locator("input.literal");
    await number.waitFor();
    assert.equal(await number.inputValue(), "63");
    await number.fill("31");
    assert(await graph.locator("header").isHidden());
    await number.press("Enter");
    await page.waitForFunction(() => document.querySelector(".drawing-slider input").value === "31");
    await page.locator("#toggle-source").click();
    await page.waitForFunction(() => document.querySelector(".view-lines").innerText.replaceAll("\u00a0", " ").includes("shapeCount = (31)"));
    await page.locator("#toggle-tangle").click();
    await page.locator("#undo-edit").click();
    await page.waitForFunction(() => document.querySelector(".drawing-slider input").value === "63");
    // A rejected graph edit can be discarded without changing the drawing.
    await page.waitForFunction(() => document.querySelector("#drawing-graph").contentDocument.querySelector("input.literal")?.value === "63");
    await number.fill("oops");
    await number.press("Enter");
    await graph.getByRole("button", { name: "Discard edits", exact: true }).click();
    await page.waitForFunction(() => document.querySelector("#drawing-graph").contentDocument.querySelector("input.literal")?.value === "63");
    await page.locator("#toggle-source").click();
    await page.goto(base + "drawings/circle-creation/");
    await page.locator("#stage svg circle").first().click();
    const code = page.locator("#source-editor .view-lines");
    const sourceLines = await code.elementHandle();
    await page.waitForFunction(lines => lines.innerText.replaceAll("\u00a0", " ").includes("radius = 24"), sourceLines);
    const original = await code.innerText();
    for (const finish of ["cancel", "ok"]) {
      await page.locator("#field-r").click();
      await page.locator("dialog input").fill("42");
      await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 42);
      await page.waitForFunction(lines => /radius = \(42/.test(lines.innerText.replaceAll("\u00a0", " ")), sourceLines);
      await page.locator(`dialog [data-action=${finish}]`).click();
      await page.locator("dialog").waitFor({ state: "detached" });
      if (finish === "ok") await page.locator("#undo-edit").click();
      await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 24);
      await page.waitForFunction(({ lines, expected }) => lines.innerText === expected, { lines: sourceLines, expected: original });
    }
    // Typed source applies live; invalid source is marked and not applied; a
    // drawing edit replaces the invalid buffer; Undo steps back through both.
    const radius = () => page.evaluate(() => Number(document.querySelector("#stage svg circle").getAttribute("r")));
    // Double-click the number token and type over it, as a person would.
    const retype = async (word, text) => {
      await page.locator("#source-editor .view-line span span", { hasText: new RegExp(`^${word}$`) }).first().dblclick();
      await page.keyboard.type(text);
    };
    await retype("24", "30");
    await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 30);
    await page.locator("#source-status", { hasText: "Applied" }).waitFor();
    await retype("30", "(");
    await page.locator("#source-status", { hasText: /Error on line \d+/ }).waitFor();
    await page.locator("#source-editor .squiggly-error").first().waitFor();
    assert.equal(await radius(), 30);
    assert.match(await page.locator("#file-status").textContent(), /Up to date/);
    await page.locator("#field-r").click();
    await page.locator("dialog input").fill("35");
    await page.locator("dialog [data-action=ok]").click();
    await page.locator("dialog").waitFor({ state: "detached" });
    await page.waitForFunction(lines => /radius = \(35/.test(lines.innerText.replaceAll(" ", " ")), sourceLines);
    await page.locator("#source-editor .squiggly-error").waitFor({ state: "detached" });
    await page.locator("#undo-edit").click();
    await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 30);
    await page.locator("#undo-edit").click();
    await page.waitForFunction(() => Number(document.querySelector("#stage svg circle").getAttribute("r")) === 24);
    await page.waitForFunction(({ lines, expected }) => lines.innerText === expected, { lines: sourceLines, expected: original });
    await page.locator("#stage svg").first().click({position: { x: 4, y: 4 }});
    await page.locator("#add-circle").click();
    await page.waitForFunction(() => shapeTool?.ready);
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
    assert.equal(await stories.count(), 64);
    await stories.filter({ hasText: "unreachable case" }).click();
    await page.getByText("⚠ Unreachable arm").waitFor();
    assert.deepEqual(errors, []);
    console.log("Static drawing, live Monaco, preview/cancel/accept/undo, mobile and storybook checks passed.");
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; }).finally(() => server.close());
