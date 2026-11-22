const $ = (id) => document.getElementById(id);
const definition = new URLSearchParams(location.search).get('definition');
const catalogUrl = document.documentElement.dataset.catalog;
const storyMode = Boolean(catalogUrl) || location.pathname === '/tangle/stories';
const changes = new Map();
let graph,
  selected = null,
  busy = false,
  fitted = window.innerWidth >= 900,
  previewUrl,
  canUndo = false;
let rootItem,
  connections = [],
  layouts = [],
  wires = [],
  ports = new Map();
let sourceBytes, inspected = null;
const decoder = new TextDecoder();
const details = new WeakMap();
const functionDetails = new WeakMap();
// The two buttons flanking a conditional keyword expose the same subject.
const conditionalPeers = new WeakMap();
const editedSpans = new WeakMap();

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function status(message, error = false) {
  $('status').textContent = message;
  $('status').classList.toggle('error', error);
  $('reload').hidden = !error;
}

function updateButtons() {
  $('save').disabled = busy || !graph;
  $('undo').disabled = busy || (!canUndo && !changes.size);
  $('undo').textContent = changes.size ? 'Discard edits' : 'Undo';
}

function hint() {
  updateButtons();
  if (storyMode) status('Hover a connector to inspect its type and ropes.');
  else if (selected !== null) status('Choose an input socket for ' + ports.get(selected).name + '.');
  else if (changes.size)
    status(changes.size + (changes.size === 1 ? ' unsaved edit' : ' unsaved edits'));
  else status('Drag a node to position it, edit a number, or connect a parameter.');
}

function stage(expr, change, changed, control) {
  editedSpans.set(change, expr.token_span || expr.span);
  if (changed) changes.set(expr.id, change);
  else changes.delete(expr.id);
  control.classList.toggle('dirty', changed);
  hint();
}

function nub(shape = 'round') {
  const mark = el('span', 'nublet ' + shape);
  mark.setAttribute('aria-hidden', 'true');
  return mark;
}

function describe(mark, value) {
  details.set(mark, value);
  if (!mark.closest('button, .capture')) {
    mark.removeAttribute('aria-hidden');
    mark.setAttribute('role', 'img');
    mark.setAttribute('aria-label', value.ty + ' connector');
    mark.tabIndex = 0;
  }
  return mark;
}

function expressionText(value) {
  if (!value.span) return value.label;
  // Knot spans count UTF-8 bytes. Replay pending literal and binding edits within this expression.
  const { start, end } = value.span;
  const slice = (a, b) => decoder.decode(sourceBytes.subarray(a - graph.source_start, b - graph.source_start));
  const edits = [...changes.values()].filter(change => {
    const span = editedSpans.get(change);
    return span && span.start >= start && span.end <= end;
  }).sort((a, b) => editedSpans.get(a).start - editedSpans.get(b).start);
  let cursor = start, text = '';
  for (const change of edits) {
    const span = editedSpans.get(change);
    const replacement = change.kind === 'number' ? change.value : ports.get(change.parameter).name;
    text += slice(cursor, span.start) + replacement;
    cursor = span.end;
  }
  return text + slice(cursor, end);
}

function functionName(node, info) {
  if (info) {
    functionDetails.set(node, info);
    node.classList.add('function-name');
    node.tabIndex = 0;
  }
  return node;
}

function expressionName(node, expr) {
  if (!expr.constructor_type) return functionName(node, expr.callable);
  node.classList.add('adt-name');
  const info = expr.callable || { name: expr.label, signature: expr.ty, at_site: expr.ty };
  return functionName(node, { ...info, origin: ['Constructor of ' + expr.constructor_type, info.origin].filter(Boolean).join(' · ') });
}

function connectorAt(target) {
  return target.closest('.nublet, .function-name') || target.closest('.rail-end, .port, .socket')?.querySelector('.nublet');
}

function touchesInspected(wire) {
  if (!inspected) return false;
  const mark = inspected.matches('.port') ? inspected.querySelector('.nublet') : inspected;
  const peer = conditionalPeers.get(mark);
  return wire.from === mark || wire.socket === mark ||
    (peer !== undefined && (wire.from === peer || wire.socket === peer));
}

function inspect(mark) {
  mark = mark || null;
  if (mark === inspected) return;
  inspected?.classList.remove('inspected');
  conditionalPeers.get(inspected)?.classList.remove('inspected');
  if (inspected) (inspected.closest('button, .capture') || inspected).removeAttribute('aria-describedby');
  inspected = mark;
  const tip = $('port-tip');
  tip.hidden = !mark;
  for (const wire of connections) {
    wire.path?.classList.toggle('inspected-wire', touchesInspected(wire));
  }
  if (!mark) return;
  mark.classList.add('inspected');
  conditionalPeers.get(mark)?.classList.add('inspected');
  (mark.closest('button, .capture') || mark).setAttribute('aria-describedby', 'port-tip');
  const value = details.get(mark.classList.contains('condition-inner') ? conditionalPeers.get(mark) : mark);
  const info = functionDetails.get(mark);
  if (info) {
    tip.replaceChildren(el('strong', '', info.name + ' :: ' + info.signature));
    if (!mark.matches('.box-toggle, .port')) tip.append(el('code', '', 'At call site  ' + info.at_site));
    tip.append(el('small', '', info.origin));
  } else {
    tip.replaceChildren(el('strong', '', value.ty), el('code', '', expressionText(value)));
    if (mark.classList.contains('source-use')) tip.append(el('small', '', 'Used inside this source expression.'));
  }
  const rect = mark.getBoundingClientRect();
  tip.style.left = Math.max(4, Math.min(rect.left, innerWidth - tip.offsetWidth - 4)) + 'px';
  tip.style.top = (rect.bottom + 8 + tip.offsetHeight <= innerHeight ? rect.bottom + 8 :
    Math.max(4, rect.top - tip.offsetHeight - 8)) + 'px';
}

function selectPort(id) {
  selected = id;
  ports.forEach(({ control }, port) => control.setAttribute('aria-pressed', String(port === id)));
  hint();
  resize();
}

function field(expr, parentLabel, index, name, sources) {
  const label = parentLabel + ', input ' + (index + 1);
  if (expr.kind === 'number') {
    const input = el('input', 'literal');
    input.value = expr.label;
    input.setAttribute('aria-label', label + ', number');
    input.title = (name || label) + ' · ' + expr.ty;
    if (storyMode) { input.readOnly = true; return input; }
    input.oninput = () =>
      stage(
        expr,
        { kind: 'number', target: expr.id, value: input.value },
        input.value !== expr.label,
        input,
      );
    return input;
  }
  if (expr.kind !== 'reference') return expressionName(el('span', 'inline-value', expr.label), expr);
  const socket = el('button', 'socket', name);
  const endpoint = nub();
  socket.prepend(endpoint);
  const wire = {
    socket: endpoint,
    from: (captures) => sources(Number(socket.dataset.parameter), captures),
    control: socket,
  };
  wires.push(wire);
  function bind(port) {
    const binding = ports.get(port);
    socket.dataset.parameter = port;
    describe(endpoint, { ty: port === expr.parameter ? expr.ty : binding.ty, label: binding.name });
    socket.setAttribute('aria-label', label + ', connected to ' + binding.name);
  }
  function connect() {
    if (selected === null) {
      status('Choose a parameter output, then this input.');
      return;
    }
    if (!expr.visible.includes(selected)) {
      status('This binding is outside the input’s scope or shadowed.');
      return;
    }
    bind(selected);
    stage(
      expr,
      { kind: 'connect', target: expr.id, parameter: selected },
      selected !== expr.parameter,
      socket,
    );
    selectPort(null);
  }
  bind(expr.parameter);
  if (storyMode) return socket;
  socket.onclick = connect;
  socket.ondragover = (e) => {
    if (expr.visible.includes(selected)) {
      e.preventDefault();
      socket.classList.add('over');
    }
  };
  socket.ondragleave = () => socket.classList.remove('over');
  socket.ondrop = (e) => {
    e.preventDefault();
    socket.classList.remove('over');
    connect();
  };
  return socket;
}

const leaf = (expr) => ['reference', 'number', 'value'].includes(expr.kind);
const pinned = (expr) => expr.position !== undefined || expr.computed_position;
const inline = (expr) => leaf(expr) && !pinned(expr) && !(expr.kind === 'value' && expr.open_inputs);

function endpoint(label, side, shape = 'round') {
  const row = el('span', 'rail-end ' + side, label);
  const mark = nub(shape);
  row.append(mark);
  return { row, mark };
}

function boundarySource(header, outerSource, local) {
  const captured = new Map();
  return (port, routes) => {
    if (local.has(port)) return local.get(port);
    const from = outerSource(port, routes);
    if (!captured.has(port)) {
      const anchor = endpoint('', 'capture', 'capture-triangle');
      const binding = ports.get(port);
      describe(anchor.mark, { ty: binding.ty, label: binding.name });
      anchor.row.setAttribute('role', 'img');
      anchor.row.tabIndex = 0;
      anchor.row.setAttribute('aria-label', 'Captured ' + binding.name);
      header.append(anchor.row);
      captured.set(port, anchor.mark);
    }
    const mark = captured.get(port);
    routes.set(mark, { from, socket: mark, parameter: port });
    return mark;
  };
}

// A pattern consumes one value and exposes each checked binding on its inner side.
function patternHub(input, local, inArm = false) {
  const hub = el('div', 'pattern-hub');
  if (input.implicit) {
    hub.classList.add('scalar');
    const inlet = endpoint('', 'left');
    const outlet = endpoint('', 'port');
    for (const mark of [inlet.mark, outlet.mark]) describe(mark, input);
    hub.append(inlet.row, outlet.row);
    return { box: hub, inlet: inlet.mark, out: outlet.mark };
  }
  const scalar = input.parameters.length === 1 && input.parameters[0].name === input.pattern;
  const trunk = endpoint('', 'left', scalar ? 'round' : 'triangle');
  hub.classList.toggle('scalar', scalar);
  describe(trunk.mark, { ty: input.ty, label: input.pattern });
  hub.append(trunk.row);
  function port(p, whole = false) {
    const control = functionName(el('button', 'port', p.name), p.callable);
    const mark = nub(whole || scalar ? 'round' : 'square');
    control.append(mark);
    describe(mark, { ty: p.ty, label: p.name });
    control.setAttribute('aria-label', 'Connect parameter ' + p.name);
    control.setAttribute('aria-pressed', 'false');
    control.draggable = !storyMode;
    if (!storyMode) control.onclick = () => selectPort(selected === p.port ? null : p.port);
    control.ondragstart = (e) => {
      e.dataTransfer.setData('text/plain', p.name);
      selectPort(p.port);
    };
    control.ondragend = () => selectPort(null);
    ports.set(p.port, { name: p.name, ty: p.ty, control });
    local.set(p.port, mark);
    return control;
  }
  const byName = new Map(input.parameters.map(p => [p.name, p]));
  function draw(shape, group) {
    const heading = el('div', 'pattern-heading');
    if (shape.label) heading.append(el('span', 'pattern-symbol', shape.label));
    for (const name of shape.bindings) heading.append(port(byName.get(name), shape.children.length > 0));
    group.append(heading);
    if (!shape.children.length) return;
    const list = el('div', 'pattern-parts');
    for (const child of shape.children) {
      const part = el('div', child.children.length ? 'pattern-group' : 'pattern-part');
      draw(child, part);
      list.append(part);
    }
    group.append(list);
  }
  if (scalar) hub.append(port(input.parameters[0]));
  else if (!inArm || input.parameters.length) draw(input.shape, hub);
  return { box: hub, inlet: trunk.mark };
}

// A box owns its layout and boundary ports. Children return the anchor for
// their value; a lambda returns its bottom diamond, never its body's result.
function boxed(scope, name, outerSource, armOwner = null, explicitBlock = false, renderBody = null) {
  const box = el('section', 'scope-box');
  box.setAttribute('aria-label', name + ' scope');
  const header = el('div', 'scope-header');
  const patternArm = armOwner !== null && scope.inputs.length > 0;
  const toggle = el('button', 'box-toggle');
  toggle.setAttribute('aria-expanded', 'true');
  toggle.setAttribute('aria-label', 'Collapse ' + name + ' box');
  const chevron = el('span', 'chevron', patternArm ? '▴' : '⌄');
  toggle.append(chevron);
  if (patternArm) {
    header.append(el('strong', '', name));
    toggle.classList.add('pattern-toggle');
  } else {
    toggle.append(document.createTextNode(name));
    header.append(toggle);
  }
  const left = el('div', 'boundary inputs-boundary');
  // A parameterless box can forward a function value directly through its bottom port.
  const forwardsFunction = (!scope.inputs.length || armOwner !== null) && !!scope.body.open_inputs;
  const result = describe(forwardsFunction ? functionOutput() : outputDot(), scope.body);
  const canvas = el('div', 'scope-contents');
  const body = el('div', 'scope-body');
  body.append(left, canvas);
  box.append(header, body);
  (forwardsFunction ? box : body).append(result);
  if (patternArm) box.append(toggle);
  const local = new Map();
  const source = boundarySource(header, outerSource, local);
  const patterns = scope.inputs.map(input => patternHub(input, local, !!armOwner));
  left.append(...patterns.map(p => p.box));
  const block = scope.body.kind;
  let roots;
  if (renderBody) {
    roots = [renderBody(canvas, source, patterns)];
  } else if (['do', 'let'].includes(block) && (explicitBlock || !pinned(scope.body))) {
    box.classList.add(block === 'do' ? 'monadic-context' : 'let-context');
    if (!explicitBlock) header.append(el('span', 'context-label', block));
    roots = scopedSteps(scope.body, canvas, source, local);
  } else {
    roots = [expression(scope.body, canvas, source)];
  }
  wires.push({ from: roots.at(-1).out, socket: result });
  let out = result;
  const definition = !armOwner && scope.inputs.length > 0;
  if (definition) {
    out = functionOutput();
    box.append(out);
  }
  toggle.onclick = () => {
    const closed = box.classList.toggle('collapsed');
    toggle.setAttribute('aria-expanded', String(!closed));
    toggle.setAttribute('aria-label', (closed ? 'Expand ' : 'Collapse ') + name + ' box');
    chevron.textContent = patternArm ? (closed ? '▾' : '▴') : (closed ? '›' : '⌄');
    left.inert = closed;
    selectPort(null);
  };
  const item = { box, out, children: [], definition: definition || forwardsFunction, inlet: patterns[0]?.inlet };
  layouts.push({ canvas, left, roots, owner: armOwner || item, padding: [0, 16] });
  return item;
}

function functionOutput() {
  const mark = nub('diamond');
  mark.classList.add('definition-port');
  return mark;
}

function outputDot() {
  const dot = nub();
  dot.classList.add('output-dot');
  return dot;
}

// Coordinates describe the top left in the containing canvas, in unscaled pixels.
function placeNodes(frame, moving, dx = 0, dy = 0) {
  // Keep the starting envelope during a drag so the opposite edges stay still.
  const bounds = moving ? { ...frame.bounds } : { left: 0, top: 0, right: 0, bottom: 0 };
  const positions = frame.nodes.map(item => {
    const x = item.at[0] + (item === moving ? dx : 0);
    const y = item.at[1] + (item === moving ? dy : 0);
    bounds.left = Math.min(bounds.left, x);
    bounds.top = Math.min(bounds.top, y);
    bounds.right = Math.max(bounds.right, x + item.box.offsetWidth);
    bounds.bottom = Math.max(bounds.bottom, y + item.box.offsetHeight);
    return [x, y];
  });
  frame.nodes.forEach((item, i) => {
    item.box.style.left = positions[i][0] - bounds.left + 'px';
    item.box.style.top = positions[i][1] - bounds.top + 'px';
  });
  frame.canvas.style.width = bounds.right - bounds.left + frame.padding[0] + 'px';
  frame.canvas.style.height = bounds.bottom - bounds.top + frame.padding[1] + 'px';
  return bounds;
}

// Size dependencies before their consumer. Value inputs share the consumer's
// row; function definitions feed it from a separate row above.
function arrange(frame, captureSources) {
  const { canvas, left, roots } = frame;
  if (!canvas.getClientRects().length) return;
  const gapX = 30, gapY = 16;
  const captureLane = captureSources.has(left);
  left.classList.toggle('above-captures', captureLane);

  function measure(item) {
    const branches = item.children.map(measure);
    const values = branches.filter(branch => !branch.item.definition);
    const definitions = branches.filter(branch => branch.item.definition);
    const inputWidth = Math.max(0, ...values.map(b => b.width));
    const w = item.box.offsetWidth, h = item.box.offsetHeight;
    let x = values.length ? inputWidth + gapX : 0;
    const mid = Math.max(h / 2, values[0]?.mid || 0);
    let bottom = 0;
    for (const [i, branch] of values.entries()) {
      branch.x = inputWidth - branch.width;
      branch.y = i === 0 ? mid - branch.mid : bottom + gapY;
      bottom = branch.y + branch.height;
    }

    let above = 0, rowWidth = 0;
    if (definitions.length) {
      above = Math.max(...definitions.map(b => b.height)) + gapX;
      // Put the last diamond just left of the input rail. Multiple definitions
      // keep argument order across the row and leave their boxes clear of it.
      for (const branch of definitions) {
        branch.x = rowWidth;
        branch.y = above - gapX - branch.height;
        rowWidth += branch.width + gapX;
      }
      const last = definitions.at(-1);
      const diamondX = last.x + last.nodeX + last.item.box.offsetWidth / 2;
      const shift = Math.max(0, diamondX + gapY - x);
      x += shift;
      for (const branch of definitions) branch.x += x - gapY - diamondX;
      for (const branch of values) {
        branch.x += shift;
        branch.y += above;
      }
      rowWidth = last.x + last.width;
    }
    return { item, branches, nodeX: x, nodeY: above + mid - h / 2, mid: above + mid,
      width: Math.max(rowWidth, x + w), height: above + Math.max(mid + h / 2, bottom) };
  }

  const nodes = [];
  function place(branch, x, y) {
    const item = branch.item;
    item.auto = [Math.round(x + branch.nodeX), Math.round(y + branch.nodeY)];
    item.at = item.position ?? item.auto;
    item.frame = frame;
    nodes.push(item);
    for (const child of branch.branches) place(child, x + child.x, y + child.y);
  }

  const trees = roots.map(measure);
  if (canvas.closest('.scope-box').classList.contains('monadic-context')) {
    let y = captureLane ? left.offsetHeight + gapY : gapY;
    for (const tree of trees) {
      place(tree, 0, y);
      y += tree.height + 30;
    }
    frame.nodes = nodes;
    frame.bounds = placeNodes(frame);
    return;
  }
  const result = trees.pop();
  const top = captureLane ? left.offsetHeight + gapY : gapY;
  const width = Math.max(0, ...trees.map(tree => tree.width));
  let y = top;
  for (const tree of trees) {
    place(tree, width - tree.width, y);
    y += tree.height + gapY;
  }
  // Place the bindings beside their result, retaining source order down the column.
  const resultY = Math.max(top, (top + y - gapY) / 2 - result.mid);
  place(result, trees.length ? width + gapX : 0, resultY);
  frame.nodes = nodes;
  frame.bounds = placeNodes(frame);
}

function expression(expr, canvas, sources) {
  const item = makeExpression(expr, canvas, sources);
  describe(item.out, expr);
  movable(item, expr);
  return item;
}

function scopedSteps(expr, canvas, source, local) {
  const steps = expr.steps || [];
  // Allocate ports first so a simultaneous do-let group can refer to itself.
  const bindings = steps.map(step => step.pattern && patternHub(step.pattern, local));
  const roots = steps.map((step, index) => {
    const rhs = expression(step.value, canvas, source);
    const binding = bindings[index];
    if (!binding) return rhs;
    describe(binding.inlet, step.value);
    binding.box.classList.add('binding-knot');
    canvas.append(binding.box);
    wires.push({ from: rhs.out, socket: binding.inlet, kind: step.kind === 'let' ? 'value' : 'bind' });
    return { box: binding.box, children: [rhs] };
  });
  roots.push(expression(expr.inputs[0], canvas, source));
  return roots;
}

function alternatives(expr, canvas, sources) {
  const box = el('section', 'scope-box alternatives ' + expr.kind);
  box.setAttribute('aria-label', expr.kind + ' expression');
  const header = el('div', 'scope-header');
  const scrutinee = expr.inputs[0];
  const gate = endpoint('', 'condition-gate');
  const inner = describe(nub('condition-inner'), scrutinee);
  const keyword = el('span', 'condition-keyword');
  keyword.append(gate.row, el('strong', '', expr.kind), inner);
  header.append(keyword);
  const rows = el('div', 'arms');
  const result = expr.open_inputs ? functionOutput() : outputDot();
  box.append(header, rows, result);
  canvas.append(box);
  const item = { box, out: result, children: [], definition: !!expr.open_inputs };
  if (inline(scrutinee)) {
    const socket = field(scrutinee, expr.label, 0, '', sources);
    if (scrutinee.kind === 'reference') {
      gate.row.replaceChildren(socket);
      gate.mark = socket.querySelector('.nublet');
    } else {
      header.append(socket);
    }
  } else {
    const subject = expression(scrutinee, canvas, sources);
    item.children.push(subject);
    wires.push({ from: subject.out, socket: gate.mark });
  }
  if (!gate.mark.closest('button')) describe(gate.mark, scrutinee);
  for (const mark of [gate.mark, inner]) mark.classList.add('condition-button');
  conditionalPeers.set(gate.mark, inner);
  conditionalPeers.set(inner, gate.mark);
  if (expr.kind === 'case') header.append(el('span', '', 'of'));
  if (expr.incomplete) header.append(el('span', 'match-warning', '⚠ Incomplete match'));
  const armSource = boundarySource(header, sources, new Map());
  expr.arms.forEach((scope, i) => {
    const name = expr.kind === 'case' ? scope.inputs[0].pattern : ['then', 'else'][i];
    const arm = boxed(scope, name, armSource, item);
    arm.box.classList.add('branch-arm');
    if (scope.unreachable) arm.box.querySelector('.scope-header').append(el('span', 'match-warning', '⚠ Unreachable arm'));
    rows.append(arm.box);
    if (arm.inlet) wires.push({ from: inner, socket: arm.inlet, kind: 'pattern' });
    wires.push({ from: arm.out, socket: result, kind: 'merge' });
  });
  return item;
}

// A pipeline supplies a value now. A composition supplies it later through
// the inner port of a function box. Both use the same chain of stages.
function slubChain(stages, canvas, sources, input, upstream, returnsFunction) {
  const chain = el('div', 'slub-chain');
  const entry = describe(nub(), input);
  entry.classList.add('chain-head');
  const exit = returnsFunction ? functionOutput() : nub();
  if (!returnsFunction) exit.classList.add('chain-tail');
  const children = [];
  chain.append(entry);
  for (const step of stages) {
    const pill = el('span', 'slub');
    if (!pinned(step) && step.kind === 'reference') {
      const inlet = endpoint('', 'stage-function');
      describe(inlet.mark, step);
      inlet.row.append(expressionName(el('span', 'slub-label', step.label), step));
      pill.append(inlet.row);
      wires.push({ from: captures => sources(step.parameter, captures), socket: inlet.mark, parameter: step.parameter });
    } else if (inlineStage(step)) {
      pill.append(expressionName(el('span', 'slub-label', step.label), step));
      step.inputs.forEach((arg, i) =>
        pill.append(field(arg, step.label, i, step.labels?.[i] || '', sources)));
    } else {
      const fn = expression(step, canvas, sources);
      const inlet = endpoint('<apply>', 'stage-function');
      describe(inlet.mark, step);
      pill.append(inlet.row);
      children.push(fn);
      wires.push({ from: fn.out, socket: inlet.mark });
    }
    chain.append(pill);
  }
  chain.append(exit);
  wires.push({ from: upstream, socket: entry });
  canvas.append(chain);
  return { box: chain, out: exit, children, definition: returnsFunction };
}

function inlineStage(expr) {
  return !pinned(expr) && ['call', 'value'].includes(expr.kind) && expr.inputs.every(inline);
}

function composition(expr, canvas, sources) {
  const stages = [];
  function flatten(stage) {
    if (stage.kind === 'composition' && !pinned(stage)) stage.inputs.forEach(flatten);
    else stages.push(stage);
  }
  expr.inputs.forEach(flatten);
  const input = { implicit: true, label: 'Composition input', ty: expr.open_inputs[0] };
  const result = { label: 'Composition result', ty: expr.composition_result };
  const item = boxed({ inputs: [input], body: result }, '>>', sources, null, false,
    (inner, source, [argument]) => {
      const chain = slubChain(stages, inner, source, input, argument.out, false);
      describe(chain.out, result);
      return chain;
    });
  item.box.classList.add('composition');
  functionName(item.box.querySelector('.box-toggle'), expr.callable);
  canvas.append(item.box);
  return item;
}

function makeExpression(expr, canvas, sources, forwarded = []) {
  // Only stages with inline captures can sit on a rope. More complex
  // right-hand expressions retain explicit inputs and their own ropes.
  if (expr.kind === 'composition') return composition(expr, canvas, sources);
  if (expr.kind === 'if' || expr.kind === 'case') return alternatives(expr, canvas, sources);
  if (expr.kind === 'do' || expr.kind === 'let') {
    const item = boxed({ inputs: [], body: expr }, expr.kind, sources, null, true);
    canvas.append(item.box);
    return item;
  }
  if (expr.scope) {
    const item = definitionBox(expr.scope, 'λ', sources);
    canvas.append(item.box);
    return item;
  }
  const stages = [];
  let base = expr;
  while (base.kind === 'pipe' && (base === expr || !pinned(base))) {
    const rhs = base.inputs[1];
    if (!inlineStage(rhs)) break;
    stages.unshift(rhs);
    base = base.inputs[0];
  }
  if (stages.length) {
    const upstream = expression(base, canvas, sources);
    const chain = slubChain(stages, canvas, sources, base, upstream.out, !!expr.open_inputs);
    chain.children.unshift(upstream);
    return chain;
  }
  const knot = el('article', 'knot ' + base.kind);
  const item = { box: knot, children: [] };
  canvas.append(knot);
  let label = base.label;
  switch (base.kind) {
    case 'field-access': label = '.' + label; break;
    case 'apply': label = '<apply>'; break;
    case 'source': label = 'Source'; break;
  }
  const title = expressionName(el('div', 'knot-label', label), base);
  if (base.callee_port !== undefined) {
    const inlet = describe(nub(), { ty: base.callable?.at_site ?? ports.get(base.callee_port).ty, label });
    inlet.classList.add('callee-inlet');
    title.prepend(inlet);
    wires.push({ from: captures => sources(base.callee_port, captures), socket: inlet, parameter: base.callee_port });
  }
  knot.append(title);
  const body = el('div', 'knot-body');
  const rail = el('div', 'input-rail');
  function add(arg, i, name) {
    if (arg === base || inline(arg)) {
      const row = el('div', 'input-row');
      if (arg.kind !== 'reference') {
        row.classList.add('nubbed');
        row.append(field(arg, base.label, i, name, sources));
        if (name) row.append(el('span', 'argument-name', name));
      } else row.append(field(arg, base.label, i, name, sources));
      rail.append(row);
    } else {
      const target = endpoint(name, 'left');
      describe(target.mark, arg);
      rail.append(target.row);
      const child = expression(arg, canvas, sources);
      item.children.push(child);
      wires.push({ from: child.out, socket: target.mark });
    }
  }
  if (base.kind === 'source') {
    for (const port of base.source_ports || []) {
      const binding = ports.get(port);
      const inlet = endpoint(binding.name, 'left', 'source-use');
      inlet.mark.textContent = '🧩';
      describe(inlet.mark, { ty: binding.ty, label: binding.name });
      inlet.mark.setAttribute('aria-label', 'Source dependency ' + binding.name);
      wires.push({ from: captures => sources(port, captures), socket: inlet.mark, parameter: port });
      rail.append(inlet.row);
    }
    rail.append(el('pre', 'fold', base.label));
  } else if (base.kind === 'number' || base.kind === 'reference') add(base, 0, '');
  else
    base.inputs.forEach((arg, i) =>
      add(arg, i, base.labels?.[i] || (base.kind === 'list' ? String(i + 1) : '')),
    );
  for (const [i, { argument, from }] of forwarded.entries()) {
    const inlet = endpoint(base.labels?.[base.inputs.length + i] || '', 'left');
    describe(inlet.mark, argument);
    rail.append(inlet.row);
    wires.push({ from, socket: inlet.mark });
  }
  body.append(rail);
  knot.append(body);
  item.definition = !!base.open_inputs;
  if (item.definition) {
    knot.classList.add('function-value');
    for (const [i, ty] of base.open_inputs.entries()) {
      const unused = endpoint(base.labels?.[base.inputs.length + i] || '', 'left');
      unused.mark.classList.add('open-input');
      describe(unused.mark, { ty, label: 'Unconnected input' });
      rail.append(unused.row);
    }
    item.out = functionOutput();
    knot.append(item.out);
  } else {
    item.out = outputDot();
    body.append(item.out);
  }
  return item;
}

// Resize from the dragged node outwards. Growing through a left or top edge
// moves the enclosing box by the same amount, keeping the grip under the pointer.
function previewMove(item, dx, dy, scale) {
  let frame = item.frame;
  while (frame) {
    const bounds = placeNodes(frame, item, dx, dy);
    dx = bounds.left - frame.bounds.left;
    dy = bounds.top - frame.bounds.top;
    item = frame.owner;
    frame = item?.frame;
  }
  $('board').style.translate = dx * scale + 'px ' + dy * scale + 'px';
  sizeViewport(scale);
  drawWires(connections);
}

function movable(item, expr) {
  const handle = item.box.querySelector(':scope > .scope-header, :scope > .knot-label') || item.box;
  item.position = expr.position;
  if (storyMode) return;
  if (expr.computed_position) {
    handle.title = 'This position is a Knot expression.';
    return;
  }
  handle.classList.add('move-handle');
  if (!handle.closest('.function-name') && !handle.querySelector('.function-name')) handle.title = 'Drag to position';
  let drag, suppressClick = false;
  handle.addEventListener('click', e => {
    if (suppressClick && e.detail) {
      e.preventDefault();
      e.stopPropagation();
    }
  }, true);
  handle.onpointerdown = e => {
    if (!e.isPrimary || e.button !== 0 || busy || e.target.closest('input, .socket, .port, .nublet, .capture')) return;
    suppressClick = false;
    const board = $('board');
    drag = { id: e.pointerId, x: e.clientX, y: e.clientY, at: [...item.at],
      scrollX: $('viewport').scrollLeft, scrollY: $('viewport').scrollTop,
      scale: board.getBoundingClientRect().width / board.offsetWidth };
    e.target.setPointerCapture(e.pointerId);
  };
  function offset(e) {
    const viewport = $('viewport');
    return [(e.clientX - drag.x + viewport.scrollLeft - drag.scrollX) / drag.scale,
      (e.clientY - drag.y + viewport.scrollTop - drag.scrollY) / drag.scale];
  }
  handle.onpointermove = e => {
    if (drag?.id !== e.pointerId) return;
    if (!suppressClick && Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < 3) return;
    suppressClick = true;
    const [dx, dy] = offset(e);
    previewMove(item, dx, dy, drag.scale);
  };
  handle.onpointerup = e => {
    if (drag?.id !== e.pointerId) return;
    if (suppressClick) {
      const xy = offset(e).map((delta, i) => Math.round(drag.at[i] + delta));
      const target = expr.id ?? null;
      const key = 'position:' + target;
      if (xy.some((v, i) => v !== drag.at[i])) {
        const original = expr.position ?? item.auto;
        if (original.every((v, i) => v === xy[i])) {
          item.position = expr.position;
          changes.delete(key);
        } else {
          item.position = xy;
          changes.set(key, { kind: 'position', target, x: xy[0], y: xy[1] });
        }
        handle.classList.toggle('dirty', changes.has(key));
      }
      resize();
      hint();
    }
    drag = null;
  };
  handle.onlostpointercapture = () => {
    if (!drag) return;
    drag = null;
    resize();
  };
}

// A definition exposes its remaining arguments even when the source omits
// their names. These ports describe the call and have no editable source span.
function definitionBox(scope, name, outerSource) {
  const fn = scope.body;
  if (!fn.open_inputs) return boxed(scope, name, outerSource);
  const implicit = fn.open_inputs.map((ty, i) => ({
    implicit: true, ty, label: 'Unnamed argument ' + (scope.inputs.length + i + 1),
  }));
  const inputs = [...scope.inputs, ...implicit];
  const result = { ty: fn.applied_result, label: name + ' result' };
  return boxed({ inputs, body: result }, name, outerSource, null, false, (canvas, source, patterns) => {
    const direct = ['call', 'value', 'apply', 'operator'].includes(fn.kind);
    const call = direct ? { ...fn, kind: fn.kind === 'value' ? 'call' : fn.kind }
      : { kind: 'apply', label: 'apply', inputs: [fn], callable: fn.callable };
    call.ty = result.ty;
    call.open_inputs = undefined;
    const forwarded = implicit.map((argument, i) => ({ argument, from: patterns[scope.inputs.length + i].out }));
    const item = makeExpression(call, canvas, source, forwarded);
    if (direct) movable(item, fn);
    describe(item.out, result);
    return item;
  });
}

function render() {
  inspect(null);
  const board = $('board');
  board.replaceChildren($('wires'));
  layouts = [];
  wires = [];
  ports = new Map();
  selected = null;
  rootItem = definitionBox(graph.scope, graph.definition);
  functionName(rootItem.box.querySelector('.box-toggle'), graph.callable);
  if (rootItem.definition) describe(rootItem.out, { ty: graph.ty, label: graph.definition });
  rootItem.frame = { canvas: board, nodes: [rootItem], padding: [24, 24] };
  movable(rootItem, graph);
  board.append(rootItem.box);
  resize();
}

function resize() {
  const board = $('board');
  if (!rootItem) return;
  const captures = new Map();
  const ordinary = wires.map((wire) => ({
    ...wire,
    from: typeof wire.from === 'function' ? wire.from(captures) : wire.from,
  }));
  for (const anchor of board.querySelectorAll('.capture'))
    anchor.hidden = !captures.has(anchor.querySelector('.nublet'));
  const captureSources = new Set([...captures.values()].map(w => w.from.closest('.inputs-boundary')));
  for (const layout of layouts) arrange(layout, captureSources);
  rootItem.auto = [24, 24];
  rootItem.at = rootItem.position ?? rootItem.auto;
  rootItem.frame.bounds = placeNodes(rootItem.frame);
  board.style.translate = '';
  connections = [...captures.values(), ...ordinary];
  const w = board.offsetWidth,
    h = board.offsetHeight;
  const scale = fitted
    ? Math.min(1, $('viewport').clientWidth / w, $('viewport').clientHeight / h)
    : 1;
  sizeViewport(scale);
  $('fit').textContent = fitted ? 'Actual size' : 'Fit graph';
  drawWires(connections);
}

function sizeViewport(scale) {
  const board = $('board');
  board.style.transform = 'scale(' + scale + ')';
  $('extent').style.width = board.offsetWidth * scale + 'px';
  $('extent').style.height = board.offsetHeight * scale + 'px';
}

function drawWires(connections) {
  const svg = $('wires');
  svg.replaceChildren(svg.querySelector('defs'));
  const board = $('board').getBoundingClientRect();
  const scale = board.width / $('board').offsetWidth;
  if (!scale) return;
  const center = (r, origin) => [
    (r.left + r.width / 2 - origin.left) / scale,
    (r.top + r.height / 2 - origin.top) / scale,
  ];
  // A capture belongs behind the children of the scope it crosses, but above
  // that scope's own background. Each scope has its own drawing surface.
  for (const layer of $('board').querySelectorAll('.capture-ropes')) layer.replaceChildren();
  for (const wire of connections) {
    const start = wire.from;
    if (!start?.getClientRects().length || !wire.socket.getClientRects().length) continue;
    const entersCapture = wire.socket.classList.contains('capture-triangle');
    const scope = entersCapture
      ? wire.socket.closest('.scope-box').parentElement.closest('.scope-box')
      : start.classList.contains('capture-triangle') && start.closest('.scope-box');
    let layer = svg;
    if (scope) {
      layer = scope.querySelector(':scope > .capture-ropes');
      if (!layer) {
        layer = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
        layer.classList.add('capture-ropes');
        layer.setAttribute('aria-hidden', 'true');
        scope.prepend(layer);
      }
    }
    const origin = scope ? layer.getBoundingClientRect() : board;
    const [x, y] = center(start.getBoundingClientRect(), origin);
    const [u, v] = center(wire.socket.getBoundingClientRect(), origin);
    const bend = Math.max(12, Math.abs(u - x) * 0.42);
    const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    const verticalBend = Math.max(8, Math.abs(v - y) * 0.4);
    let exit = [x + bend, y];
    if (start.classList.contains('definition-port')) exit = [x, y + verticalBend];
    else if (start.classList.contains('capture-triangle')) exit = [x, y + verticalBend];
    const entersBottom = wire.socket.classList.contains('definition-port');
    const entry = entersCapture || entersBottom || wire.socket.closest('.stage-function') ? [u, v - verticalBend] : [u - bend, v];
    let d = `M ${x} ${y} C ${exit.join(' ')} ${entry.join(' ')} ${u} ${v}`;
    if (wire.kind === 'pattern') {
      const trunk = (wire.socket.closest('.arms').getBoundingClientRect().left - board.left) / scale + 7;
      d = `M ${x} ${y} V ${y + 11} H ${trunk} V ${v} H ${u}`;
    }
    if (wire.kind) path.classList.add('rope-' + wire.kind);
    path.setAttribute('d', d);
    const parameter = wire.control ? Number(wire.control.dataset.parameter) : wire.parameter;
    if (parameter !== undefined)
      path.setAttribute(
        'class',
        selected === parameter ? 'lit-wire' : 'parameter-wire',
      );
    layer.append(path);
    wire.path = path;
    if (touchesInspected(wire)) path.classList.add('inspected-wire');
    if (wire.kind === 'bind') {
      const gap = path.cloneNode(false);
      gap.setAttribute('class', 'rope-bind-gap');
      gap.setAttribute('marker-end', 'url(#bind-tip)');
      svg.append(gap);
    }
  }
}

async function request(url, body) {
  const response = await fetch(
    url,
    body === undefined
      ? {}
      : {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        },
  );
  if (!response.ok) throw Error('Request failed (' + response.status + ').');
  const data = await response.json();
  if (data.ok === false || data.error) throw Error(data.reason || data.error);
  return data;
}

async function loadStory() {
  const { stories } = await request(catalogUrl || '/api/tangle/stories');
  const groups = new Map();
  const nav = $('stories');
  nav.replaceChildren();
  const name = definition || stories[0].graph.definition;
  for (const { group, graph } of stories) {
    if (!groups.has(group)) {
      const section = el('section');
      section.append(el('h2', '', group));
      groups.set(group, section);
      nav.append(section);
    }
    const link = el('a', '', graph.definition.replaceAll('_', ' '));
    link.href = '?definition=' + encodeURIComponent(graph.definition);
    if (graph.definition === name) link.setAttribute('aria-current', 'page');
    groups.get(group).append(link);
  }
  nav.querySelector('[aria-current]')?.scrollIntoView({ block: 'nearest' });
  const story = stories.find(story => story.graph.definition === name);
  if (!story) throw Error('Unknown story. Choose one from the list.');
  return story;
}

async function load() {
  const result = storyMode ? await loadStory() : await request(
    definition ? '/api/tangle?definition=' + encodeURIComponent(definition) : '/api/tangle',
  );
  graph = result.graph;
  sourceBytes = new TextEncoder().encode(graph.source);
  canUndo = result.canUndo;
  changes.clear();
  $('module').textContent = storyMode ? 'Syntax storybook' : graph.module;
  $('definition').textContent = graph.definition;
  $('source').textContent = graph.source;
  const old = previewUrl;
  const image = $('preview');
  image.hidden = !result.preview;
  $('preview-message').hidden = storyMode || !!result.preview;
  if (result.preview) {
    previewUrl = URL.createObjectURL(new Blob([result.preview], { type: 'image/svg+xml' }));
    image.src = previewUrl;
    image.alt = graph.definition + ' preview';
  } else {
    image.removeAttribute('src');
    previewUrl = null;
    $('preview-message').textContent =
      result.previewError || 'No preview defined for ' + graph.definition + '.';
  }
  if (old) URL.revokeObjectURL(old);
  render();
  hint();
}

async function run(action) {
  if (busy) return;
  busy = true;
  $('board').inert = true;
  $('save').disabled = $('undo').disabled = true;
  try {
    await action();
  } catch (error) {
    status(error.message, true);
  } finally {
    busy = false;
    $('board').inert = false;
    updateButtons();
  }
}

$('save').onclick = () =>
  run(async () => {
    await request('/api/tangle', {
      snapshot: graph.snapshot,
      definition: graph.definition,
      changes: [...changes.values()],
    });
    await load();
    status('Saved.');
  });
$('undo').onclick = () =>
  run(async () => {
    if (changes.size) {
      await load();
      status('Unsaved edits discarded.');
      return;
    }
    await request('/api/undo', { snapshot: graph.snapshot });
    await load();
    status('Edit undone.');
  });
$('fit').onclick = () => {
  fitted = !fitted;
  resize();
};
$('reload').onclick = () => run(load);
$('board').addEventListener('pointerover', e => {
  if (!e.buttons && e.pointerType !== 'touch') inspect(connectorAt(e.target));
});
$('board').addEventListener('pointerout', e => {
  if (!e.relatedTarget || !$('board').contains(e.relatedTarget)) inspect(null);
});
$('board').addEventListener('focusin', e => inspect(connectorAt(e.target)));
$('board').addEventListener('focusout', () => inspect(null));
$('board').addEventListener('pointerdown', () => inspect(null));
$('viewport').addEventListener('scroll', () => inspect(null));
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') inspect(null);
});
window.addEventListener('resize', resize);
window.addEventListener('beforeunload', (event) => {
  if (changes.size) event.preventDefault();
});
if (storyMode) {
  document.body.classList.add('story-mode');
  document.title = 'Tangle · Syntax storybook';
  $('stories').hidden = false;
  $('gallery-link').href = catalogUrl ? '../' : '/tangle';
  $('gallery-link').textContent = catalogUrl ? 'Demos' : 'Definition editor';
  $('undo').hidden = $('save').hidden = $('preview-heading').hidden = true;
}
run(load);
