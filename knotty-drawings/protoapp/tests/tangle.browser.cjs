// Run against a release build with Playwright available via NODE_PATH.
const assert = require('node:assert/strict');
const fs = require('node:fs'),
  path = require('node:path'),
  os = require('node:os');
const { spawn } = require('node:child_process');
const { chromium } = require('playwright');
const app = path.resolve(__dirname, '..'),
  library = path.dirname(app);
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'tangle-roundtrip-'));
const processes = [];
async function open(source, directory, filename = 'MakersFair.knot') {
  const folder = path.join(temp, directory);
  fs.mkdirSync(folder);
  const file = path.join(folder, filename);
  fs.writeFileSync(file, source);
  const p = spawn(path.join(app, 'target/release/knottydrawings'), [
    file,
    '--library',
    library,
    '--renderer',
    'direct',
    '--no-open',
    '--port',
    '0',
  ]);
  processes.push(p);
  const url = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error('Startup timeout')), 30000);
    let output = '';
    p.stdout.on('data', (chunk) => {
      output += chunk;
      const match = output.match(/at (http:\/\/[^\s]+)/);
      if (match) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    });
    p.once('error', reject);
    p.once('exit', (code) => {
      clearTimeout(timer);
      reject(Error('Server exited ' + code));
    });
  });
  return { url, file };
}

async function controlFlow(page) {
  const source = fs.readFileSync(path.join(library, 'examples/tangle-control-flow.knot'), 'utf8');
  const { file, url } = await open(source, 'control-flow', 'ControlFlow.knot');
  const route = name => '/tangle?definition=' + name;
  async function projected(name) {
    const response = await page.request.get(url + '/api' + route(name));
    const result = await response.json();
    assert(result.ok, result.reason);
    return result.graph;
  }
  async function show(name) {
    await page.goto(url + route(name));
    await page.waitForFunction(() => document.querySelector('#preview').naturalWidth > 0);
  }
  async function attributes(selector, key) {
    return page.locator('#preview').evaluate(async (img, [selector, key]) => {
      const doc = new DOMParser().parseFromString(await (await fetch(img.src)).text(), 'image/svg+xml');
      return Array.from(doc.querySelectorAll(selector), shape => Number(shape.getAttribute(key)));
    }, [selector, key]);
  }
  async function saveAndCheck(before, after) {
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#status').textContent === 'Saved.');
    assert.equal(fs.readFileSync(file, 'utf8'), source.replace(before, after));
  }
  async function undo() {
    await page.getByRole('button', { name: 'Undo', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#status').textContent === 'Edit undone.');
    assert.equal(fs.readFileSync(file, 'utf8'), source);
  }

  async function checkOutputDots() {
    // Every expression or scope has a single result shared by its inner and outer ropes.
    for (const box of await page.locator('.knot, .scope-box').all()) {
      const dot = box.locator(':scope > .output-dot, :scope > .scope-body > .output-dot, :scope > .knot-body > .output-dot');
      assert.equal(await dot.count(), 1);
      assert.equal(await dot.textContent(), '');
      const node = await box.boundingBox(), port = await dot.boundingBox();
      assert(Math.abs(port.x + port.width / 2 - node.x - node.width) < 2);
    }
  }
  async function checkHover(mark, type, expression, ropes) {
    await mark.hover();
    const tip = page.getByRole('tooltip');
    assert.match(await tip.locator('strong').innerText(), type);
    assert.match(await tip.locator('code').innerText(), expression);
    assert.equal(await page.locator('path.inspected-wire').count(), ropes);
    await page.locator('h1').hover();
    assert.equal(await tip.count(), 0);
    assert.equal(await page.locator('path.inspected-wire').count(), 0);
  }
  async function checkConditionalPair(kind, type, expression, ropes) {
    const header = page.locator('.alternatives.' + kind + ' > .scope-header');
    const [outer, inner] = await header.locator('.condition-button').all();
    const label = await header.locator('.condition-keyword strong').boundingBox();
    const left = await outer.boundingBox(), right = await inner.boundingBox();
    assert(left.x + left.width < label.x && right.x > label.x + label.width);
    for (const bounds of [left, right]) {
      assert(Math.abs(bounds.y + bounds.height / 2 - label.y - label.height / 2) < 1);
    }
    for (const mark of [outer, inner]) {
      await checkHover(mark, type, expression, ropes);
      await mark.evaluate(node => (node.closest('button') || node).focus());
      assert.equal(await header.locator('.condition-button.inspected').count(), 2);
      await page.keyboard.press('Escape');
      assert.equal(await header.locator('.condition-button.inspected').count(), 0);
    }
    if (kind === 'case') {
      assert(await page.locator('#wires .rope-pattern').evaluateAll((paths, bounds) => paths.every(path => {
        const start = path.getPointAtLength(0).matrixTransform(path.getScreenCTM());
        return Math.hypot(start.x - bounds.x - bounds.width / 2, start.y - bounds.y - bounds.height / 2) < 1;
      }), right));
    }
  }
  await show('dot_grid');
  await checkOutputDots();
  assert.equal(await page.locator('.monadic-context .binding-knot').count(), 3);
  assert.equal(await page.locator('#wires .rope-sequence').count(), 0);
  assert.equal(await page.locator('#wires .rope-bind').count(), 2);
  assert.equal(await page.locator('#wires .rope-bind-gap[marker-end]').count(), 2);
  assert.equal(await page.locator('#wires .rope-value').count(), 1);
  await checkHover(page.locator('.binding-knot .left .nublet').first(), /List/, /\[24, 60, 96\]/, 1);
  const pure = page.locator('article.call').filter({ has: page.getByText('pure', { exact: true }) });
  assert.equal(await pure.locator('.input-rail .rail-end').textContent(), '');
  assert.deepEqual(await attributes('circle', 'cx'), [24, 24, 60, 60, 96, 96]);
  await page.getByRole('textbox', { name: '[Float], input 1, number', exact: true }).first().fill('32');
  await saveAndCheck('x <- [24', 'x <- [32');
  assert.deepEqual(await attributes('circle', 'cx'), [32, 32, 60, 60, 96, 96]);
  await undo();

  await page.getByRole('textbox', { name: '8, input 1, number', exact: true }).fill('10');
  await saveAndCheck('let radius = 8', 'let radius = 10');
  assert.deepEqual(await attributes('circle', 'r'), [10, 10, 10, 10, 10, 10]);
  await undo();

  await show('signal');
  await checkOutputDots();
  assert.equal(await page.locator('.alternatives.if .branch-arm').count(), 2);
  const conditional = page.locator('.alternatives.if');
  assert.equal(await conditional.locator(':scope > .scope-header .condition-gate .nublet').count(), 1);
  assert.equal(await page.locator('#wires .rope-pattern').count(), 0);
  assert.deepEqual(await conditional.locator('.capture:visible').allTextContents(), ['', '', '']);
  await checkHover(conditional.locator(':scope > .output-dot'), /Node/, /if level > 50/, 3);
  await checkConditionalPair('if', /Bool/, /level > 50/, 1);
  await checkHover(conditional.locator(':scope > .scope-header .capture-triangle'), /Float/, /^level$/, 3);
  const crossings = conditional.locator(':scope > .capture-ropes path');
  assert.equal(await crossings.count(), 2);
  assert(await crossings.evaluateAll(paths => paths.every(p =>
    /^M [^A-Z]+ C [^A-Z]+$/.test(p.getAttribute('d')) && getComputedStyle(p).strokeDasharray !== 'none')));
  const gate = await conditional.locator('.condition-gate .nublet').boundingBox();
  assert(await page.locator('#wires path').evaluateAll((paths, gate) => paths.some(p => {
    const end = p.getPointAtLength(p.getTotalLength()).matrixTransform(p.getScreenCTM());
    return Math.hypot(end.x - gate.x - gate.width / 2, end.y - gate.y - gate.height / 2) < 1 &&
      /^M [^A-Z]+ C [^A-Z]+$/.test(p.getAttribute('d')) && getComputedStyle(p).strokeDasharray === 'none';
  }), gate));
  // The rope to else crosses then geometrically, but then paints over it.
  assert(await crossings.last().evaluate(path => {
    const midpoint = path.getPointAtLength(path.getTotalLength() / 2).matrixTransform(path.getScreenCTM());
    // Hit-test the full curve even when the midpoint falls between dots.
    path.style.strokeDasharray = 'none';
    path.style.pointerEvents = 'stroke';
    const stack = document.elementsFromPoint(midpoint.x, midpoint.y);
    path.style.removeProperty('pointer-events');
    path.style.removeProperty('stroke-dasharray');
    const arm = stack.findIndex(el => el.classList.contains('branch-arm'));
    return arm >= 0 && arm < stack.indexOf(path);
  }));
  assert.deepEqual(await attributes('circle', 'r'), [7, 14]);
  await page.getByRole('textbox', { name: '>, input 2, number', exact: true }).fill('20');
  await conditional.locator('.condition-gate .nublet').focus();
  assert.equal(await page.getByRole('tooltip').locator('code').innerText(), 'level > 20');
  await page.keyboard.press('Escape');
  assert.equal(await page.getByRole('tooltip').count(), 0);
  await saveAndCheck('level > 50', 'level > 20');
  assert.deepEqual(await attributes('circle', 'r'), [14, 14]);
  await undo();

  await show('stamp');
  await checkOutputDots();
  assert.equal(await page.locator('#wires .rope-pattern').count(), 2);
  assert.equal(await page.locator('#wires .rope-merge').count(), 2);
  await checkConditionalPair('case', /Stamp/, /^shape$/, 3);
  const disc = page.getByRole('region', { name: 'Disc radius scope' });
  const tile = page.getByRole('region', { name: 'Tile width height scope' });
  await checkHover(disc.locator('.inputs-boundary .left > .nublet'), /Stamp/, /Disc radius/, 1);
  await disc.getByRole('button', { name: 'Connect parameter radius' }).click();
  await tile.getByRole('button', { name: 'rect, input 3, connected to width' }).click();
  assert.match(await page.locator('#status').textContent(), /outside/);
  const { graph } = await (await page.request.get(url + '/api' + route('stamp'))).json();
  const [discArm, tileArm] = graph.scope.body.arms;
  const radius = discArm.inputs[0].parameters[0].port;
  const width = tileArm.body.inputs[0].inputs[2];
  const refused = await (await page.request.post(url + '/api/tangle', { data: {
    snapshot: graph.snapshot, definition: 'stamp',
    changes: [{ kind: 'connect', parameter: radius, target: width.id }],
  } })).json();
  assert.equal(refused.ok, false);
  await tile.getByRole('textbox', { name: 'rect, input 2, number' }).fill('30');
  await saveAndCheck('rect x 26', 'rect x 30');
  assert.deepEqual(await attributes('rect', 'y'), [30]);
  assert.deepEqual(await attributes('circle', 'r'), [12]);
  await undo();

  const imports = source.replace('import Types exposing (..)', 'import Types exposing (..)\nimport Map as M exposing (member)');
  fs.writeFileSync(file, imports + '\ninfix_names key table = ((key) {- ignored `name` -} `M.member` table, key `member` table)\n');
  fs.appendFileSync(file, String.raw`
nested_warning :: Maybe (Maybe Int) -> Int
nested_warning arg = case arg of
    Just inner -> case inner of
        Just n -> n
        Just 1 -> 1
    Nothing -> 0

captured :: Maybe (Float -> Float)
captured = do
    let wrapped = Just 4
    n <- wrapped
    pure (\delta -> delta + n)

grouped :: Float -> Float -> Float
grouped x y = (({- use x here -} x))

@label("sample")
grouped_number :: Int
grouped_number = @whip("first") @whip("second") (@label("inner") (({- 🧶 -} 0x10 {- keep -})))

local_fn :: Maybe Float
local_fn = do
    let addOne v = v + 1
    pure (addOne 5)
`);
  await page.goto(url + route('captured'));
  const lambda = page.getByRole('region', { name: 'λ scope' });
  await lambda.waitFor();
  assert.equal(await page.locator('.monadic-context').count(), 1);
  assert.equal(await page.locator('.binding-knot').count(), 2);
  assert.equal(await page.locator('#wires .rope-bind-gap[marker-end]').count(), 1);
  assert.equal(await lambda.locator('.capture:visible').getAttribute('aria-label'), 'Captured n');
  const captured = await projected('captured');
  const [ordinary, bind] = captured.scope.body.steps;
  assert.equal(ordinary.kind, 'let');
  assert.equal(bind.value.parameter, ordinary.pattern.parameters[0].port);
  assert(!bind.value.visible.includes(bind.pattern.parameters[0].port));
  const infix = await projected('infix_names');
  assert.deepEqual(infix.scope.body.inputs.map(input => input.label), ['M.member', 'member']);
  const { scope: { body: nested } } = await projected('nested_warning');
  assert.equal(nested.incomplete, undefined);
  assert(nested.arms.every(arm => !arm.unreachable));
  const inner = nested.arms[0].body;
  assert.equal(inner.incomplete, true);
  assert.deepEqual(inner.arms.map(arm => Boolean(arm.unreachable)), [false, true]);
  const grouped = await projected('grouped');
  const replaced = await (await page.request.post(url + '/api/tangle', { data: {
    definition: 'grouped', snapshot: grouped.snapshot,
    changes: [{ kind: 'connect', target: grouped.scope.body.id, parameter: grouped.scope.inputs[1].parameters[0].port }],
  } })).json();
  assert.equal(replaced.ok, true, replaced.reason);
  assert(fs.readFileSync(file, 'utf8').includes('grouped x y = (({- use x here -} y))'));
  await page.goto(url + route('grouped_number'));
  await page.locator('#board > .scope-box').waitFor();
  const beforeNumber = fs.readFileSync(file, 'utf8');
  const numberGraph = await projected('grouped_number');
  const layers = numberGraph.scope.body.annotations;
  assert.deepEqual(numberGraph.annotations[0].entries.map(entry => [entry.key, entry.value]), [['label', '"sample"']]);
  assert.deepEqual(layers.map(layer => layer.entries.map(entry => [entry.key, entry.value])), [
    [['whip', '"first"'], ['whip', '"second"']], [['label', '"inner"']],
  ]);
  assert(layers[0].owner.start < layers[1].owner.start);
  for (const { entries } of [...numberGraph.annotations, ...layers]) {
    for (const { value, span } of entries) {
      assert.equal(Buffer.from(beforeNumber).subarray(span.start, span.end).toString(), value);
    }
  }
  const number = page.locator('input.literal');
  assert.equal(await number.inputValue(), '0x10');
  await number.fill('31');
  await page.locator('.knot.number .output-dot').focus();
  assert.match(await page.getByRole('tooltip').locator('code').innerText(), /\(\(\{- 🧶 -\} 31 \{- keep -\}\)\)/);
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#status').textContent === 'Saved.');
  assert.equal(fs.readFileSync(file, 'utf8'), beforeNumber.replace('🧶 -} 0x10', '🧶 -} 31'));
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#status').textContent === 'Edit undone.');
  assert.equal(fs.readFileSync(file, 'utf8'), beforeNumber);
  const local = await projected('local_fn');
  const position = { kind: 'position', target: local.scope.body.steps[0].value.id, x: 24, y: 48 };
  const edited = await (await page.request.post(url + '/api/tangle', { data: {
    definition: 'local_fn', snapshot: local.snapshot, changes: [position],
  } })).json();
  assert.equal(edited.ok, true, edited.reason);
  assert(fs.readFileSync(file, 'utf8').includes('let @position(24, 48) addOne v = v + 1'));


}

(async () => {
  const original = fs.readFileSync(path.join(library, 'examples/makers-fair.knot'), 'utf8');
  const browser = await chromium.launch({ headless: true });
  try {
    const { url, file } = await open(original, 'edited');
    const page = await browser.newPage({ viewport: { width: 1440, height: 980 } });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const api = async (server, route) => (await page.request.get(server + route)).json();
    const graph = async (server) => (await api(server, '/api/tangle')).graph;
    const post = async (body) =>
      (await page.request.post(url + '/api/tangle', { data: body })).json();
    async function loaded() {
      await page.waitForFunction(() => document.querySelector('#preview').naturalWidth > 0);
    }
    async function checkTable(cx, radius) {
      const circles = await page.locator('#preview').evaluate(async image => {
        const svg = await (await fetch(image.src)).text();
        const doc = new DOMParser().parseFromString(svg, 'image/svg+xml');
        return Array.from(doc.querySelectorAll('circle'), c =>
          ['cx', 'cy', 'r'].map(a => Number(c.getAttribute(a))));
      });
      assert.deepEqual(circles, [
        [cx, 60, radius], [50, 60, 7], [110, 60, 7], [80, 30, 7], [80, 90, 7],
      ]);
    }
    async function save() {
      await page.getByRole('button', { name: 'Save', exact: true }).click();
      await page.waitForFunction(() => document.querySelector('#status').textContent === 'Saved.');
    }
    async function compare(source, label) {
      assert.equal(fs.readFileSync(file, 'utf8'), source);
      const oracle = await open(source, label);
      const expected = await api(oracle.url, '/api/scene'),
        actual = await api(url, '/api/scene');
      assert.equal(actual.error, null);
      assert.deepEqual(actual.scene, expected.scene);
      assert.equal(actual.svg, expected.svg);
      const a = await graph(url),
        b = await graph(oracle.url);
      assert.deepEqual({ ...a, snapshot: null }, { ...b, snapshot: null });
      assert.equal(await page.locator('#source').textContent(), a.source);
    }
    await page.goto(url);
    await page.getByRole('button', { name: 'Tangle', exact: true }).click();
    const embedded = page.frameLocator('#drawing-graph');
    const picker = embedded.getByRole('combobox', { name: 'Drawing definition' });
    await embedded.getByRole('region', { name: 'program scope', exact: true }).waitFor();
    await picker.selectOption('cafe_table');
    await embedded.getByRole('region', { name: 'cafe_table scope', exact: true }).waitFor();
    await page.getByRole('link', { name: 'Full editor' }).click();
    await loaded();
    await checkTable(80, 22);
    const first = await graph(url);
    assert.deepEqual(first.scope.inputs.map(input => input.pattern), ['(x, y)']);
    const table = page.getByRole('region', { name: 'cafe_table scope' });
    const chain = table.locator('.slub-chain').first();
    assert.deepEqual(await chain.locator('.slub-label').allTextContents(),
      ['set_fill', 'set_stroke', 'set_stroke_width']);
    const segments = await chain.locator('.slub').evaluateAll(pills =>
      pills.map(pill => {
        const { x, y, right } = pill.getBoundingClientRect();
        return { x, y, right };
      }));
    for (let i = 1; i < segments.length; i++) {
      assert.equal(segments[i].y, segments[i - 1].y);
      assert(Math.abs(segments[i].x - segments[i - 1].right) < 1);
    }
    const circle = table.locator('article.call').filter({ hasText: 'circle' }).first();
    assert(await circle.evaluate(knot => {
      const rect = knot.getBoundingClientRect();
      const at = sel => {
        const r = knot.querySelector(sel).getBoundingClientRect();
        return r.x + r.width / 2;
      };
      return Math.abs(at('.input-rail .socket .nublet') - rect.left) < 2 &&
        Math.abs(at('.output-dot') - rect.right) < 2;
    }));
    const bounds = async (node) => node.boundingBox();
    const chainBox = await bounds(chain), circleBox = await bounds(circle);
    const listBox = await bounds(table.locator('article.list'));
    assert(circleBox.x + circleBox.width <= chainBox.x && chainBox.x + chainBox.width <= listBox.x);
    assert(Math.abs(circleBox.y + circleBox.height / 2 - chainBox.y - chainBox.height / 2) < 2);
    assert(Math.abs(listBox.y + listBox.height / 2 - chainBox.y - chainBox.height / 2) < 2);
    const stamp = fs.statSync(file).mtimeMs;
    await save();
    assert.equal(fs.statSync(file).mtimeMs, stamp);
    assert.equal((await graph(url)).snapshot, first.snapshot);
    const radius = page
      .getByRole('textbox', { name: 'circle, input 3, number', exact: true })
      .first();
    assert.equal(await radius.inputValue(), '22');
    await radius.fill('28');
    assert.equal(fs.readFileSync(file, 'utf8'), original);
    await save();
    const resized = original.replace('circle x y 22', 'circle x y 28');
    await compare(resized, 'radius');
    await checkTable(80, 28);
    const stale = await post({ snapshot: first.snapshot, definition: 'cafe_table', changes: [] });
    assert.equal(stale.ok, false);
    await page.reload();
    await loaded();
    assert.equal(await radius.inputValue(), '28');
    await page
      .getByRole('button', { name: 'Connect parameter y', exact: true })
      .dragTo(
        page.getByRole('button', { name: 'circle, input 1, connected to x', exact: true }).first(),
      );
    await save();
    await compare(resized.replace('circle x y 28', 'circle y y 28'), 'rewire');
    await checkTable(60, 28);
    for (const expected of [resized, original]) {
      await page.reload();
      await loaded();
      await page.getByRole('button', { name: 'Undo', exact: true }).click();
      await page.waitForFunction(
        () => document.querySelector('#status').textContent === 'Edit undone.',
      );
      assert.equal(fs.readFileSync(file, 'utf8'), expected);
      await checkTable(80, expected === original ? 22 : 28);
    }
    async function drag(handle, dx, dy) {
      const rect = await bounds(handle);
      const x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + dx, y + dy, { steps: 6 });
      await page.mouse.up();
    }
    const moved = async (node) => node.evaluate(el => [parseFloat(el.style.left), parseFloat(el.style.top)]);
    await radius.fill('25');
    await drag(circle.locator('.knot-label'), 37, 24);
    await drag(chain.locator('.slub-label').first(), -10, 32);
    await drag(table.getByRole('button', { name: 'Collapse cafe_table box' }), 22, 7);
    assert.equal(await page.locator('#status').textContent(), '4 unsaved edits');
    assert.equal(fs.readFileSync(file, 'utf8'), original);
    const circleAt = await moved(circle), chainAt = await moved(chain), tableAt = await moved(table);
    await save();
    const pinnedSource = fs.readFileSync(file, 'utf8');
    assert.equal((pinnedSource.match(/@position\(/g) || []).length, 3);
    assert(pinnedSource.includes('circle x y 25'));
    await checkTable(80, 25);
    await page.reload();
    await loaded();
    assert.deepEqual(await moved(circle), circleAt);
    assert.deepEqual(await moved(chain), chainAt);
    assert.deepEqual(await moved(table), tableAt);
    const positioned = await graph(url);
    assert.deepEqual(positioned.position, tableAt);
    await drag(circle.locator('.knot-label'), 18, 9);
    await save();
    assert.equal((fs.readFileSync(file, 'utf8').match(/@position\(/g) || []).length, 3);
    async function undoTo(expected) {
      await Promise.all([
        page.waitForResponse(response => response.url() === url + '/api/undo'),
        page.getByRole('button', { name: 'Undo', exact: true }).click(),
      ]);
      await page.waitForFunction(() => !document.querySelector('#board').inert &&
        document.querySelector('#status').textContent === 'Edit undone.');
      assert.equal(fs.readFileSync(file, 'utf8'), expected);
    }
    await undoTo(pinnedSource);
    await undoTo(original);

    const authored = original.replace('cafe_table ::',
      '@position(1, 2)\n@{ label = "Café table", position = (44.5, 19) }\ncafe_table ::');
    fs.writeFileSync(file, authored);
    await page.reload();
    await loaded();
    assert.deepEqual((await graph(url)).position, [44.5, 19]);
    await drag(table.getByRole('button', { name: 'Collapse cafe_table box' }), 20, 12);
    await save();
    const editedPair = (await graph(url)).position.join(', ');
    assert.equal(fs.readFileSync(file, 'utf8'), authored.replace('44.5, 19', editedPair));
    await undoTo(authored);
    const shadow = original
      .replace(
        'cafe_table ::',
        'y = 999\n\n@whip(IntSlider "Seats" 1 8)\nseatCount :: Int\nseatCount = 4\n\ncafe_table ::',
      )
      .replace('circle (x - 30) y 7', 'circle (let y = 41 in y) y 7');
    const incompatible = '\nchoose :: Bool -> Float -> Node\nchoose flag x = circle x x 10\n';
    fs.writeFileSync(file, shadow + incompatible);
    const choice = (await api(url, '/api/tangle?definition=choose')).graph;
    const rejected = await post({
      snapshot: choice.snapshot,
      definition: 'choose',
      changes: [{ kind: 'connect', target: choice.scope.body.inputs[0].id, parameter: choice.scope.inputs[0].parameters[0].port }],
    });
    assert.equal(rejected.ok, false);
    assert.equal(fs.readFileSync(file, 'utf8'), shadow + incompatible);
    fs.writeFileSync(file, shadow);
    await page.reload();
    await loaded();
    const g = await graph(url);
    const flat = (e) => [e, ...e.inputs.flatMap(flat)];
    const nodes = flat(g.scope.body);
    const localLet = nodes.find(e => e.kind === 'let');
    assert.equal(localLet.steps[0].pattern.pattern, 'y');
    const outerYPort = g.scope.inputs.flatMap(input => input.parameters).find(p => p.name === 'y').port;
    assert(!localLet.inputs[0].visible.includes(outerYPort));
    const input = nodes.find((e) => e.kind === 'reference' && e.label === 'x');
    const literal = nodes.find((e) => e.kind === 'number' && e.label === '22');
    const edit = (changes) => post({ snapshot: g.snapshot, definition: g.definition, changes });
    for (const change of [
      { kind: 'connect', target: localLet.inputs[0].id, parameter: outerYPort },
      { kind: 'connect', target: input.id, parameter: 99 },
      { kind: 'number', target: literal.id, value: '28 + 1' },
    ])
      assert.equal((await edit([change])).ok, false);
    assert.equal(fs.readFileSync(file, 'utf8'), shadow);
    assert.equal((await edit([{ kind: 'connect', target: input.id, parameter: outerYPort }])).ok, true);
    await page.reload();
    await loaded();
    await compare(shadow.replace('circle x y 22', 'circle y y 22'), 'shadow');
    // The callback shadows x, captures y, and may also acquire a fresh capture z.
    const fixture = String.raw`
nested_box :: Float -> Float -> Float -> List Float
nested_box x y z = map (\x -> x + y) [x]
`;
    const before = fs.readFileSync(file, 'utf8') + fixture;
    fs.writeFileSync(file, before);
    await page.goto(url + '/tangle?definition=nested_box');
    const inner = page.getByRole('region', { name: 'λ scope' });
    await inner.waitFor();
    const model = (await api(url, '/api/tangle?definition=nested_box')).graph;
    const [outerX, outerY, outerZ] = model.scope.inputs.map(group => group.parameters[0].port);
    const [lambda, list] = model.scope.body.inputs;
    const innerX = lambda.scope.inputs[0].parameters[0].port;
    const [left, right] = lambda.scope.body.inputs;
    assert.equal(left.parameter, innerX);
    assert.equal(right.parameter, outerY);
    assert.deepEqual(new Set(left.visible), new Set([innerX, outerY, outerZ, model.definition_port]));
    for (const [target, parameter] of [[left.id, outerX], [list.inputs[0].id, innerX]]) {
      const refused = await post({ snapshot: model.snapshot, definition: model.definition,
        changes: [{ kind: 'connect', target, parameter }] });
      assert.equal(refused.ok, false);
    }
    const captured = inner.locator(':scope > .scope-header .capture:visible');
    assert.equal(await captured.getAttribute('aria-label'), 'Captured y');
    const captureBox = await bounds(captured.locator('.capture-triangle'));
    const sourceBox = await bounds(page.getByRole('button', { name: 'Connect parameter y' }));
    assert(sourceBox.y + sourceBox.height < captureBox.y);
    const map = page.locator('article.call').filter({ has: page.getByText('map', { exact: true }) });
    const aboveConsumer = async () => {
      const box = await bounds(inner), consumer = await bounds(map);
      const diamond = await bounds(inner.locator(':scope > .definition-port'));
      assert(box.y + box.height < consumer.y);
      assert(Math.abs(diamond.x + diamond.width / 2 - consumer.x) < 24);
    };
    await aboveConsumer();
    assert.equal(await inner.locator(':scope > .definition-port').count(), 1);
    await page.getByRole('button', { name: 'Connect parameter z' }).click();
    await inner.getByRole('button', { name: '+, input 2, connected to y' }).click();
    assert.equal(await captured.getAttribute('aria-label'), 'Captured z');
    await inner.getByRole('button', { name: 'Collapse λ box' }).click();
    assert.equal(await inner.locator(':scope > .scope-body > .scope-contents').isVisible(), false);
    assert.equal(await captured.getAttribute('aria-label'), 'Captured z');
    await aboveConsumer();
    assert.equal(fs.readFileSync(file, 'utf8'), before);
    await inner.getByRole('button', { name: 'Expand λ box' }).click();
    await inner.getByRole('button', { name: '+, input 2, connected to z' }).waitFor();
    await save();
    assert.equal(fs.readFileSync(file, 'utf8'), before.replace('x + y) [x]', 'x + z) [x]'));
    await page.reload();
    await inner.waitFor();
    assert.equal(await captured.getAttribute('aria-label'), 'Captured z');
    await page.getByRole('button', { name: 'Undo', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#status').textContent === 'Edit undone.');
    assert.equal(fs.readFileSync(file, 'utf8'), before);
    const innerControl = inner.getByRole('button', { name: 'Collapse λ box' });
    const plus = inner.locator('article.operator');
    const plusAt = await moved(plus);
    await page.setViewportSize({ width: 980, height: 470 });
    await page.waitForFunction(() => {
      const board = document.querySelector('#board');
      return board.getBoundingClientRect().width < board.offsetWidth * 0.98;
    });
    const outer = page.getByRole('region', { name: 'nested_box scope' });
    const grip = plus.locator('.knot-label');
    const innerBefore = await bounds(inner), outerBefore = await bounds(outer);
    const start = await bounds(grip);
    const x = start.x + start.width / 2, y = start.y + start.height / 2;
    const checkGrip = async (dx, dy) => {
      const at = await bounds(grip);
      assert(Math.abs(at.x - start.x - dx) < 2 && Math.abs(at.y - start.y - dy) < 2);
    };
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 185, y + 220, { steps: 5 });
    assert((await bounds(inner)).width > innerBefore.width + 100);
    assert((await bounds(outer)).height > outerBefore.height + 50);
    await checkGrip(185, 220);
    await page.mouse.move(x - 70, y - 65, { steps: 5 });
    assert((await bounds(inner)).x < innerBefore.x - 40);
    assert((await bounds(inner)).y < innerBefore.y - 30);
    await checkGrip(-70, -65);
    assert.equal(fs.readFileSync(file, 'utf8'), before);
    // Losing capture cancels the preview and restores the original extents.
    await grip.evaluate(el => el.releasePointerCapture(1));
    await page.mouse.up();
    assert.deepEqual(await bounds(inner), innerBefore);
    assert.deepEqual(await bounds(outer), outerBefore);
    await checkGrip(0, 0);
    await drag(innerControl, 48, 16);
    await save();
    assert.deepEqual(await moved(plus), plusAt);
    const after = (await api(url, '/api/tangle?definition=nested_box')).graph;
    assert.deepEqual(after.scope.body.inputs[0].position, await moved(inner));
    await undoTo(before);

    const computed = before.replace('x + y) [x]', '@position(x, 12) (x + y)) [x]');
    fs.writeFileSync(file, computed);
    await page.reload();
    await plus.waitFor();
    assert.equal(await plus.locator('.knot-label.move-handle').count(), 0);
    const readOnly = (await api(url, '/api/tangle?definition=nested_box')).graph;
    const sum = readOnly.scope.body.inputs[0].scope.body;
    const invalid = await post({ definition: readOnly.definition, snapshot: readOnly.snapshot,
      changes: [{ kind: 'position', target: sum.id, x: 30, y: 50 }] });
    assert.equal(invalid.ok, false);
    assert.equal(fs.readFileSync(file, 'utf8'), computed);
    fs.appendFileSync(file, String.raw`
compose_two :: (Float -> Float) -> (Float -> Float) -> Float -> Float
compose_two f g x = g (f x)

two_callbacks :: Float -> Float
two_callbacks x = compose_two (\a -> a + 1) (\b -> b + 2) x
`);
    await page.goto(url + '/tangle?definition=two_callbacks');
    const callbacks = page.getByRole('region', { name: 'λ scope' });
    await callbacks.first().waitFor();
    const firstBox = await bounds(callbacks.nth(0)), secondBox = await bounds(callbacks.nth(1));
    const consumer = await bounds(page.locator('article.call').filter({ hasText: 'compose_two' }));
    assert(firstBox.x + firstBox.width < secondBox.x);
    for (const box of [firstBox, secondBox]) assert(box.y + box.height < consumer.y);
    const callbacksSource = fs.readFileSync(file, 'utf8');
    await page.goto(url + '/tangle?definition=compose_two');
    await page.getByRole('button', { name: 'Connect parameter g', exact: true }).click();
    await page.getByRole('button', { name: 'Callsite function, connected to f', exact: true }).click();
    await save();
    assert.equal(fs.readFileSync(file, 'utf8'), callbacksSource.replace('g (f x)', 'g (g x)'));
    await undoTo(callbacksSource);
    await controlFlow(page);
    await page.goto(url + '/tangle/stories');
    const navigation = page.getByRole('navigation', { name: 'Syntax stories' });
    await navigation.locator('a').first().waitFor();
    const names = await navigation.locator('a').allTextContents();
    const catalog = (await api(url, '/api/tangle/stories')).stories;
    const graphs = new Map(catalog.map(({ graph }) => [graph.definition, graph]));
    assert(names.includes('nested captures'));
    for (const name of ['inferred swap', 'fixed length pattern', 'grouped numbers', 'qualified infix',
      'incomplete case', 'unreachable case']) assert(names.includes(name));
    for (const name of names) {
      await navigation.getByRole('link', { name, exact: true }).click();
      const definition = name.replaceAll(' ', '_');
      await page.waitForFunction(name => document.querySelector('#definition').textContent === name, definition);
      assert.equal(await navigation.locator('[aria-current="page"]').innerText(), name);
      assert((await page.locator('#source').textContent()).includes(definition));
      assert(await page.locator('#board > .scope-box').isVisible());
      assert.equal(await page.locator('.knot.source').count(), 0);
      assert(await page.locator('#save').isHidden());
      if (definition === 'inferred_swap') {
        const graph = graphs.get(definition);
        assert.equal(graph.callable.at_site, 'a -> b -> (b, a)');
        assert.equal(await page.locator('.tuple > .knot-label').innerText(), '(b, a)');
        for (const [name, ty] of [['x', 'a'], ['y', 'b']]) {
          await page.getByRole('button', { name: 'Connect parameter ' + name }).focus();
          assert.equal(await page.getByRole('tooltip').locator('strong').innerText(), ty);
        }
      }
      if (definition === 'fixed_length_pattern') {
        const arm = page.getByRole('region', { name: '[a, b] scope', exact: true });
        assert.deepEqual(await arm.locator('.pattern-symbol').allTextContents(), [':', ':', '[]']);
      }
      if (definition === 'grouped_numbers') {
        assert.deepEqual(await page.locator('input.literal').evaluateAll(inputs => inputs.map(el => el.value)), ['0x10', '2.5']);
      }
      if (definition === 'qualified_infix') {
        const label = page.locator('.operator > .knot-label');
        assert.equal(await label.innerText(), 'M.member');
        await label.focus();
        assert.match(await page.getByRole('tooltip').innerText(), /Map.knot/);
      }
      if (definition === 'incomplete_case') {
        assert.equal(await page.locator('.alternatives > .scope-header .match-warning').innerText(), '⚠ Incomplete match');
        assert.equal(await page.locator('.branch-arm .match-warning').count(), 0);
      }
      if (definition === 'unreachable_case') {
        const arm = page.getByRole('region', { name: 'Just 1 scope', exact: true });
        await arm.getByRole('button', { name: 'Collapse Just 1 box' }).click();
        assert(await arm.getByText('⚠ Unreachable arm').isVisible());
        assert.equal(await page.locator('.match-warning').count(), 1);
      }
      if (definition === 'case_arms') assert.equal(await page.locator('.match-warning').count(), 0);
      if (definition === 'unary_minus_and_unit') {
        const call = page.locator('.call');
        assert.equal(await call.locator('.knot-label').innerText(), 'negate');
        await call.locator('.knot-label').focus();
        assert.match(await page.getByRole('tooltip').innerText(), /Num a => a -> a/);
        await call.locator('.socket').focus();
        assert.equal(await page.locator('path.inspected-wire').count(), 1);
        assert.equal(await page.locator('.tuple .inline-value').innerText(), '()');
      }
      if (definition === 'grouped_reference') {
        const mark = page.getByRole('button', { name: '+, input 2, connected to offset' });
        await mark.focus();
        assert.equal(await page.locator('.capture-ropes .inspected-wire').count(), 1);
        assert.equal(await page.locator('.capture:visible').getAttribute('aria-label'), 'Captured offset');
      }
      if (definition === 'sequence_without_values') {
        assert.equal(await page.locator('.binding-knot').count(), 0);
        assert.equal(await page.locator('.rope-sequence, .rope-bind, .rope-bind-gap').count(), 0);
        assert.equal(await page.locator('.monadic-context .reference').count(), 2);
      }
      if (['constructor_values', 'constructor_function', 'constructor_as_value'].includes(definition)) {
        const labels = await page.locator('.adt-name').all();
        assert.equal(labels.length, definition === 'constructor_values' ? 2 : 1);
        for (const label of labels) {
          await label.focus();
          assert.match(await page.getByRole('tooltip').innerText(), /Constructor of Stitch/);
        }
        if (definition !== 'constructor_values') {
          const named = definition === 'constructor_function';
          const parameter = page.locator('#board > .scope-box > .scope-body > .inputs-boundary .port');
          assert.equal(await parameter.innerText(), named ? 'n' : '');
          const fn = page.locator('.knot.call');
          const socket = named ? fn.getByRole('button', { name: 'Knit, input 1, connected to n' })
            : fn.locator('.input-rail .nublet');
          assert.equal(await fn.locator('.output-dot').count(), 1);
          assert.equal(await page.locator('#board > .scope-box > .definition-port').count(), 1);
          assert.equal(await fn.locator('.open-input').count(), 0);
          await socket.focus();
          assert.equal(await page.getByRole('tooltip').locator('strong').innerText(), 'Int');
          const start = await parameter.locator('.nublet').boundingBox();
          assert(await page.locator('path.inspected-wire').evaluate((path, start) => {
            const point = path.getPointAtLength(0).matrixTransform(path.getScreenCTM());
            return Math.hypot(point.x - start.x - start.width / 2, point.y - start.y - start.height / 2) < 1;
          }, start));
        }
      }
      if (definition === 'constructor_callback') {
        const ctor = page.locator('.knot.function-value').filter({ has: page.locator('.adt-name') });
        assert.equal(await ctor.locator('.open-input').count(), 1);
        await ctor.locator('.definition-port').focus();
        assert.equal(await page.getByRole('tooltip').locator('strong').innerText(), 'Int -> Stitch');
        assert.equal(await page.locator('path.inspected-wire').count(), 1);
      }
      if (definition === 'infix_function' || definition === 'infix_callback') {
        const local = definition === 'infix_callback';
        const knot = page.locator('.operator');
        assert.equal(await knot.locator('.knot-label').textContent(), local ? 'choose' : 'max');
        assert.equal(await knot.locator('.input-rail .socket').count(), 2);
        assert.equal(await knot.locator('.callee-inlet').count(), local ? 1 : 0);
        if (local) {
          await knot.locator('.callee-inlet').focus();
          assert.equal(await page.locator('path.inspected-wire').count(), 1);
        }
      }
      if (definition === 'composed_functions' || definition === 'composed_callbacks') {
        const box = page.getByRole('region', { name: '>> scope', exact: true });
        const named = definition === 'composed_functions';
        assert.deepEqual(await box.locator('.slub').allTextContents(),
          named ? ['String.length', 'max'] : ['f', 'g', 'Call']);
        assert.equal(await box.locator(':scope > .definition-port').count(), 1);
        assert.equal(await box.locator('.capture:visible').count(), named ? 0 : 2);
        for (const [mark, ty] of [['.chain-head', named ? 'String' : 'Int'], ['.chain-tail', 'Int']]) {
          await box.locator(mark).focus();
          assert.equal(await page.getByRole('tooltip').locator('strong').innerText(), ty);
          assert.equal(await page.locator('path.inspected-wire').count(), 1);
        }
      }
      if (definition === 'nested_tuple_pattern' || definition === 'nested_list_pattern') {
        const tuple = definition === 'nested_tuple_pattern';
        const arm = page.getByRole('region', { name: (tuple ? 'Just (x, y)' : '(Just n : rest) as all') + ' scope' });
        const hub = arm.locator(':scope > .scope-body > .inputs-boundary');
        assert.equal(await hub.locator('.pattern-group .pattern-symbol').innerText(), tuple ? '(,)' : 'Just');
        if (!tuple) {
          assert.equal(await hub.locator('.pattern-hub > .pattern-heading .port').innerText(), 'all');
          assert.equal(await hub.locator('.pattern-group .port').innerText(), 'n');
        }
        for (const name of tuple ? ['x', 'y'] : ['n', 'rest', 'all']) {
          assert(await hub.getByRole('button', { name: 'Connect parameter ' + name, exact: true }).isVisible());
        }
      }
      if (definition === 'self_recursion') {
        assert.equal(await page.locator('#board > .recursive-knot').count(), 1);
        const call = page.locator('.callsite.recursive-knot');
        assert(await page.locator('path.rope-recursive').evaluateAll(paths =>
          paths.length > 1 && paths.every(p => p.classList.contains('rope-capture') && getComputedStyle(p).strokeDasharray !== 'none')));
        assert.equal(await call.locator('.knot-label').innerText(), 'self_recursion');
        const end = call.locator('.callee-inlet');
        assert.equal(await end.evaluate(e => getComputedStyle(e, '::before').content), '"$"');
        await call.locator('.callee-socket').focus();
        assert.equal(await page.locator('path.inspected-wire[marker-end="url(#call-tip)"]').count(), 1);
        await page.locator('#board > .scope-box > .definition-port').focus();
        assert.equal(await page.locator('.definition-rope.inspected-wire').count(), 1);
      }
      if (definition === 'named_callsites') {
        assert.equal(await page.locator('.recursive-knot, .rope-recursive').count(), 0);
        const fn = page.getByRole('region', { name: 'twice scope', exact: true });
        await fn.locator(':scope > .definition-port').focus();
        assert.equal(await page.locator('.definition-rope.inspected-wire[marker-end="url(#call-tip)"]').count(), 2);
      }
      if (definition === 'local_self_recursion') {
        assert.equal(await page.locator('.callsite.recursive-knot').count(), 1);
        assert.equal(await page.locator('.callsite:not(.recursive-knot)').count(), 1);
        assert.equal(await page.locator('.scope-box.recursive-knot').count(), 1);
        assert.equal(await page.locator('.binding-knot').count(), 0);
        assert.equal(await page.getByRole('region', { name: 'count scope', exact: true }).count(), 1);
        assert(await page.getByRole('img', { name: 'Captured count', exact: true }).count() > 0);
      }
      if (definition === 'recursive_values') {
        assert.equal(await page.locator('.binding-knot.recursive-knot').count(), 2);
        assert.equal(await page.locator('.rope-recursive:not(.rope-capture)').count(), 2);
        assert.equal(await page.locator('.binding-knot').count(), 2);
        for (const [name, uses] of [['x', 2], ['y', 1]]) {
          await page.getByRole('button', { name: 'Connect parameter ' + name, exact: true }).focus();
          assert.equal(await page.locator('path.inspected-wire').count(), uses);
        }
      }
      if (definition === 'mutual_recursion') {
        assert.equal(await page.locator('.scope-box.recursive-knot').count(), 2);
        assert.equal(await page.locator('.callsite.recursive-knot').count(), 2);
        assert.equal(await page.locator('.callsite:not(.recursive-knot)').count(), 1);
        assert.equal(await page.locator('.binding-knot').count(), 0);
        for (const name of ['even', 'odd']) {
          assert(await page.getByRole('button', { name: 'Connect parameter ' + name, exact: true }).isVisible());
          assert(await page.getByRole('img', { name: 'Captured ' + name, exact: true }).count() > 0);
        }
      }
      if (definition === 'grouped_do_let' || definition === 'sequential_do_lets') {
        const grouped = definition === 'grouped_do_let';
        assert.deepEqual(graphs.get(definition).scope.body.steps.map(step => step.statement), grouped ? [0, 0] : [0, 1]);
        const selected = page.getByRole('button', { name: 'Connect parameter margin', exact: true });
        // The boundary argument is first in DOM order, the let binding second.
        assert.equal(await selected.count(), 2);
        for (const [i, binding] of (await selected.all()).entries()) {
          await binding.focus();
          const used = i === (grouped ? 1 : 0);
          assert.equal(await page.locator('path.inspected-wire').count(), Number(used));
        }
      }
      if (definition === 'list_patterns') {
        const arm = page.getByRole('region', { name: '[] scope', exact: true });
        const toggle = arm.getByRole('button', { name: 'Collapse [] box' });
        const open = await arm.boundingBox(), button = await toggle.boundingBox();
        assert(Math.abs(button.x + button.width / 2 - open.x - open.width / 2) < 1);
        assert(button.y > open.y + open.height / 2);
        await toggle.click();
        const expand = arm.getByRole('button', { name: 'Expand [] box' });
        assert.equal(await expand.getAttribute('aria-expanded'), 'false');
        assert.equal(await expand.textContent(), '▾');
        assert.equal(await arm.locator('.scope-header strong').textContent(), '[]');
        assert((await arm.boundingBox()).height < open.height);
        await expand.press('Enter');
        assert.equal(await toggle.textContent(), '▴');
      }
      if (definition === 'aliased_pattern') {
        assert(await page.getByRole('region', { name: '(1 : _) as ones scope', exact: true }).isVisible());
      }
      if (definition === 'lists_and_tuples') {
        assert.equal(await page.locator('.list > .knot-label').innerText(), '[(Int, String)]');
        assert.deepEqual(await page.locator('.tuple > .knot-label').allTextContents(), ['(Int, String)', '(Int, String)']);
      }
      if (definition === 'function_value' || definition === 'partial_application') {
        const fn = page.locator('.knot.call');
        const plus = definition === 'function_value';
        const inlets = fn.locator('.input-rail > .rail-end .nublet');
        const ports = page.locator('#board > .scope-box > .scope-body > .inputs-boundary .port');
        assert.deepEqual(await ports.allTextContents(), plus ? ['', ''] : ['']);
        assert.equal(await inlets.count(), plus ? 2 : 1);
        assert.equal(await fn.locator('.output-dot').count(), 1);
        assert.equal(await fn.locator('input.literal').count(), plus ? 0 : 1);
        assert.equal(await fn.locator('.knot-label').innerText(), plus ? '(+)' : 'max');
        await fn.locator('.knot-label').hover();
        const tip = page.getByRole('tooltip');
        assert.match(await tip.innerText(), plus ? /Num a => a -> a -> a/ : /Ord a => a -> a -> a/);
        assert.match(await tip.innerText(), /Int -> Int -> Int/);
        assert.match(await tip.innerText(), plus ? /Prelude.knot.*Built-in operator/ : /Prelude.knot/);
        for (const [i, inlet] of (await inlets.all()).entries()) {
          await inlet.focus();
          assert.equal(await tip.locator('strong').innerText(), 'Int');
          const start = await ports.nth(i).locator('.nublet').boundingBox();
          assert(await page.locator('path.inspected-wire').evaluate((path, start) => {
            const point = path.getPointAtLength(0).matrixTransform(path.getScreenCTM());
            return Math.hypot(point.x - start.x - start.width / 2, point.y - start.y - start.height / 2) < 1;
          }, start));
        }
      }
      if (definition === 'qualified_call' || definition === 'function_input') {
        const label = page.locator('.knot-label').first();
        await label.focus();
        const tip = page.getByRole('tooltip');
        if (definition === 'qualified_call') assert.match(await tip.innerText(), /String\.knot/);
        else {
          assert.equal(await label.innerText(), 'f');
          assert.equal(await page.locator('.callsite .input-rail .socket').count(), 1);
          assert.match(await tip.innerText(), /f :: Int -> Int/);
          assert.match(await tip.innerText(), /Local binding/);
        }
      }
      if (definition === 'slub_chain') {
        await page.locator('.slub-label').last().hover();
        assert.match(await page.getByRole('tooltip').innerText(), /Int -> List Int -> List Int/);
      }
      if (definition === 'shared_binding') {
        assert.equal(await page.locator('.binding-knot').count(), 1);
        await page.getByRole('button', { name: 'Connect parameter doubled' }).hover();
        assert.equal(await page.locator('path.inspected-wire').count(), 2);
      }
      if (definition === 'record_update') {
        assert.deepEqual(await page.locator('.record-update .input-rail > *').allTextContents(), ['record', 'y']);
        assert.equal(await page.locator('.field-access .knot-label').innerText(), '.x');
      }
      if (definition === 'cubic_point') {
        assert.equal(await page.locator('.binding-knot').count(), 6);
        assert.equal(await page.locator('.field-access').count(), 8);
        assert.equal(await page.locator('.knot.callsite').count(), 2);
        assert.equal(await page.locator('.capture:visible').count(), 4);
      }
    }
    assert.deepEqual(errors, []);
    console.log(
      'Tangle round trips passed: no-op save, literal, rope, reopen, fresh scene/SVG, exact Undo, slub chains, vertical definitions and captures, monadic binds, if/case arms, live box resizing, drag/save/reopen, stacked and inline positions, nested boxes, shadows and stale/invalid edits.',
    );
  } finally {
    await browser.close();
    for (const p of processes) p.kill();
    fs.rmSync(temp, { recursive: true, force: true });
  }
})().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
