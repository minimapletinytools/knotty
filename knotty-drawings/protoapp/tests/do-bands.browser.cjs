// Run against a release build with Playwright via NODE_PATH.
// A do block draws one band per statement; nodes stay in their band, bands grow
// to fit, discarded results end in a drop mark and unused binds are marked.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const { spawn } = require('node:child_process');
const { chromium } = require('playwright');
const app = path.resolve(__dirname, '..'), library = path.dirname(app);
const temp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'do-bands-')));
const file = path.join(temp, 'DoBands.knot');
const original = `module DoBands exposing (..)
import KnottyDrawings exposing (..)

steps :: Maybe Int -> Maybe Int
steps mx = do
    mx
    unused <- mx
    x <- mx
    let y = x + 1
    pure (x * y)

program = svg 100 100 []
`;
fs.writeFileSync(file, original);
const server = require('./reap.cjs')(spawn(path.join(app, 'target/release/knottydrawings'),
  [file, '--library', library, '--renderer', 'direct', '--no-open', '--port', '0']));

(async () => {
  const url = await new Promise((resolve, reject) => {
    let output = '';
    server.stdout.on('data', chunk => {
      const match = (output += chunk).match(/at (http:\/\/[^\s]+)/);
      if (match) resolve(match[1]);
    });
    server.once('exit', code => reject(Error('Server exited ' + code)));
  });
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1400, height: 1000 } });
    const open = async () => {
      await page.goto(url + '/tangle?definition=steps', { waitUntil: 'domcontentloaded' });
      await page.locator('.do-band').first().waitFor();
      await page.locator('#fit').click(); // actual size: pointer moves map 1:1
      await page.waitForFunction(() => document.querySelector('#fit').textContent === 'Fit graph');
    };
    await open();
    const bands = page.locator('.do-band');
    assert.deepEqual(await bands.locator('.band-op').allTextContents(), ['>>', '>>=', '>>=', 'let', '⇒']);
    assert.deepEqual(await bands.evaluateAll(b => b.map(e => e.className.replace('do-band ', ''))),
      ['discard', 'bind', 'bind', 'let', 'result']);
    assert.deepEqual(await bands.locator('.band-pattern').allTextContents(), ['unused', 'x']);
    // Disconnected cases: the discarded result ends in a drop mark; the unused bind is marked.
    await page.getByRole('img', { name: 'Result discarded (>>)' }).waitFor();
    const hubs = page.locator('.binding-knot');
    assert.deepEqual(await hubs.evaluateAll(h => h.map(e => e.classList.contains('unused-binding'))),
      [true, false, false]);

    const box = locator => locator.evaluate(e => { const r = e.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left }; });
    const plus = page.locator('.knot', { has: page.locator(':scope > .knot-label', { hasText: /^\+$/ }) });
    const drag = async (dx, dy) => {
      const r = await plus.locator(':scope > .knot-label').boundingBox();
      const x = r.x + r.width / 2, y = r.y + r.height / 2;
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + dx, y + dy, { steps: 6 });
      await page.mouse.up();
    };
    const letBand = bands.nth(3), resultBand = bands.nth(4);
    // Up and left: the node stops at its band's top and the gutter.
    await drag(-400, -400);
    let [node, band] = [await box(plus), await box(letBand)];
    assert(node.top >= band.top + 11 && node.top <= band.top + 14, JSON.stringify({ node, band }));
    assert(node.left >= band.left + 64 + 11, JSON.stringify({ node, band }));
    // Down: the band grows to keep the node, pushing the result band down.
    const resultBefore = await box(resultBand);
    await drag(0, 140);
    [node, band] = [await box(plus), await box(letBand)];
    const resultAfter = await box(resultBand);
    assert(node.top > band.top + 100 && node.bottom <= band.bottom, JSON.stringify({ node, band }));
    assert(resultAfter.top - resultBefore.top > 100, JSON.stringify({ resultBefore, resultAfter }));
    // Saved positions are relative to the band and survive a reload.
    const offset = node.top - band.top;
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#status').textContent === 'Saved.');
    const saved = fs.readFileSync(file, 'utf8');
    const [, y] = saved.match(/@position\(\s*[\d.]+,\s*([\d.]+)\)\s*\(?x \+ 1/).map(Number);
    assert(Math.abs(y - offset) < 2, `${y} vs ${offset}`);
    await open();
    [node, band] = [await box(plus), await box(letBand)];
    assert(Math.abs(node.top - band.top - offset) < 2);
    console.log('Do bands passed: statement bands, drop mark, unused binds, band-bound dragging, growth and saved band positions.');
  } finally {
    await browser.close();
    server.kill();
    fs.rmSync(temp, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); server.kill(); process.exit(1); });
