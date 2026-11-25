const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
let definition = params.get('definition');
const host = params.get('embed') === '1' && window.frameElement?.id === 'drawing-graph'
  ? window.parent.drawingGraphHost : null;
const catalogUrl = document.documentElement.dataset.catalog;
const storyMode = !host && (Boolean(catalogUrl) || location.pathname === '/tangle/stories');
const changes = new Map();
let graph,
  selected = null,
  busy = false,
  fitted = Boolean(host) || window.innerWidth >= 900,
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
  $('choose-definition').disabled = busy || changes.size > 0;
  $('save').disabled = busy || !graph || (host && (!changes.size || staleGraph()));
  $('undo').disabled = busy || (!canUndo && !changes.size);
  $('undo').textContent = changes.size ? 'Discard edits' : 'Undo';
}

function hint() {
  updateButtons();
  if (storyMode) status('Hover an eyelet to inspect its type and ropes.');
  else if (host && changes.size && staleGraph()) status('Drawing changed. Discard edits to load the current graph.', true);
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

function eyelet(shape = 'round') {
  const mark = el('span', 'eyelet ' + shape);
  mark.setAttribute('aria-hidden', 'true');
  return mark;
}

function describe(mark, value) {
  details.set(mark, value);
  if (!mark.closest('button, .capture')) {
    mark.removeAttribute('aria-hidden');
    mark.setAttribute('role', 'img');
    mark.setAttribute('aria-label', value.ty + ' eyelet');
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

function eyeletAt(target) {
  return target.closest('.eyelet, .function-name') || target.closest('.rail-end, .port, .socket')?.querySelector('.eyelet');
}

function touchesInspected(wire) {
  if (!inspected) return false;
  const mark = inspected.matches('.port') ? inspected.querySelector('.eyelet') : inspected;
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

function field(expr, parentLabel, index, name, sources, onBind) {
  const label = onBind ? 'Callsite function' : parentLabel + ', input ' + (index + 1);
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
    if (host) {
      input.onchange = commitEmbedded;
      input.onkeydown = event => { if (event.key === 'Enter') input.blur(); };
    }
    return input;
  }
  if (expr.kind !== 'reference') return expressionName(el('span', 'inline-value', expr.label), expr);
  const socket = el('button', 'socket', name);
  const endpoint = eyelet();
  socket.prepend(endpoint);
  const wire = {
    socket: endpoint,
    from: (captures) => sources(Number(socket.dataset.parameter), captures),
    control: socket,
    recursive: expr.recursive,
  };
  wires.push(wire);
  function bind(port) {
    const binding = ports.get(port);
    wire.recursive = expr.recursive && port === expr.parameter;
    onBind?.(binding);
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
    commitEmbedded();
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
  const mark = eyelet(shape);
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

function bindOutput(p, control) {
  control.setAttribute('aria-label', 'Connect parameter ' + p.name);
  control.setAttribute('aria-pressed', 'false');
  control.draggable = !storyMode;
  if (!storyMode) control.onclick = () => selectPort(selected === p.port ? null : p.port);
  control.ondragstart = (e) => {
    e.dataTransfer.setData('text/plain', p.name);
    selectPort(p.port);
  };
  control.ondragend = () => selectPort(null);
  ports.set(p.port, { ...p, control });
  if (control.matches('.definition-port')) {
    control.setAttribute('role', 'button');
    control.onkeydown = e => {
      if (!storyMode && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); control.click(); }
    };
  }
}

// A pattern consumes one value and exposes each checked binding on its inner side.
function bobbin(input, local, inArm = false) {
  const pill = el('div', 'bobbin');
  if (input.implicit) {
    pill.classList.add('scalar');
    const inlet = endpoint('', 'left');
    const outlet = endpoint('', 'port');
    for (const mark of [inlet.mark, outlet.mark]) describe(mark, input);
    pill.append(inlet.row, outlet.row);
    return { box: pill, inlet: inlet.mark, out: outlet.mark };
  }
  const scalar = input.parameters.length === 1 && input.parameters[0].name === input.pattern;
  const trunk = endpoint('', 'left', scalar ? 'round' : 'triangle');
  pill.classList.toggle('scalar', scalar);
  describe(trunk.mark, { ty: input.ty, label: input.pattern });
  pill.append(trunk.row);
  function port(p, whole = false) {
    const control = functionName(el('button', 'port', p.name), p.callable);
    const mark = eyelet(whole || scalar ? 'round' : 'square');
    control.append(mark);
    describe(mark, { ty: p.ty, label: p.name });
    bindOutput(p, control);
    local.set(p.port, mark);
    return control;
  }
  const byName = new Map(input.parameters.map(p => [p.name, p]));
  // `a : b : rest` reads as one list, so its spine is one column of rows and
  // not a box per `:`. A tail with its own name keeps its group.
  function spine(shape) {
    const items = [];
    let tail = shape;
    while (tail.label === ':' && tail.children.length === 2 && (tail === shape || !tail.bindings.length)) {
      items.push(tail.children[0]);
      tail = tail.children[1];
    }
    return [...items, tail];
  }
  function part(child) {
    const box = el('div', child.children.length ? 'pattern-group' : 'pattern-part');
    draw(child, box);
    return box;
  }
  function draw(shape, group) {
    if (shape.label === ':' && shape.children.length === 2) {
      if (shape.bindings.length) {
        const heading = el('div', 'pattern-heading');
        for (const name of shape.bindings) heading.append(port(byName.get(name), true));
        group.append(heading);
      }
      const rows = el('div', 'pattern-parts pattern-chain');
      const items = spine(shape);
      items.forEach((child, i) => {
        const row = el('div', 'pattern-row' + (i === items.length - 1 ? ' pattern-tail' : ''));
        row.append(el('span', 'pattern-symbol', i ? ':' : ''), part(child));
        rows.append(row);
      });
      group.append(rows);
      return;
    }
    const heading = el('div', 'pattern-heading');
    if (shape.label) heading.append(el('span', 'pattern-symbol', shape.label));
    for (const name of shape.bindings) heading.append(port(byName.get(name), shape.children.length > 0));
    group.append(heading);
    if (!shape.children.length) return;
    const list = el('div', 'pattern-parts');
    for (const child of shape.children) list.append(part(child));
    group.append(list);
  }
  if (scalar) pill.append(port(input.parameters[0]));
  else if (!inArm || input.parameters.length) draw(input.shape, pill);
  return { box: pill, inlet: trunk.mark };
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
  const patterns = scope.inputs.map(input => bobbin(input, local, !!armOwner));
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
  const item = { box, out, bindings: local, children: [], definition: definition || forwardsFunction, inlet: patterns[0]?.inlet };
  layouts.push({ canvas, left, roots, owner: armOwner || item, padding: [0, 16], bands: roots.bands });
  return item;
}

function functionOutput() {
  const mark = eyelet('diamond');
  mark.classList.add('definition-port');
  return mark;
}

function outputDot() {
  const dot = eyelet();
  dot.classList.add('output-dot');
  return dot;
}

// Coordinates describe the top left in the containing canvas, in unscaled pixels.
function placeNodes(frame, moving, dx = 0, dy = 0) {
  if (frame.bands) return placeBands(frame, moving, dx, dy);
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
    // Inputs marked offRow sit below the consumer's row, clear of its definition port's rope.
    const lead = values[0] && !values[0].item.offRow;
    const mid = Math.max(h / 2, lead ? values[0].mid : 0);
    let bottom = lead ? 0 : mid + h / 2;
    for (const [i, branch] of values.entries()) {
      branch.x = inputWidth - branch.width;
      branch.y = i === 0 && lead ? mid - branch.mid : bottom + gapY;
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
  if (frame.bands) {
    frame.top = captureLane ? left.offsetHeight + gapY : 0;
    frame.clamp = bandClamp;
    // Each band stacks its trees from its own top; grouped lets share a band.
    const next = frame.bands.map(() => BAND.pad);
    trees.forEach((tree, i) => {
      const band = roots[i].band, start = nodes.length;
      place(tree, BAND.gutter + BAND.pad, next[band]);
      for (const item of nodes.slice(start)) item.band = band;
      next[band] += tree.height + gapY;
    });
    frame.nodes = nodes;
    frame.bounds = placeNodes(frame);
    return;
  }
  const result = trees.pop();
  const top = captureLane ? left.offsetHeight + gapY : gapY;
  // Named functions are shared producers. Lay them above the row of call sites.
  const bindings = trees.filter(tree => !tree.item.definition);
  const definitions = trees.filter(tree => tree.item.definition);
  let definitionX = 0;
  for (const tree of definitions) {
    place(tree, definitionX, top);
    definitionX += tree.width + gapX;
  }
  const bindingTop = top + Math.max(0, ...definitions.map(tree => tree.height + gapX));
  const width = Math.max(0, ...bindings.map(tree => tree.width));
  let y = bindingTop;
  for (const tree of bindings) {
    place(tree, width - tree.width, y);
    y += tree.height + gapY;
  }
  // Place the bindings beside their result, retaining source order down the column.
  const resultY = Math.max(bindingTop, (bindingTop + y - gapY) / 2 - result.mid);
  place(result, bindings.length ? width + gapX : 0, resultY);
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
  const bindings = steps.map(step => {
    if (step.named_function) {
      const [binding] = step.pattern.parameters;
      ports.set(binding.port, binding);
      return null;
    }
    return step.pattern && bobbin(step.pattern, local);
  });
  const roots = steps.map((step, index) => {
    if (step.named_function) {
      const [binding] = step.pattern.parameters;
      const fn = namedDefinition(step.value.scope, binding, source);
      local.set(binding.port, fn.out);
      canvas.append(fn.box);
      movable(fn, step.value);
      return fn;
    }
    const rhs = expression(step.value, canvas, source);
    const binding = bindings[index];
    if (!binding) return rhs;
    describe(binding.inlet, step.value);
    binding.box.classList.add('binding-knot');
    markRecursive(binding.box, step.pattern.parameters.some(p => p.recursive));
    canvas.append(binding.box);
    // A bind is an ordinary rope; its inlet dot carries the >>= mark.
    if (step.kind !== 'let') binding.inlet.classList.add('bind-inlet');
    wires.push({ from: rhs.out, socket: binding.inlet, kind: step.kind === 'let' ? 'value' : 'bind' });
    return { box: binding.box, children: [rhs] };
  });
  roots.push(expression(expr.inputs[0], canvas, source));
  if (expr.kind === 'do') roots.bands = doBands(expr, steps, roots, bindings, canvas, local);
  return roots;
}

// A do block is a sequence: one horizontal band per statement, in source order,
// whether it desugars to >>= (a bind), >> (a discarded result) or a pure let.
// Nodes stay inside their statement's band; bands grow to fit their contents.
// A discarded result ends in a drop mark so it is never left dangling, and a
// bind whose names are never used is marked as kept only for its effect.
function doBands(expr, steps, roots, bindings, canvas, local) {
  const bands = [];
  const byStatement = new Map();
  steps.forEach((step, index) => {
    let band = byStatement.get(step.statement);
    if (!band) {
      band = { kind: step.kind, steps: [], ports: [], bobbins: [] };
      byStatement.set(step.statement, band);
      bands.push(band);
    }
    band.steps.push(step);
    roots[index].band = bands.indexOf(band);
    if (bindings[index]) band.bobbins.push(bindings[index].box);
    for (const p of step.pattern?.parameters || []) band.ports.push(local.get(p.port));
    if (step.kind === 'discard') {
      band.sink = el('span', 'drop-sink', '⊘');
      band.sink.setAttribute('role', 'img');
      band.sink.setAttribute('aria-label', 'Result discarded (>>)');
      band.sink.title = 'Result discarded: only the effect is sequenced (>>)';
      canvas.append(band.sink);
      wires.push({ from: roots[index].out, socket: band.sink, kind: 'discard' });
    }
  });
  roots.at(-1).band = bands.length;
  bands.push({ kind: 'result', steps: [], ports: [], bobbins: [] });
  const labels = { bind: '>>=', discard: '>>', let: 'let', result: '⇒' };
  const titles = {
    bind: 'Bind (>>=): run this effect and name its result for later statements',
    discard: 'Sequence (>>): run this effect and discard its result',
    let: 'Let: pure bindings, no effect to sequence',
    result: 'Result: the last statement is the value of the do block',
  };
  bands.forEach((band, index) => {
    band.element = el('div', 'do-band ' + band.kind);
    band.element.setAttribute('role', 'group');
    const gutter = el('div', 'band-gutter');
    const op = el('span', 'band-op', labels[band.kind]);
    gutter.title = titles[band.kind];
    gutter.append(op);
    const names = band.steps.map(step => step.pattern?.pattern).filter(Boolean);
    if (band.kind === 'bind' && names.length) gutter.append(el('span', 'band-pattern', names.join(', ')));
    band.element.setAttribute('aria-label', `Statement ${index + 1}: ${titles[band.kind].split(':')[0]}`);
    band.element.append(gutter);
  });
  // Keep the bands behind the nodes, in order.
  canvas.prepend(...bands.map(band => band.element));
  return bands;
}

const BAND = { gutter: 64, pad: 12, min: 48, sink: 34 };
function bandClamp([x, y]) {
  return [Math.max(BAND.gutter + BAND.pad, x), Math.max(BAND.pad, y)];
}

// Positions inside a band are relative to that band, so a growing band never
// pushes another statement's nodes into it.
function placeBands(frame, moving, dx = 0, dy = 0) {
  const heights = frame.bands.map(() => BAND.min);
  let right = BAND.gutter + BAND.pad;
  const positions = frame.nodes.map(item => {
    const [x, y] = bandClamp([item.at[0] + (item === moving ? dx : 0), item.at[1] + (item === moving ? dy : 0)]);
    heights[item.band] = Math.max(heights[item.band], y + item.box.offsetHeight + BAND.pad);
    right = Math.max(right, x + item.box.offsetWidth);
    return [x, y];
  });
  const sinks = frame.bands.some(band => band.sink);
  const width = right + (sinks ? BAND.sink + BAND.pad : BAND.pad);
  const tops = [];
  let top = frame.top;
  for (const height of heights) {
    tops.push(top);
    top += height;
  }
  frame.nodes.forEach((item, i) => {
    item.box.style.left = positions[i][0] + 'px';
    item.box.style.top = tops[item.band] + positions[i][1] + 'px';
  });
  frame.bands.forEach((band, i) => {
    Object.assign(band.element.style, { top: tops[i] + 'px', height: heights[i] + 'px', width: width + 'px' });
    if (band.sink) {
      band.sink.style.left = right + BAND.pad + 'px';
      band.sink.style.top = tops[i] + heights[i] / 2 + 'px';
    }
  });
  frame.canvas.style.width = width + 'px';
  frame.canvas.style.height = top + frame.padding[1] / 2 + 'px';
  return { left: 0, top: 0, right: width, bottom: top };
}

// Names bound but never used: the statement is kept only for its effect.
function markUnusedBindings() {
  for (const layout of layouts) {
    for (const band of layout.bands || []) {
      if (band.kind !== 'bind') continue;
      const used = band.ports.map(mark => connections.some(wire => wire.from === mark));
      band.ports.forEach((mark, i) => {
        const control = mark.closest('.port') || mark;
        control.classList.toggle('unused-name', !used[i]);
        if (!used[i]) control.title = 'Never used: this bind is kept only for its effect';
      });
      const idle = !used.some(Boolean);
      for (const pill of band.bobbins) {
        pill.classList.toggle('unused-binding', idle);
        if (idle) pill.title = band.ports.length ? 'Bound names are never used; only the effect is sequenced'
          : 'Wildcard bind: only the effect is sequenced';
      }
    }
  }
}

function alternatives(expr, canvas, sources) {
  const box = el('section', 'scope-box alternatives ' + expr.kind);
  box.setAttribute('aria-label', expr.kind + ' expression');
  const header = el('div', 'scope-header');
  const scrutinee = expr.inputs[0];
  const gate = endpoint('', 'condition-gate');
  const inner = describe(eyelet('condition-inner'), scrutinee);
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
      gate.mark = socket.querySelector('.eyelet');
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

// A pipeline supplies a value now. A composition has no value yet: its chain
// is itself the function node, with an open head and a definition port below.
// A composition chain composes its stages (∘) instead of applying them ($).
function slubChain(stages, canvas, sources, input, upstream, returnsFunction, composes = false, result = null) {
  const chain = el('div', 'slub-chain' + (composes ? ' compose' : ''));
  const callee = 'callee-inlet' + (composes ? ' compose-inlet' : '');
  const entry = describe(eyelet(), input);
  entry.classList.add('chain-head');
  if (!upstream) entry.classList.add('open-input');
  const exit = returnsFunction ? functionOutput() : eyelet();
  if (!returnsFunction) exit.classList.add('chain-tail');
  const children = [];
  chain.append(entry);
  for (const step of stages) {
    const pill = el('span', 'slub');
    markRecursive(pill, step.recursive);
    if (!pinned(step) && step.kind === 'reference') {
      const inlet = endpoint('', 'stage-function', callee);
      describe(inlet.mark, step);
      inlet.row.append(expressionName(el('span', 'slub-label', step.label), step));
      pill.append(inlet.row);
      wires.push({ from: captures => sources(step.parameter, captures), socket: inlet.mark, recursive: step.recursive, parameter: step.parameter });
    } else if (slubStage(step)) {
      pill.append(expressionName(el('span', 'slub-label', step.label), step));
      step.inputs.forEach((arg, i) => {
        if (inline(arg)) return pill.append(field(arg, step.label, i, step.labels?.[i] || '', sources));
        // An argument too large to sit in the pill keeps its own node and enters through an eyelet.
        const inlet = describe(eyelet('round slub-inlet'), arg);
        if (step.labels?.[i]) inlet.title = step.labels[i];
        pill.append(inlet);
        const child = expression(arg, canvas, sources);
        child.offRow = composes;
        children.push(child);
        wires.push({ from: child.out, socket: inlet });
      });
    } else {
      const fn = expression(step, canvas, sources);
      const inlet = endpoint(step.scope ? 'λ' : '', 'stage-function', callee);
      describe(inlet.mark, step);
      pill.append(inlet.row);
      children.push(fn);
      wires.push({ from: fn.out, socket: inlet.mark, recursive: step.recursive });
    }
    chain.append(pill);
  }
  if (result) {
    // A composition's result leaves when it is called, so its tail is open like its head.
    const tail = describe(eyelet(), result);
    tail.classList.add('chain-tail', 'open-input');
    chain.append(tail);
  }
  chain.append(exit);
  if (upstream) wires.push({ from: upstream, socket: entry });
  canvas.append(chain);
  return { box: chain, out: exit, children, definition: returnsFunction };
}

// A call can sit on a chain as a pill: simple arguments show inline, the rest
// enter through eyelets.
function slubStage(expr) {
  return !pinned(expr) && ['call', 'value'].includes(expr.kind);
}

function compositionStages(expr) {
  const stages = [];
  function flatten(stage) {
    if (stage.kind === 'composition' && !pinned(stage)) stage.inputs.forEach(flatten);
    else stages.push(stage);
  }
  expr.inputs.forEach(flatten);
  return stages;
}

// A position on a stage that would otherwise be a pill takes it off the chain,
// so the compositions around it fall back to ordinary operator nodes.
const breaksSlub = (stage) => pinned(stage) && ['reference', 'call', 'value'].includes(stage.kind);

function composition(expr, canvas, sources) {
  const input = { implicit: true, label: 'Composition input', ty: expr.open_inputs[0] };
  const result = { label: 'Composition result', ty: expr.composition_result };
  return slubChain(compositionStages(expr), canvas, sources, input, null, true, true, result);
}

// `a + b + c` needs no parentheses, so it is one knot with three operands and
// not a knot per `+`. The side that chains without parentheses gives the
// associativity: the left for `+`, the right for `:`.
function operatorChain(expr) {
  const chains = (child) => child.kind === 'operator' && child.label === expr.label && !child.grouped
    && !pinned(child) && child.callee_port === undefined;
  if (expr.kind !== 'operator' || expr.callee_port !== undefined) return expr.inputs;
  const operands = [];
  let [left, right] = expr.inputs;
  if (chains(left)) {
    for (operands.unshift(right); chains(left); [left, right] = left.inputs) operands.unshift(left.inputs[1]);
    operands.unshift(left);
  } else {
    for (operands.push(left); chains(right); [left, right] = right.inputs) operands.push(right.inputs[0]);
    operands.push(right);
  }
  return operands;
}

function makeExpression(expr, canvas, sources, forwarded = []) {
  // Only stages with inline captures can sit on a rope. More complex
  // right-hand expressions retain explicit inputs and their own ropes.
  if (expr.kind === 'composition' && !compositionStages(expr).some(breaksSlub)) return composition(expr, canvas, sources);
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
    if (!slubStage(rhs)) break;
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
  markRecursive(knot, base.recursive);
  const item = { box: knot, children: [] };
  canvas.append(knot);
  let label = base.label;
  switch (base.kind) {
    case 'field-access': label = '.' + label; break;
    case 'callsite': label = base.inputs[0].kind === 'reference' ? base.inputs[0].label : '<inline>'; break;
    case 'source': label = 'Source'; break;
  }
  const title = expressionName(el('div', 'knot-label', label), base);
  knot.append(title);
  let args = operatorChain(base);
  if (base.kind === 'callsite') {
    const [callee, ...operands] = args;
    args = operands;
    if (callee.kind === 'reference' && !pinned(callee)) {
      const socket = field(callee, label, 0, '', sources, binding => {
        markRecursive(knot, callee.recursive && binding.port === callee.parameter);
        title.textContent = binding.name;
        functionName(title, binding.port === callee.parameter ? base.callable : binding.callable);
      });
      socket.classList.add('callee-socket');
      socket.querySelector('.eyelet').classList.add('callee-inlet');
      knot.append(socket);
    } else {
      const fn = expression(callee, canvas, sources);
      item.children.push(fn);
      const inlet = describe(eyelet('callee-inlet'), callee);
      knot.append(inlet);
      wires.push({ from: fn.out, socket: inlet, recursive: callee.recursive });
    }
  } else if (base.callee_port !== undefined) {
    const inlet = describe(eyelet('callee-inlet'), { ty: base.callable.at_site, label });
    knot.append(inlet);
    wires.push({ from: captures => sources(base.callee_port, captures), socket: inlet, recursive: base.recursive, parameter: base.callee_port });
  }
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
    args.forEach((arg, i) =>
      add(arg, i, base.labels?.[i] || (base.kind === 'list' ? String(i + 1)
        // A chained operator repeats on each later operand: a, + b, + c.
        : base.kind === 'operator' && args.length > 2 && i ? base.label : '')),
    );
  for (const [i, { argument, from }] of forwarded.entries()) {
    const inlet = endpoint(base.labels?.[args.length + i] || '', 'left');
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
      const unused = endpoint(base.labels?.[args.length + i] || '', 'left');
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
    if (!e.isPrimary || e.button !== 0 || busy || e.target.closest('input, .socket, .port, .eyelet, .capture')) return;
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
      let xy = offset(e).map((delta, i) => Math.round(drag.at[i] + delta));
      if (item.frame?.clamp) xy = item.frame.clamp(xy);
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
      commitEmbedded();
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
    const direct = ['call', 'value', 'callsite', 'operator'].includes(fn.kind);
    const call = direct ? { ...fn, kind: fn.kind === 'value' ? 'call' : fn.kind }
      : { kind: 'callsite', label: '<inline>', inputs: [fn], callable: fn.callable };
    call.ty = result.ty;
    call.open_inputs = undefined;
    const forwarded = implicit.map((argument, i) => ({ argument, from: patterns[scope.inputs.length + i].out }));
    const item = makeExpression(call, canvas, source, forwarded);
    if (direct) movable(item, fn);
    describe(item.out, result);
    return item;
  });
}

function markRecursive(element, recursive) {
  element.classList.toggle('recursive-knot', Boolean(recursive));
  if (recursive) element.setAttribute('aria-description', 'Recursive');
  else element.removeAttribute('aria-description');
}

function namedDefinition(scope, binding, outerSource) {
  ports.set(binding.port, binding);
  const item = definitionBox(scope, binding.name, outerSource);
  markRecursive(item.box, binding.recursive);
  functionName(item.box.querySelector('.box-toggle'), binding.callable);
  describe(item.out, { ty: binding.ty, label: binding.name });
  bindOutput(binding, item.out);
  item.bindings.set(binding.port, item.out);
  return item;
}

function render() {
  inspect(null);
  const board = $('board');
  board.replaceChildren($('wires'));
  layouts = [];
  wires = [];
  ports = new Map();
  selected = null;
  rootItem = graph.definition_port === undefined ? definitionBox(graph.scope, graph.definition)
    : namedDefinition(graph.scope, { name: graph.definition, port: graph.definition_port, ty: graph.ty, callable: graph.callable, recursive: graph.recursive });
  markRecursive(rootItem.box, graph.recursive);
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
  // A shared capture segment carries the tint if any use downstream closes the cycle.
  for (const wire of ordinary) {
    if (!wire.recursive) continue;
    for (let segment = captures.get(wire.from); segment && !segment.recursive; segment = captures.get(segment.from)) {
      segment.recursive = true;
    }
  }
  for (const anchor of board.querySelectorAll('.capture'))
    anchor.hidden = !captures.has(anchor.querySelector('.eyelet'));
  const captureSources = new Set([...captures.values()].map(w => w.from.closest('.inputs-boundary')));
  for (const layout of layouts) arrange(layout, captureSources);
  rootItem.auto = [24, 24];
  rootItem.at = rootItem.position ?? rootItem.auto;
  rootItem.frame.bounds = placeNodes(rootItem.frame);
  board.style.translate = '';
  connections = [...captures.values(), ...ordinary];
  markUnusedBindings();
  const w = board.offsetWidth,
    h = board.offsetHeight;
  const scale = zoom ?? (fitted
    ? Math.min(1, $('viewport').clientWidth / w, $('viewport').clientHeight / h)
    : 1);
  sizeViewport(scale);
  $('fit').textContent = fitted && zoom === null ? 'Actual size' : 'Fit graph';
  drawWires(connections);
}

// Wheel zoom keeps the board point under the cursor fixed; it overrides Fit until
// Fit graph is chosen again. The middle button drags the view.
let zoom = null, panning = null;
function zoomAt(factor, clientX, clientY) {
  const board = $('board'), viewport = $('viewport');
  if (!board.offsetWidth) return;
  const before = board.getBoundingClientRect().width / board.offsetWidth;
  const next = Math.min(4, Math.max(0.1, before * factor));
  if (next === before) return;
  const box = viewport.getBoundingClientRect();
  const x = clientX - box.left, y = clientY - box.top;
  const pointX = (viewport.scrollLeft + x) / before, pointY = (viewport.scrollTop + y) / before;
  zoom = next;
  sizeViewport(next);
  viewport.scrollLeft = pointX * next - x;
  viewport.scrollTop = pointY * next - y;
  $('fit').textContent = 'Fit graph';
  inspect(null);
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
    if (start.classList.contains('definition-port')) exit = [x, y + Math.min(24, verticalBend)];
    else if (start.classList.contains('capture-triangle')) exit = [x, y + verticalBend];
    const entersCall = wire.socket.classList.contains('callee-inlet');
    const entersBottom = wire.socket.classList.contains('definition-port');
    // A pill's argument eyelet sits mid-chain, so its rope arrives vertically from its node's side.
    const entersPill = wire.socket.classList.contains('slub-inlet');
    const entry = entersPill ? [u, v + (y < v ? -1 : 1) * Math.max(14, verticalBend)]
      : entersCapture || entersBottom || entersCall ? [u, v - verticalBend] : [u - bend, v];
    let d = `M ${x} ${y} C ${exit.join(' ')} ${entry.join(' ')} ${u} ${v}`;
    const owner = start.matches('.scope-box > .definition-port[role=button]') && start.parentElement;
    if (owner && owner.contains(wire.socket)) {
      // Leave the bottom diamond, then return above the target through the right gutter.
      const target = wire.socket.closest('.scope-box, .knot, .slub');
      const right = Math.max(x, u, (target.getBoundingClientRect().right - origin.left) / scale) + 12;
      const bottom = y + 12, top = v - 14, r = 6;
      d = `M ${x} ${y} Q ${x} ${bottom} ${x+r} ${bottom} H ${right-r}
        Q ${right} ${bottom} ${right} ${bottom-r} V ${top+r}
        Q ${right} ${top} ${right-r} ${top} H ${u+r} Q ${u} ${top} ${u} ${v}`;
    }
    if (wire.kind === 'pattern') {
      const trunk = (wire.socket.closest('.arms').getBoundingClientRect().left - board.left) / scale + 7;
      d = `M ${x} ${y} V ${y + 11} H ${trunk} V ${v} H ${u}`;
    }
    if (wire.kind) path.classList.add('rope-' + wire.kind);
    path.setAttribute('d', d);
    const parameter = wire.control ? Number(wire.control.dataset.parameter) : wire.parameter;
    if (parameter !== undefined) path.classList.add(selected === parameter ? 'lit-wire' : 'parameter-wire');
    if (entersCapture || start.classList.contains('capture-triangle')) path.classList.add('rope-capture');
    if (wire.recursive) path.classList.add('rope-recursive');
    if (entersCall) path.setAttribute('marker-end', 'url(#call-tip)');
    if (start.classList.contains('definition-port') && parameter !== undefined) path.classList.add('definition-rope');
    layer.append(path);
    wire.path = path;
    if (touchesInspected(wire)) path.classList.add('inspected-wire');
  }
}

async function request(url, body) {
  const response = await (host ? host.request : fetch)(
    url,
    body === undefined
      ? {}
      : {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        },
  );
  if (response.ok === false) throw Error('Request failed (' + response.status + ').');
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

// Story source is editable: each pause re-checks the story module with the edit
// spliced in and redraws its graph. Nothing is saved; Reset or reload restores it.
let storyOriginal = null, storyTimer, storySequence = 0, storyWorker, storySerial = 0;
const storyPending = new Map();
function checkStory(body) {
  const runtime = document.documentElement.dataset.runtime;
  if (!runtime) {
    return fetch('/api/tangle/story', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body) }).then(r => r.json());
  }
  // Static Pages: the drawing runtime checks stories in a worker, without a drawing.
  if (!storyWorker) {
    storyWorker = new Worker(new URL(runtime, location.href), { type: 'module' });
    storyWorker.onmessage = ({ data: { id, value, error } }) => {
      const call = storyPending.get(id);
      if (!call) return;
      storyPending.delete(id);
      if (error) call.reject(Error(error)); else call.resolve(value);
    };
    storyWorker.onerror = e => {
      for (const call of storyPending.values()) call.reject(Error(e.message || 'Story runtime failed to load.'));
      storyPending.clear();
    };
  }
  return new Promise((resolve, reject) => {
    const id = ++storySerial;
    storyPending.set(id, { resolve, reject });
    storyWorker.postMessage({ id, demo: null,
      request: { url: '/api/tangle/story', method: 'POST', body: JSON.stringify(body) } });
  });
}
function storyStatus(message, error = false) {
  status(message, error);
  $('reload').hidden = true;
}
function showStory(next) {
  graph = next;
  sourceBytes = new TextEncoder().encode(graph.source);
  render();
}
function editStory(text) {
  $('reset-story').hidden = text === storyOriginal.graph.source;
  clearTimeout(storyTimer);
  storyStatus('Checking…');
  storyTimer = setTimeout(async () => {
    const sequence = ++storySequence;
    try {
      const result = await checkStory({ definition: storyOriginal.graph.definition, source: text });
      if (sequence !== storySequence) return;
      if (!result.ok) {
        sourceView.markers(result.line ? [{ line: result.line, column: result.column, message: result.reason }] : []);
        storyStatus(result.reason.replace(/^.*?tangle-stories\.knot:\s?/, 'Line '), true);
        return;
      }
      sourceView.markers([]);
      showStory(result.graph);
      storyStatus($('reset-story').hidden ? 'Hover an eyelet to inspect its type and ropes.'
        : 'Story edited. Reset story or reload to restore the original.');
    } catch (error) {
      if (sequence === storySequence) storyStatus(error.message, true);
    }
  }, 300);
}
$('reset-story').onclick = () => {
  clearTimeout(storyTimer);
  storySequence++;
  sourceView?.set(storyOriginal.graph.source);
  sourceView?.markers([]);
  $('reset-story').hidden = true;
  showStory(storyOriginal.graph);
  hint();
};

// Saved source in Monaco; plain text until it loads or if it is unavailable.
let sourceView, sourceLoading, shownSource = '';
function showSource(text) {
  shownSource = text;
  if (sourceView) return sourceView.set(text);
  $('source-text').textContent = text;
  if (host || sourceLoading) return;
  const url = new URL('editor.js', new URL(document.documentElement.dataset.monaco || '/source-editor/', location.href));
  const css = document.createElement('link');
  css.rel = 'stylesheet';
  css.href = new URL('editor.css', url);
  document.head.append(css);
  sourceLoading = import(url.href).then(({ create }) => {
    $('source-text').remove();
    sourceView = create($('source'), shownSource, storyMode ? editStory : null, { readOnly: !storyMode, fit: true });
  }).catch(() => {});
}

async function load() {
  const query = new URLSearchParams();
  if (host) query.set('embed', '1');
  if (definition) query.set('definition', definition);
  const result = storyMode ? await loadStory() : await request('/api/tangle' + (query.size ? '?' + query : ''));
  if (storyMode) {
    storyOriginal = result;
    $('source-heading').textContent = 'Story source';
  }
  if (graph?.definition !== result.graph.definition) zoom = null;
  graph = result.graph;
  if (host) {
    definition = graph.definition;
    const picker = $('choose-definition');
    picker.replaceChildren(...result.definitions.map(name => new Option(name, name)));
    picker.value = definition;
  }
  sourceBytes = new TextEncoder().encode(graph.source);
  canUndo = result.canUndo;
  changes.clear();
  $('module').textContent = storyMode ? 'Syntax storybook' : graph.module;
  $('definition').textContent = graph.definition;
  showSource(graph.source);
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

let refreshPending = false;
function staleGraph() {
  const revision = host.revision();
  return revision.error || graph?.snapshot !== revision.snapshot;
}
function refreshDrawingGraph() {
  if (!host) return;
  if (busy) { refreshPending = true; return; }
  const revision = host.revision();
  canUndo = revision.canUndo;
  updateButtons();
  if (revision.error) { status(revision.error, true); return; }
  if (!revision.snapshot) return;
  if (staleGraph()) {
    if (changes.size) hint();
    else run(load);
  } else {
    resize();
    if ($('status').classList.contains('error')) hint();
  }
}
async function run(action) {
  if (busy) return;
  busy = true;
  $('board').inert = true;
  updateButtons();
  try {
    await action();
  } catch (error) {
    status(error.message, true);
    host?.log?.(error.message, true);
  } finally {
    busy = false;
    $('board').inert = false;
    updateButtons();
    if (refreshPending) { refreshPending = false; refreshDrawingGraph(); }
  }
}

function commitEmbedded() {
  if (host && changes.size && !staleGraph()) run(saveChanges);
}
async function saveChanges() {
  await request('/api/tangle', {
    snapshot: graph.snapshot,
    definition: graph.definition,
    changes: [...changes.values()],
  });
  if (host) await host.reload();
  await load();
  status('Saved.');
  host?.log?.('Saved Tangle edit.');
}
$('save').onclick = () => run(saveChanges);
$('undo').onclick = () =>
  run(async () => {
    if (changes.size) {
      await load();
      status('Unsaved edits discarded.');
      return;
    }
    await request('/api/undo', { snapshot: graph.snapshot });
    if (host) await host.reload();
    await load();
    status('Edit undone.');
  });
$('fit').onclick = () => {
  fitted = zoom === null ? !fitted : true;
  zoom = null;
  resize();
};
$('viewport').addEventListener('wheel', e => {
  e.preventDefault();
  // Trackpad pinches arrive as ctrl+wheel with small deltas.
  const delta = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY;
  zoomAt(Math.exp(-delta * (e.ctrlKey ? 0.01 : 0.0015)), e.clientX, e.clientY);
}, { passive: false });
$('viewport').addEventListener('pointerdown', e => {
  if (e.button !== 1) return;
  e.preventDefault();
  const viewport = $('viewport');
  viewport.setPointerCapture(e.pointerId);
  viewport.classList.add('panning');
  panning = { id: e.pointerId, x: e.clientX, y: e.clientY, left: viewport.scrollLeft, top: viewport.scrollTop };
});
$('viewport').addEventListener('pointermove', e => {
  if (panning?.id !== e.pointerId) return;
  $('viewport').scrollLeft = panning.left - (e.clientX - panning.x);
  $('viewport').scrollTop = panning.top - (e.clientY - panning.y);
});
const endPan = () => { panning = null; $('viewport').classList.remove('panning'); };
$('viewport').addEventListener('pointerup', e => { if (panning?.id === e.pointerId) endPan(); });
$('viewport').addEventListener('lostpointercapture', endPan);
// Keep the middle button from starting the browser's autoscroll.
$('viewport').addEventListener('mousedown', e => { if (e.button === 1) e.preventDefault(); });
$('reload').onclick = () => run(load);
$('board').addEventListener('pointerover', e => {
  if (!e.buttons && e.pointerType !== 'touch') inspect(eyeletAt(e.target));
});
$('board').addEventListener('pointerout', e => {
  if (!e.relatedTarget || !$('board').contains(e.relatedTarget)) inspect(null);
});
$('board').addEventListener('focusin', e => inspect(eyeletAt(e.target)));
$('board').addEventListener('focusout', () => inspect(null));
$('board').addEventListener('pointerdown', () => inspect(null));
$('viewport').addEventListener('scroll', () => inspect(null));
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') inspect(null);
});
window.addEventListener('resize', resize);

// Drag the divider beside the source pane to resize it; the width is remembered,
// arrow keys nudge it and a double-click restores the default.
(() => {
  const handle = $('aside-handle'), main = document.querySelector('main'), aside = document.querySelector('aside');
  const KEY = 'tangle.asideWidth', MIN_ASIDE = 200, MIN_GRAPH = 320;
  const stored = () => { try { return JSON.parse(localStorage.getItem(KEY)); } catch { return null; } };
  const store = width => { try { localStorage.setItem(KEY, JSON.stringify(width)); } catch { /* not remembered */ } };
  const place = () => { handle.style.left = aside.offsetLeft - 4 + 'px'; };
  let width = null, drag = null;
  const apply = next => {
    if (next === null) main.style.removeProperty('--aside-width');
    else {
      const nav = $('stories').hidden ? 0 : $('stories').offsetWidth;
      const max = Math.max(MIN_ASIDE, main.clientWidth - nav - MIN_GRAPH);
      next = Math.round(Math.min(Math.max(next, MIN_ASIDE), max));
      main.style.setProperty('--aside-width', next + 'px');
    }
    width = next;
    place();
  };
  const commit = () => { store(width); resize(); };
  apply(stored());
  // The workspace changes size when the story list appears, moving the aside.
  const observer = new ResizeObserver(place);
  observer.observe(aside);
  observer.observe(document.querySelector('.workspace'));
  addEventListener('resize', () => { if (width !== null) apply(width); else place(); });
  handle.addEventListener('pointerdown', e => {
    if (e.button !== 0) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    handle.classList.add('dragging');
    drag = { x: e.clientX, width: aside.offsetWidth };
  });
  handle.addEventListener('pointermove', e => { if (drag) apply(drag.width + drag.x - e.clientX); });
  const end = () => {
    if (!drag) return;
    drag = null;
    handle.classList.remove('dragging');
    commit();
  };
  handle.addEventListener('pointerup', end);
  handle.addEventListener('lostpointercapture', end);
  handle.addEventListener('dblclick', () => { apply(null); commit(); });
  handle.addEventListener('keydown', e => {
    const step = e.shiftKey ? 80 : 20;
    const delta = { ArrowLeft: step, ArrowRight: -step }[e.key];
    if (delta === undefined) return;
    e.preventDefault();
    apply(aside.offsetWidth + delta);
    commit();
  });
})();
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
if (host) {
  document.body.classList.add('embedded');
  $('reload').textContent = 'Discard edits';
  $('choose-definition').hidden = false;
  $('choose-definition').onchange = event => {
    definition = event.target.value;
    run(load);
  };
  refreshDrawingGraph();
} else {
  run(load);
}
