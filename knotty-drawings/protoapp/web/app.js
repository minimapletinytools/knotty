// Knotty Drawings prototype viewer.
// The server evaluates the drawing and returns its scene tree (JSON) and SVG,
// with a data-key on every node. A key is the path of child indices from the
// root, e.g. "2.1"; the root is "".
"use strict";

const drawingRequest = typeof browserDrawing === "function" ? browserDrawing : window.fetch.bind(window);
const $ = (id) => document.getElementById(id);

const state = {
  revision: null,
  snapshot: null,
  loadSequence: 0,
  whips: [],
  whipsEditable: false,
  scene: null,
  error: null,
  selected: null, // key of the selected node, or null
  expanded: new Set([""]),
  view: { x: 0, y: 0, scale: 1 },
  fitted: false,
  geometry: null,
  editability: null, // { key, fields: { name: { editable, reason } } }
};

let paintPicker = null;
window.knotPaintProfile = [];

const storage = {
  get(name, fallback) {
    try {
      const value = localStorage.getItem("knotty." + name);
      return value === null ? fallback : JSON.parse(value);
    } catch {
      return fallback;
    }
  },
  set(name, value) {
    try {
      localStorage.setItem("knotty." + name, JSON.stringify(value));
    } catch {
      /* storage unavailable: preferences just aren't remembered */
    }
  },
};

// Loading ------------------------------------------------------------------------

async function load() {
  if (!paintPicker) await recoverPaintPreview();
  const sequence = ++state.loadSequence;
  const response = await drawingRequest("/api/scene");
  const data = await response.json();
  if (sequence !== state.loadSequence) return;
  return displayScene(data);
}

async function displayScene(data) {
  function expand(node) {
    if (!data.editabilitySets) return;
    node.editability = data.editabilitySets[node.editabilityIndex];
    delete node.editabilityIndex;
    node.children?.forEach(expand);
  }
  if (data.scenePatch) {
    const patch = data.scenePatch;
    data.scene = patch.base === state.snapshot && state.scene
      ? SceneReconcile.children($("stage"), state.scene, patch.children, expand) : null;
    if (!data.scene) {
      const sequence = ++state.loadSequence;
      const full = await (await drawingRequest("/api/scene")).json();
      if (sequence === state.loadSequence) return displayScene(full);
      return;
    }
  }
  if (circleTool && !circleTool.committing && circleTool.snapshot!==data.snapshot) stopCircle();
  KnotSource.update(data.source ?? "");
  state.sourceRootKey=data.sourceRootKey || "";
  if (state.snapshot !== data.snapshot || data.error) geometryCache.clear();
  state.snapshot = data.snapshot;
  state.whips = data.whips || [];
  state.whipsEditable = data.whipsEditable;
  state.editability = null;
  state.revision = data.revision;
  state.error = data.error;
  $("file-name").textContent = data.file.split("/").pop();
  $("file-name").title = data.file;
  $("select-curve").hidden = !data.scene?.children?.some(n => n.id === "snake-curve");
  showError(data.error);
  if (!data.scene) return;
  if (!data.scenePatch) expand(data.scene);
  state.scene = data.scene;
  state.editability = selectedEditability();
  if (!data.scenePatch) SceneReconcile.svg($("stage"), data.svg);
  if (state.selected !== null && !findNode(state.selected)) select(null);
  if (!state.fitted) {
    fit();
    state.fitted = true;
  }
  applyView();
  renderTree();
  renderProperties();
  if (!geometryDrag) scheduleGeometryCheck();
}

function showError(error) {
  const status = $("file-status");
  $("banner").hidden = !error;
  $("banner-detail").textContent = error || "";
  status.textContent = error ? "Error" : "Up to date";
  status.classList.toggle("error", Boolean(error));
}

async function poll() {
  try {
    if (paintPicker || circleTool) { setTimeout(poll, 1000); return; }
    const response = await drawingRequest("/api/revision");
    const data = await response.json();
    if (data.revision !== state.revision || data.error !== state.error) await load();
  } catch {
    $("file-status").textContent = "Not connected to knottydrawings";
    $("file-status").classList.add("error");
  }
  setTimeout(poll, 1000);
}

function findNode(key) {
  let node = state.scene;
  if (!node) return null;
  if (key === "") return node;
  for (const part of key.split(".")) {
    node = node.children[Number(part)];
    if (!node) return null;
  }
  return node;
}

function ancestors(key) {
  const parts = key === "" ? [] : key.split(".");
  const result = [""];
  for (let i = 1; i < parts.length; i++) result.push(parts.slice(0, i).join("."));
  return result;
}

// Selection ------------------------------------------------------------------------

function select(key) {
  if (paintPicker || circleTool) return;
  state.geometry = null;
  state.selected = key;
  state.editability = selectedEditability();
  if (key !== null) ancestors(key).forEach((k) => state.expanded.add(k));
  renderTree();
  renderProperties();
  updateBoxes();
  if (key !== null) {
    const row = document.querySelector(`.tree-row[data-key="${key}"]`);
    if (row) row.scrollIntoView({ block: "nearest" });
  }
  scheduleGeometryCheck();
}

function elementFor(key) {
  return $("stage").querySelector(`[data-key="${key}"]`);
}

// Layers tree ------------------------------------------------------------------------

const GLYPHS = {
  svg: "▭", group: "▤", rect: "□", circle: "○", ellipse: "⬭", line: "╱",
  polyline: "⌇", polygon: "⬠", path: "∿", text: "T",
};

function renderTree() {
  const tree = $("tree");
  tree.replaceChildren();
  if (state.scene) tree.append(treeItem(state.scene, 0));
}

function treeItem(node, depth) {
  const item = document.createElement("li");
  item.setAttribute("role", "treeitem");
  const hasChildren = node.children.length > 0;
  const open = hasChildren && state.expanded.has(node.key);
  if (hasChildren) item.setAttribute("aria-expanded", String(open));

  const row = document.createElement("div");
  row.className = "tree-row" + (node.key === state.selected ? " selected" : "");
  row.dataset.key = node.key;
  row.style.paddingLeft = 6 + depth * 14 + "px";

  const disclosure = document.createElement("button");
  disclosure.className = "disclosure" + (hasChildren ? "" : " leaf");
  disclosure.setAttribute("aria-expanded", String(open));
  disclosure.setAttribute("aria-label", open ? "Collapse" : "Expand");
  disclosure.tabIndex = -1;
  disclosure.addEventListener("click", (event) => {
    event.stopPropagation();
    if (state.expanded.has(node.key)) state.expanded.delete(node.key);
    else state.expanded.add(node.key);
    renderTree();
  });

  const glyph = document.createElement("span");
  glyph.className = "kind-glyph";
  glyph.textContent = GLYPHS[node.kind] || "·";

  const label = document.createElement("span");
  label.className = "tree-label";
  label.textContent = node.kind;
  if (node.id) {
    const id = document.createElement("span");
    id.className = "tree-id";
    id.textContent = " " + node.id;
    label.append(id);
  }
  if (hasChildren) label.append(` (${node.children.length})`);

  const key = document.createElement("span");
  key.className = "tree-key";
  key.textContent = node.key;

  row.append(disclosure, glyph, label, key);
  row.addEventListener("click", () => select(node.key));
  row.addEventListener("mouseenter", () => showHover(node.key));
  row.addEventListener("mouseleave", () => showHover(null));
  item.append(row);

  if (open) {
    const list = document.createElement("ul");
    list.setAttribute("role", "group");
    for (const child of node.children) list.append(treeItem(child, depth + 1));
    item.append(list);
  }
  return item;
}

// Canvas: zoom, pan, pick ------------------------------------------------------------

function applyView() {
  const { x, y, scale } = state.view;
  $("stage").style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
  $("zoom-level").textContent = Math.round(scale * 100) + "%";
  updateBoxes();
}

function drawingSize() {
  const svg = $("stage").querySelector("svg");
  if (!svg) return { width: 1, height: 1 };
  return { width: svg.width.baseVal.value || 1, height: svg.height.baseVal.value || 1 };
}

function fit() {
  const canvas = $("canvas").getBoundingClientRect();
  const { width, height } = drawingSize();
  const margin = 48;
  const scale = Math.max(0.01, Math.min((canvas.width - margin) / width, (canvas.height - margin) / height));
  state.view = {
    scale,
    x: (canvas.width - width * scale) / 2,
    y: (canvas.height - height * scale) / 2,
  };
  applyView();
}

function actualSize() {
  const canvas = $("canvas").getBoundingClientRect();
  zoomAt(canvas.width / 2, canvas.height / 2, 1 / state.view.scale);
}

function zoomAt(px, py, factor) {
  const view = state.view;
  const scale = Math.min(64, Math.max(0.02, view.scale * factor));
  const applied = scale / view.scale;
  view.x = px - (px - view.x) * applied;
  view.y = py - (py - view.y) * applied;
  view.scale = scale;
  applyView();
}

let spaceHeld = false;
let pan = null; // { x, y, viewX, viewY, moved }

function setupCanvas() {
  const canvas = $("canvas");

  canvas.addEventListener("wheel", (event) => {
    event.preventDefault();
    if (geometryDrag || circleTool?.gesture || circleTool?.committing) return;
    const rect = canvas.getBoundingClientRect();
    // Pinch gestures arrive as ctrl+wheel with small deltas.
    const speed = event.ctrlKey ? 0.01 : 0.0015;
    const delta = event.deltaMode === 1 ? event.deltaY * 16 : event.deltaY;
    zoomAt(event.clientX - rect.left, event.clientY - rect.top, Math.exp(-delta * speed));
  }, { passive: false });

  canvas.addEventListener("pointerdown", (event) => {
    suppressCanvasClick=false;
    if (paintPicker || circleTool?.committing) return;
    if (circleTool && event.button===0 && !spaceHeld) {startCircle(event);return;}
    const handle=event.target.closest?.("[data-geometry]")?.getAttribute("data-geometry");
    if (handle && event.button===0 && !spaceHeld) { startGeometryDrag(event,handle); return; }
    if (event.button === 1 || (event.button === 0 && spaceHeld)) {
      event.preventDefault();
      canvas.setPointerCapture(event.pointerId);
      pan = { x: event.clientX, y: event.clientY, viewX: state.view.x, viewY: state.view.y };
      canvas.classList.add("panning");
    }
  });

  canvas.addEventListener("pointermove", (event) => {
    if (circleTool?.gesture) {moveCircle(event);return;}
    if (geometryDrag) { moveGeometryDrag(event); return; }
    if (pan) {
      state.view.x = pan.viewX + event.clientX - pan.x;
      state.view.y = pan.viewY + event.clientY - pan.y;
      applyView();
      return;
    }
    showHover(keyAt(event.target));
  });

  const endPan = () => {
    pan = null;
    canvas.classList.remove("panning");
  };
  canvas.addEventListener("pointerup", event => {
    if (circleTool?.gesture) {finishCircle(event);return;}
    if (geometryDrag && event.pointerId===geometryDrag.pointerId) { moveGeometryDrag(event); finishGeometryDrag(true); } else endPan();
  });
  canvas.addEventListener("pointercancel", () => { if (circleTool?.gesture) stopCircle(); if (geometryDrag) finishGeometryDrag(false); endPan(); });
  canvas.addEventListener("lostpointercapture", () => { if (circleTool?.gesture) stopCircle(); if (geometryDrag && !geometryDrag.ended) finishGeometryDrag(false); });
  canvas.addEventListener("pointerleave", () => showHover(null));

  canvas.addEventListener("click", (event) => {
    if (suppressCanvasClick) { suppressCanvasClick=false; return; }
    if (event.button !== 0 || spaceHeld || paintPicker || circleTool) return;
    select(keyAt(event.target));
  });

  // Keep the middle button from starting the browser's autoscroll.
  canvas.addEventListener("mousedown", (event) => {
    if (event.button === 1) event.preventDefault();
  });

  new ResizeObserver(() => { if (circleTool?.gesture) stopCircle(); if (geometryDrag && !geometryDrag.ended) finishGeometryDrag(false); updateBoxes(); }).observe(canvas);
}

// The deepest keyed node under the pointer; clicking the bare page selects nothing.
function keyAt(target) {
  const element = target.closest && target.closest("[data-key]");
  if (!element || !$("stage").contains(element)) return null;
  const key = element.getAttribute("data-key");
  return key === "" ? null : key;
}

function placeBox(box, key) {
  const element = key === null ? null : elementFor(key);
  if (!element) {
    box.hidden = true;
    return;
  }
  const canvas = $("canvas").getBoundingClientRect();
  const rect = element.getBoundingClientRect();
  box.hidden = false;
  box.style.left = rect.left - canvas.left + "px";
  box.style.top = rect.top - canvas.top + "px";
  box.style.width = Math.max(rect.width, 1) + "px";
  box.style.height = Math.max(rect.height, 1) + "px";
}

let hovered = null;
function showHover(key) {
  hovered = key === state.selected ? null : key;
  placeBox($("hover-box"), hovered);
}

function updateBoxes() {
  placeBox($("selection-box"), state.selected);
  placeBox($("hover-box"), hovered);
  renderGeometryWidget();
}

// Circle creation -----------------------------------------------------------------
// The gesture is local SVG only. Release sends one request through the selected
// output's unravel, which owns insertion policy and validates the source edit.
let circleTool = null;
function circleStatus(message, error=false) {
  $("creation-status").textContent=message;
  $("creation-status").classList.toggle("error",error);
}
function stopCircle() {
  if (circleTool?.committing) return;
  const gesture=circleTool?.gesture;
  circleTool=null;
  $("creation-overlay").replaceChildren();
  $("add-circle").setAttribute("aria-pressed","false");
  $("canvas").classList.remove("creating-circle");
  $("properties").inert=false;
  $("tree").inert=false;
  $("undo-edit").disabled=false;
  if (gesture && $("canvas").hasPointerCapture(gesture.pointerId)) $("canvas").releasePointerCapture(gesture.pointerId);
  circleStatus("");
  scheduleGeometryCheck();
}
async function activateCircle() {
  if (circleTool) {stopCircle();return;}
  if (paintPicker || geometryDrag || !state.scene || state.error) return;
  const selected=state.selected===null?null:findNode(state.selected);
  const parent=selected && ["svg","group"].includes(selected.kind)?state.selected:
    selected?state.selected.split(".").slice(0,-1).join("."):state.sourceRootKey;
  const tool={parent,snapshot:state.snapshot,ready:false,gesture:null,committing:false};
  circleTool=tool;
  clearTimeout(geometryTimer); ++geometrySequence; cancelGeometryCheck();
  state.geometry=null; renderGeometryWidget();
  $("add-circle").setAttribute("aria-pressed","true");
  $("properties").inert=true; $("tree").inert=true; $("undo-edit").disabled=true;
  const label=parent===""?"root":`container ${parent}`;
  tool.label=label;
  circleStatus(`Checking circle creation in ${label}…`);
  try {
    const result=await (await drawingRequest("/api/create/circle/check",{method:"POST",headers:{"Content-Type":"application/json"},
      body:JSON.stringify({snapshot:tool.snapshot,parent,cx:0,cy:0,r:10})})).json();
    if (circleTool!==tool) return;
    if (!result.ok) throw Error(result.reason);
    if (state.snapshot!==tool.snapshot) throw Error("The drawing changed. Choose the Circle tool again.");
    tool.ready=true;
    $("canvas").classList.add("creating-circle");
    circleStatus(`Circle → ${label}. Drag from the center; click for radius 10. Escape cancels.`);
  } catch(error) {
    if (circleTool===tool) circleStatus(`${label}: ${error.message} (Escape to exit)`,true);
  }
}
function startCircle(event) {
  const tool=circleTool;
  event.preventDefault(); suppressCanvasClick=true;
  if (!tool?.ready || tool.gesture || tool.committing) return;
  try {
    const matrix=elementFor(tool.parent)?.getScreenCTM(), inverse=matrix?.inverse();
    if (!matrix || !inverse || ![inverse.a,inverse.b,inverse.c,inverse.d,inverse.e,inverse.f].every(Number.isFinite)) throw Error("The destination has no invertible coordinate frame.");
    const center=new DOMPoint(event.clientX,event.clientY).matrixTransform(inverse);
    tool.gesture={pointerId:event.pointerId,matrix,inverse,cx:center.x,cy:center.y,r:10,startX:event.clientX,startY:event.clientY};
    $("canvas").setPointerCapture(event.pointerId);
    paintCirclePreview();
  } catch(error) {circleStatus(error.message,true);}
}
function moveCircle(event) {
  const gesture=circleTool?.gesture;
  if (!gesture || event.pointerId!==gesture.pointerId) return;
  if (Math.hypot(event.clientX-gesture.startX,event.clientY-gesture.startY)>3) gesture.dragged=true;
  if (gesture.dragged) {
    const local=new DOMPoint(event.clientX,event.clientY).matrixTransform(gesture.inverse);
    gesture.r=Math.max(0.000001,Math.hypot(local.x-gesture.cx,local.y-gesture.cy));
  }
  paintCirclePreview();
}
function paintCirclePreview() {
  const g=circleTool.gesture, rect=$("canvas").getBoundingClientRect(), m=g.matrix;
  const transform=`matrix(${m.a} ${m.b} ${m.c} ${m.d} ${m.e-rect.left} ${m.f-rect.top})`;
  $("creation-overlay").replaceChildren(svgPart("circle",{cx:g.cx,cy:g.cy,r:g.r,transform,
    fill:"#d9365e33",stroke:"#d9365e","stroke-width":1.5,"vector-effect":"non-scaling-stroke"}));
}
async function finishCircle(event) {
  const tool=circleTool, g=tool?.gesture;
  if (!g || event.pointerId!==g.pointerId) return;
  moveCircle(event);
  tool.gesture=null; tool.committing=true; tool.ready=false;
  suppressCanvasClick=true;
  if ($("canvas").hasPointerCapture(g.pointerId)) $("canvas").releasePointerCapture(g.pointerId);
  circleStatus(`Adding circle to ${tool.label}…`);
  try {
    const result=await (await drawingRequest("/api/create/circle",{method:"POST",headers:{"Content-Type":"application/json"},
      body:JSON.stringify({snapshot:tool.snapshot,parent:tool.parent,cx:g.cx,cy:g.cy,r:g.r})})).json();
    if (!result.ok) throw Error(result.reason);
    await load();
    tool.committing=false; stopCircle(); select(result.key);
    $("edit-status").textContent="Added circle. Undo removes it.";
    $("edit-status").classList.remove("error");
  } catch(error) {
    tool.committing=false; stopCircle();
    $("edit-status").textContent=error.message;
    $("edit-status").classList.add("error");
    await load();
  }
}

// Source-checked geometry widgets --------------------------------------------------
let geometryDrag = null, geometrySequence = 0, geometryTimer = null, suppressCanvasClick = false;
const geometryCache = new Map(); // bounded to 64 selections within the displayed snapshot
let geometryPending = null;
window.knotSelectionProfile = [];
function cancelGeometryCheck() {
  if (!geometryPending) return;
  const {ticket,abort}=geometryPending;
  geometryPending=null;
  abort.abort();
  drawingRequest("/api/geometry/cancel",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({ticket})}).catch(()=>{});
}
function scheduleGeometryCheck() {
  clearTimeout(geometryTimer);
  cancelGeometryCheck();
  const sequence = ++geometrySequence, key = state.selected, snapshot = state.snapshot, started=performance.now();
  state.geometry = null;
  $("geometry-status").textContent="";
  renderGeometryWidget();
  if (key === null || !snapshot || state.error || paintPicker || circleTool) return;
  const node=findNode(key), element=elementFor(key);
  if (!node || !element || typeof element.getBBox!=="function") return;
  let model;
  try {
    model=CanvasGeometry.model(node,element.getBBox());
    const matrix=element.getScreenCTM();
    if (!matrix || !Number.isFinite(matrix.inverse().a)) return;
  } catch {return;}
  if (!model) return;
  // Like the property editor, curve controls are offered from shape editability.
  // Their actual source route is validated on every preview, with the coordinate
  // frame pinned by begin_geometry. Whole-path affine probes cannot establish
  // whether a particular control point can change independently.
  const curveEditable=model.kind==="path" && fieldEditability("commands").editable;
  if (curveEditable) {
    state.geometry={snapshot,key,model,capabilities:{},curveEditable};
    renderGeometryWidget();
  }
  const probes=CanvasGeometry.probes(model), cacheKey=JSON.stringify([key,probes]);
  function install(result,cached) {
    state.geometry={snapshot,key,model,curveEditable,capabilities:result.capabilities || {},reason:result.reason};
    renderGeometryWidget();
    window.knotSelectionProfile.push({key,cached,handles_ms:performance.now()-started,...result.timings});
    if (window.knotSelectionProfile.length>200) window.knotSelectionProfile.shift();
  }
  if (geometryCache.has(cacheKey)) {install(geometryCache.get(cacheKey),true);return;}
  // Yield to paint the selection/properties; no fixed 100 ms delay.
  geometryTimer=setTimeout(async()=>{
    if (sequence!==geometrySequence || paintPicker) return;
    const ticket=crypto.randomUUID(),abort=new AbortController();
    geometryPending={ticket,abort};
    $("geometry-status").textContent=(curveEditable?"Drag Bézier knobs to reshape. ":"")+"Checking move and resize…";
    try {
      const response=await drawingRequest("/api/geometry",{method:"POST",headers:{"Content-Type":"application/json"},signal:abort.signal,body:JSON.stringify({snapshot,key,probes,ticket})});
      const result=await response.json();
      if (sequence!==geometrySequence || state.snapshot!==snapshot || state.selected!==key || paintPicker || result.cancelled) return;
      if (result.ok) {
        geometryCache.set(cacheKey,result);
        if (geometryCache.size>64) geometryCache.delete(geometryCache.keys().next().value);
      }
      install(result,false);
    } catch(error) {
      if (error.name!=="AbortError" && sequence===geometrySequence) $("geometry-status").textContent="Move/resize unavailable while disconnected.";
    } finally {if (geometryPending?.ticket===ticket) geometryPending=null;}
  },0);
}

function svgPart(name, attributes) {
  const element = document.createElementNS("http://www.w3.org/2000/svg",name);
  for (const [key,value] of Object.entries(attributes)) element.setAttribute(key,String(value));
  return element;
}
function renderGeometryWidget() {
  placeBox($("selection-box"),state.selected);
  const overlay = $("geometry-overlay");
  overlay.replaceChildren();
  const active = geometryDrag || state.geometry;
  if (!active || state.error || (!geometryDrag && (paintPicker || active.snapshot !== state.snapshot || active.key !== state.selected))) return;
  const {model,capabilities} = active;
  const handles = active.curve ? [] : CanvasGeometry.handles(model,capabilities);
  const movable = !active.curve && (capabilities.moveX?.editable || capabilities.moveY?.editable);
  const curveModel=geometryDrag ? {...model,values:active.latest.values} : model;
  const controls=active.curveEditable ? CanvasGeometry.curveControls(curveModel) : [];
  if (!movable && !handles.length && !controls.length) {
    $("geometry-status").textContent="This selection cannot be moved or resized.";
    $("selection-box").title = active.reason || Object.values(capabilities).find(c=>c.reason)?.reason || "This geometry cannot be edited.";
    return;
  }
  const element = elementFor(active.key), matrix = geometryDrag ? active.matrix : element?.getScreenCTM();
  if (!matrix) return;
  $("selection-box").hidden = true;
  $("geometry-status").textContent=(movable?"Drag to move. ":"")+(handles.length?"Drag box handles to resize. ":"")+(controls.length?"Drag anchors or Bézier knobs to reshape. ":"")+"Escape cancels.";
  const b = geometryDrag ? active.latest.bounds : model.bounds, canvas = $("canvas").getBoundingClientRect();
  const point = (x,y) => { const p = new DOMPoint(x,y).matrixTransform(matrix); return [p.x-canvas.left,p.y-canvas.top]; };
  const corners = [[b.x,b.y],[b.x+b.width,b.y],[b.x+b.width,b.y+b.height],[b.x,b.y+b.height]].map(([x,y])=>point(x,y));
  const points = corners.map(p=>p.join(",")).join(" ");
  if (movable) {
    const move = svgPart("polygon", {points,class:"geometry-move","data-geometry":"move","aria-label":"Move selection"});
    const title = svgPart("title",{}); title.textContent = capabilities.moveX?.editable && capabilities.moveY?.editable ? "Drag to move. Escape cancels." : capabilities.moveX?.editable ? "Drag to move horizontally. Escape cancels." : "Drag to move vertically. Escape cancels.";
    move.append(title); overlay.append(move);
  }
  if (movable || handles.length) overlay.append(svgPart("polygon",{points,class:"geometry-frame"}));
  for (const handle of handles) {
    const [hx,hy]=CanvasGeometry.HANDLES[handle], [x,y]=point(b.x+hx*b.width,b.y+hy*b.height);
    const part = svgPart("rect",{x:x-4.5,y:y-4.5,width:9,height:9,class:"geometry-handle","data-geometry":handle,"aria-label":"Resize "+handle});
    // Cursor follows the projected diagonal/axis under rotations and reflections.
    const vx=matrix.a*(hx-.5)+matrix.c*(hy-.5), vy=matrix.b*(hx-.5)+matrix.d*(hy-.5);
    const index=((Math.round(Math.atan2(vy,vx)/(Math.PI/4))%4)+4)%4;
    part.style.cursor=["ew-resize","nwse-resize","ns-resize","nesw-resize"][index];
    overlay.append(part);
  }
  // Draw controls last so the move rectangle cannot intercept their pointers.
  for (const control of controls.filter(c=>c.kind==="control")) {
    const [x1,y1]=point(...control.anchor), [x2,y2]=point(control.x,control.y);
    overlay.append(svgPart("line",{x1,y1,x2,y2,class:"bezier-tangent"}));
  }
  // Anchors stay on top when a tangent has zero length.
  for (const control of [...controls.filter(c=>c.kind==="control"),...controls.filter(c=>c.kind==="anchor")]) {
    const [x,y]=point(control.x,control.y), anchor=control.kind==="anchor";
    const attributes={class:anchor?"bezier-anchor":"bezier-control","data-geometry":control.id,
      "aria-label":`${anchor?"Path anchor":"Bézier control"} ${control.index+1}`};
    const part=anchor ? svgPart("rect",{...attributes,x:x-4,y:y-4,width:8,height:8})
      : svgPart("circle",{...attributes,cx:x,cy:y,r:5});
    const title=svgPart("title",{});
    title.textContent=anchor ? "Drag anchor and its attached handles. Escape cancels."
      : "Drag Bézier control point; the opposite handle keeps a continuous tangent. Escape cancels.";
    part.append(title); overlay.append(part);
  }
}
function startGeometryDrag(event,handle) {
  const active=state.geometry;
  if (paintPicker || !active || active.snapshot!==state.snapshot || event.button!==0 || spaceHeld) return;
  const curve=active.curveEditable && CanvasGeometry.curveControls(active.model).some(c=>c.id===handle);
  if (!curve && handle!=="move" && !CanvasGeometry.handles(active.model,active.capabilities).includes(handle)) return;
  if (handle==="move" && !active.capabilities.moveX?.editable && !active.capabilities.moveY?.editable) return;
  const matrix=elementFor(active.key)?.getScreenCTM();
  if (!matrix || !Number.isFinite(matrix.inverse().a)) return;
  event.preventDefault();
  const session=crypto.randomUUID(), origin=new DOMPoint(event.clientX,event.clientY).matrixTransform(matrix.inverse());
  ++state.loadSequence; ++geometrySequence; clearTimeout(geometryTimer); cancelGeometryCheck();
  const drag={...active,curve,matrix,inverse:matrix.inverse(),origin,handle,pointerId:event.pointerId,startX:event.clientX,startY:event.clientY,moved:false,finishing:false,error:null,latest:{values:active.model.values,bounds:active.model.bounds}};
  geometryDrag=drag;
  const controller=new PaintSession({
    request:paintRequest,session,snapshot:state.snapshot,key:active.key,beginAction:"begin_geometry",
    drawing(data) { ++state.loadSequence; return displayScene(data); },
    status(message,error) { if (error) drag.error=message; else drag.error=null; $("edit-status").textContent=error?message:"Drag to edit; release to keep, Escape to cancel."; $("edit-status").classList.toggle("error",error); },
    profile(timing) { window.knotPaintProfile.push(timing); if (window.knotPaintProfile.length>200) window.knotPaintProfile.shift(); },
  });
  paintPicker={controller,session,geometry:true};
  try { sessionStorage.setItem("knotty.paintSession",session); } catch {}
  $("canvas").setPointerCapture(event.pointerId);
  $("canvas").classList.add("geometry-dragging");
  renderGeometryWidget();
}
function moveGeometryDrag(event) {
  const drag=geometryDrag;
  if (!drag || drag.finishing || drag.ended || event.pointerId!==drag.pointerId) return;
  if (!drag.moved && Math.hypot(event.clientX-drag.startX,event.clientY-drag.startY)<3) return;
  drag.moved=true;
  const p=new DOMPoint(event.clientX,event.clientY).matrixTransform(drag.inverse);
  drag.latest=drag.curve
    ? CanvasGeometry.curveDrag(drag.model,drag.handle,p.x-drag.origin.x,p.y-drag.origin.y)
    : CanvasGeometry.drag(drag.model,drag.handle,p.x-drag.origin.x,p.y-drag.origin.y,drag.capabilities,event.shiftKey);
  paintPicker.controller.pick(drag.latest.values);
  renderGeometryWidget();
}
async function finishGeometryDrag(keep) {
  const drag=geometryDrag;
  if (!drag || drag.finishing) return;
  drag.finishing=true; drag.ended=true; suppressCanvasClick=true;
  const {controller}=paintPicker;
  let finished=await controller.finish(keep && drag.moved), failed=null;
  if (!finished && keep) {
    failed=drag.error || "The requested geometry could not be applied.";
    finished=await controller.finish(false); // don't leave a prior partial drag applied
  }
  if (!finished) { drag.finishing=false; return; } // Escape can retry cancellation
  failed=controller.finishError || failed;
  try { sessionStorage.removeItem("knotty.paintSession"); } catch {}
  paintPicker=null; geometryDrag=null;
  $("canvas").classList.remove("geometry-dragging");
  if ($("canvas").hasPointerCapture(drag.pointerId)) $("canvas").releasePointerCapture(drag.pointerId);
  $("edit-status").textContent=failed || (keep && drag.moved?"Geometry changed.":"Drag cancelled.");
  $("edit-status").classList.toggle("error",Boolean(failed));
  await load();
}

// Properties ------------------------------------------------------------------------

function selectedEditability() {
  if (state.error || state.selected === null) return null;
  return { snapshot:state.snapshot, key:state.selected, fields:findNode(state.selected)?.editability || {} };
}

function fieldEditability(name) {
  const fields = state.editability && state.editability.fields;
  return (fields && fields[name]) || { editable: false, reason: "Checking whether this can be changed…" };
}

// Field schema. The scene sends each node's values as flat "props" keyed by
// dotted field path; this says how to group and edit them.
const GEOMETRY = {
  svg: [], group: [],
  rect: ["x", "y", "width", "height", "rx", "ry"],
  circle: ["cx", "cy", "r"],
  ellipse: ["cx", "cy", "rx", "ry"],
  line: ["x1", "y1", "x2", "y2"],
  polyline: ["points", "closed"],
  polygon: ["points", "closed"],
  path: ["commands"],
  text: ["x", "y", "content"],
};

const GROUPS = [
  ["Viewport", (kind) => (kind === "svg" ? ["width", "height", "viewBox"] : [])],
  ["Geometry", (kind) => GEOMETRY[kind] || []],
  ["Font", () => ["font.family", "font.size", "font.anchor", "font.baseline"]],
  ["Fill", () => ["fill.paint", "fill.opacity", "fill.rule"]],
  ["Stroke", () => ["stroke.paint", "stroke.width", "stroke.opacity", "stroke.cap", "stroke.join",
    "stroke.miterLimit", "stroke.dashes", "stroke.dashOffset"]],
  ["Node", () => ["id", "transform", "opacity", "visible"]],
];

const ENUMS = {
  "fill.rule": ["nonzero", "evenodd"],
  "stroke.cap": ["butt", "round", "square"],
  "stroke.join": ["miter", "round", "bevel"],
  "font.anchor": ["start", "middle", "end"],
  "font.baseline": ["alphabetic", "middle", "central", "hanging"],
};

// Structured values use bounded JSON arrays; the server validates their schema.
const TEXT_FIELDS = ["points", "commands", "viewBox", "transform", "stroke.dashes"];

function fieldType(name) {
  if (ENUMS[name]) return "enum";
  if (TEXT_FIELDS.includes(name)) return "json";
  if (name === "fill.paint" || name === "stroke.paint") return "paint";
  if (name === "closed" || name === "visible") return "bool";
  if (name === "id" || name === "content" || name === "font.family") return "string";
  return "float";
}

function nodeGroups(node) {
  const groups = [];
  for (const [name, fieldsFor] of GROUPS) {
    const fields = fieldsFor(node.kind)
      .filter((field) => field in node.props)
      .map((field) => ({ name: field, type: fieldType(field), options: ENUMS[field], value: node.props[field] }));
    if (fields.length) groups.push({ name, fields });
  }
  return groups;
}

let savingWhip = false;

function drawingControls(controls, title) {
  const section = document.createElement("section");
  section.className = "property-group drawing-controls";
  const heading = document.createElement("h3");
  heading.textContent = title;
  section.append(heading);
  const snapshot = state.snapshot;
  const key = state.selected;
  controls.forEach(([index, whip]) => {
    const row = document.createElement("div");
    row.className = "drawing-slider";
    const label = document.createElement("label");
    const range = document.createElement("input");
    const count = document.createElement("output");
    range.type = "range";
    range.id = "drawing-slider-" + index;
    range.min = whip.min;
    range.max = whip.max;
    range.step = 1;
    range.value = whip.value ?? whip.min;
    range.disabled = savingWhip || !state.whipsEditable || whip.value === null;
    label.htmlFor = range.id;
    label.textContent = whip.label;
    count.htmlFor.add(range.id);
    count.value = whip.value ?? "Unavailable";
    range.addEventListener("input", () => { count.value = range.value; });
    range.addEventListener("change", async () => {
      if (savingWhip) return;
      const refocus = document.activeElement === range;
      savingWhip = true;
      range.disabled = true;
      const status = $("edit-status");
      try {
        const response = await drawingRequest("/api/whip", {method: "POST", headers: {"Content-Type": "application/json"},
          body: JSON.stringify({snapshot, index, key, value: range.valueAsNumber})});
        const result = await response.json();
        if (!result.ok) throw Error(result.reason);
        status.textContent = "";
        status.classList.remove("error");
      } catch (error) {
        status.textContent = error.message;
        status.classList.add("error");
      } finally {
        savingWhip = false;
        await load();
        if (refocus && document.activeElement === document.body && state.whips[index]?.label === whip.label) {
          $(range.id)?.focus({preventScroll: true});
        }
      }
    });
    row.append(label, count, range);
    section.append(row);
  });
  return section;
}

function renderProperties() {
  const container = $("properties");
  const node = state.selected === null ? null : findNode(state.selected);
  container.replaceChildren();
  const controls = Array.from(state.whips.entries());
  const global = controls.filter(([, w]) => w.keys == null);
  const selected = controls.filter(([, w]) => w.keys?.includes(state.selected));
  if (global.length) container.append(drawingControls(global, "Drawing"));
  if (selected.length) container.append(drawingControls(selected, "Object controls"));
  if (!node) {
    $("properties-title").textContent = "Properties";
    container.insertAdjacentHTML("beforeend", '<p class="empty">Select something in the drawing or the layers list to see its properties.</p>');
    return;
  }
  $("properties-title").textContent = node.kind + (node.id ? " " + node.id : "") + "  " + node.key;
  for (const group of nodeGroups(node)) {
    const section = document.createElement("section");
    section.className = "property-group";
    const heading = document.createElement("h3");
    heading.textContent = group.name;
    section.append(heading);
    for (const field of group.fields) section.append(fieldRow(node.key, field));
    container.append(section);
  }
}

function fieldRow(key, field) {
  const editability = fieldEditability(field.name);
  const row = document.createElement("div");
  row.className = "field " + (editability.editable ? "editable" : "locked");
  row.title = editability.editable ? "Can be changed" : editability.reason || "Can't be changed";

  const label = document.createElement("label");
  label.className = "field-label";
  label.textContent = field.name.split(".").pop();
  const id = "field-" + field.name.replace(/\W/g, "-");
  label.htmlFor = id;

  const value = document.createElement("div");
  value.className = "field-value";
  const control = document.createElement("button");
  control.type = "button";
  control.id = id;
  control.className = "paint-button";
  control.append(...lockedDisplay(field));
  control.addEventListener("click", () => field.type === "paint" ? openPaintPicker(key, field) : openPropertyEditor(key, field));
  control.dataset.control = "";
  const locked = document.createElement("div");
  locked.className = "locked-value";
  locked.append(...lockedDisplay(field));
  value.append(control, locked);

  row.append(label, value);
  return row;
}

function formatNumber(n) {
  return Number.isInteger(n) ? String(n) : String(Number(n.toFixed(4)));
}

function hex(rgb) {
  return "#" + rgb.map((c) => c.toString(16).padStart(2, "0")).join("");
}

function rgbOf(hexColor) {
  return [1, 3, 5].map((i) => parseInt(hexColor.slice(i, i + 2), 16));
}

function swatch(rgb) {
  const element = document.createElement("span");
  element.className = "swatch" + (rgb ? "" : " none");
  if (rgb) element.style.background = hex(rgb);
  return element;
}

function lockedDisplay(field) {
  const text = document.createElement("span");
  text.className = "readonly-text";
  switch (field.type) {
    case "float":
      text.textContent = formatNumber(field.value);
      return [text];
    case "bool":
      text.textContent = field.value ? "Yes" : "No";
      return [text];
    case "paint":
      text.textContent = field.value ? hex(field.value) : "None";
      return [swatch(field.value), text];
    case "json":
      text.textContent = JSON.stringify(field.value);
      return [text];
    case "string":
      if (field.value === null) {
        text.textContent = "None";
        text.classList.add("none-label");
      } else {
        text.textContent = field.value;
      }
      return [text];
    default:
      text.textContent = field.value === "" ? "None" : String(field.value);
      if (field.value === "") text.classList.add("none-label");
      return [text];
  }
}

const JSON_HELP = {
  points: 'Pairs of coordinates: [[10,20],[30,40]]. Keep the existing number of points.',
  commands: 'Path commands: [["M",10,20],["L",30,40],["C",1,2,3,4,5,6],["Z"]]. Keep the existing command count.',
  transform: 'Operations: [["translate",10,20],["scale",2,2],["rotate",45],["rotate",45,10,20],["skewX",10],["skewY",10]]. Keep the existing operation count.',
  viewBox: 'Four numbers: [minX,minY,width,height], or null.',
  "stroke.dashes": 'Dash lengths: [4,2]. Keep the existing number of entries.',
};
function openPropertyEditor(key, field) {
  if (paintPicker) return;
  cancelGeometryCheck();
  ++state.loadSequence;
  const session = crypto.randomUUID(), dialog = document.createElement("dialog");
  dialog.className = "paint-dialog property-dialog";
  dialog.setAttribute("aria-labelledby", "property-title");
  dialog.innerHTML = '<h2 id="property-title"></h2><label class="property-input-label"></label><p class="property-help"></p><p class="paint-status" role="status" aria-live="polite"></p><div class="paint-actions"><button type="button" data-action="cancel">Cancel</button><button type="button" data-action="ok">OK</button></div>';
  dialog.querySelector("h2").textContent = field.name;
  const input = document.createElement(field.type === "json" || field.name === "content" ? "textarea" : field.type === "enum" ? "select" : "input");
  input.id = "property-value"; input.setAttribute("aria-label", field.name);
  if (field.type === "enum") for (const option of field.options) input.append(new Option(option, option, false, option === field.value));
  else if (field.type === "bool") { input.type = "checkbox"; input.checked = field.value; }
  else {
    if (field.type === "float") { input.type = "number"; input.step = "any"; input.required = true; }
    input.value = field.type === "json" ? JSON.stringify(field.value) : field.value ?? "";
  }
  dialog.querySelector("label").append(input);
  dialog.querySelector(".property-help").textContent = JSON_HELP[field.name] || (field.name === "id" ? "Empty clears the ID, when the source accepts an optional ID." : "Changes preview as you type. Cancel restores the original.");
  const status = dialog.querySelector(".paint-status");
  const controller = new PaintSession({
    request: paintRequest, session, snapshot: state.snapshot, key, field: field.name,
    drawing(data) { ++state.loadSequence; return displayScene(data); },
    status(message, error) { status.textContent = message; status.classList.toggle("error", error); },
    profile(timing) { window.knotPaintProfile.push(timing); if (window.knotPaintProfile.length > 200) window.knotPaintProfile.shift(); },
  });
  paintPicker = { controller, session };
  try { sessionStorage.setItem("knotty.paintSession", session); } catch {}
  let valid = true, closing = false;
  input.addEventListener("input", () => {
    try {
      let value;
      if (field.type === "json") value = JSON.parse(input.value);
      else if (field.type === "float") { value = input.valueAsNumber; if (!Number.isFinite(value)) throw new Error("Enter a complete number."); }
      else if (field.type === "bool") value = input.checked;
      else value = field.name === "id" && input.value === "" ? null : input.value;
      valid = true; input.setCustomValidity(""); controller.pick(value);
    } catch {
      valid = false; input.setCustomValidity("Enter a complete valid value.");
      status.textContent = "Enter a complete valid value to preview it."; status.classList.add("error");
    }
  });
  async function finish(keep) {
    if (closing || (keep && (!valid || !input.reportValidity()))) return;
    closing = true;
    for (const control of dialog.querySelectorAll("input, textarea, select, button")) control.disabled = true;
    if (await controller.finish(keep)) {
      try { sessionStorage.removeItem("knotty.paintSession"); } catch {}
      paintPicker = null; dialog.close(); dialog.remove();
      $("edit-status").textContent = controller.finishError || (keep ? "Change kept." : "Preview cancelled.");
      $("edit-status").classList.toggle("error", Boolean(controller.finishError));
      await load();
    } else {
      closing = false;
      for (const control of dialog.querySelectorAll("input, textarea, select, button")) control.disabled = false;
    }
  }
  dialog.querySelector('[data-action="ok"]').addEventListener("click", () => finish(true));
  dialog.querySelector('[data-action="cancel"]').addEventListener("click", () => finish(false));
  dialog.addEventListener("cancel", event => { event.preventDefault(); finish(false); });
  document.body.append(dialog); dialog.showModal(); input.focus();
}

// The modal remains mounted while scene/properties refresh underneath it.
async function paintRequest(body) {
  const headers = {"Content-Type":"application/json"};
  if (body.action === "update" && body.snapshot === state.snapshot) headers["X-Knot-Scene-Patch"] = "subtrees-v1";
  const response = await drawingRequest("/api/paint", { method: "POST", headers, body: JSON.stringify(body) });
  return response.json();
}
function openPaintPicker(key, field) {
  if (paintPicker) return;
  cancelGeometryCheck();
  ++state.loadSequence; // invalidate a scene fetch begun before opening
  const session = crypto.randomUUID();
  const dialog = document.createElement("dialog");
  dialog.className = "paint-dialog";
  dialog.setAttribute("aria-labelledby", "paint-title");
  dialog.innerHTML = `
    <h2 id="paint-title"></h2>
    <div class="paint-sv" tabindex="0" role="group" aria-label="Saturation and brightness. Use arrow keys to adjust."><span class="paint-cursor"></span></div>
    <label class="paint-hue-label">Hue <input class="paint-hue" type="range" min="0" max="359" step="1"></label>
    <div class="paint-channels">
      <label>Red <input data-channel="0" type="range" min="0" max="255" step="1"></label>
      <label>Green <input data-channel="1" type="range" min="0" max="255" step="1"></label>
      <label>Blue <input data-channel="2" type="range" min="0" max="255" step="1"></label>
    </div>
    <label class="paint-hex-label"><span class="paint-sample"></span> Hex <input class="paint-hex" type="text" maxlength="7" pattern="#[0-9a-fA-F]{6}" spellcheck="false" required></label>
    <label><input class="paint-none" type="checkbox"> No paint</label>
    <p class="paint-status" role="status" aria-live="polite"></p>
    <div class="paint-actions"><button type="button" data-action="cancel">Cancel</button><button type="button" data-action="ok">OK</button></div>`;
  dialog.querySelector("h2").textContent = field.name === "stroke.paint" ? "Stroke color" : "Fill color";
  const square = dialog.querySelector(".paint-sv"), cursor = dialog.querySelector(".paint-cursor");
  const hue = dialog.querySelector(".paint-hue"), hexInput = dialog.querySelector(".paint-hex");
  const sample = dialog.querySelector(".paint-sample"), status = dialog.querySelector(".paint-status");
  status.textContent = "Drag to preview. OK keeps the color; Cancel restores the original.";
  let rgb = field.value || [0, 0, 0], hsv = rgbToHsv(rgb), closing = false;
  const controller = new PaintSession({
    request: paintRequest, session, snapshot: state.snapshot, key, field: field.name,
    drawing(data) { ++state.loadSequence; return displayScene(data); },
    status(message, error) { status.textContent = message; status.classList.toggle("error", error); },
    profile(timing) { window.knotPaintProfile.push(timing); if (window.knotPaintProfile.length > 200) window.knotPaintProfile.shift(); },
  });
  paintPicker = { controller, session };
  try { sessionStorage.setItem("knotty.paintSession", session); } catch {}
  const noPaint = dialog.querySelector(".paint-none");
  noPaint.checked = field.value === null;
  noPaint.addEventListener("input", () => controller.pick(noPaint.checked ? null : rgb.slice()));
  function sync(pick = true) {
    if (pick) noPaint.checked = false;
    square.style.backgroundColor = `hsl(${hsv[0]}, 100%, 50%)`;
    cursor.style.left = `${hsv[1] * 100}%`; cursor.style.top = `${(1 - hsv[2]) * 100}%`;
    hue.value = hsv[0];
    if (hexInput.value.toLowerCase() !== hex(rgb)) hexInput.value = hex(rgb);
    sample.style.background = hex(rgb);
    for (const input of dialog.querySelectorAll("[data-channel]")) input.value = rgb[Number(input.dataset.channel)];
    if (pick) controller.pick(rgb.slice());
  }
  function position(event) {
    const rect = square.getBoundingClientRect();
    hsv[1] = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
    hsv[2] = 1 - Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height));
    rgb = hsvToRgb(...hsv); sync();
  }
  square.addEventListener("pointerdown", event => { if (closing) return; square.setPointerCapture(event.pointerId); position(event); });
  square.addEventListener("pointermove", event => { if (!closing && square.hasPointerCapture(event.pointerId)) position(event); });
  square.addEventListener("pointerup", event => { if (square.hasPointerCapture(event.pointerId)) square.releasePointerCapture(event.pointerId); });
  square.addEventListener("keydown", event => {
    if (closing || !["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const amount = event.shiftKey ? 0.1 : 0.01;
    if (event.key === "ArrowLeft") hsv[1] -= amount;
    if (event.key === "ArrowRight") hsv[1] += amount;
    if (event.key === "ArrowUp") hsv[2] += amount;
    if (event.key === "ArrowDown") hsv[2] -= amount;
    hsv[1] = Math.max(0, Math.min(1, hsv[1])); hsv[2] = Math.max(0, Math.min(1, hsv[2]));
    rgb = hsvToRgb(...hsv); sync();
  });
  hue.addEventListener("input", () => { hsv[0] = Number(hue.value); rgb = hsvToRgb(...hsv); sync(); });
  for (const input of dialog.querySelectorAll("[data-channel]")) input.addEventListener("input", () => { rgb[Number(input.dataset.channel)] = Number(input.value); hsv = rgbToHsv(rgb); sync(); });
  hexInput.addEventListener("input", () => { if (/^#[0-9a-f]{6}$/i.test(hexInput.value)) { rgb = rgbOf(hexInput.value); hsv = rgbToHsv(rgb); sync(); } });
  async function finish(keep) {
    if (closing || (keep && !noPaint.checked && !hexInput.reportValidity())) return;
    closing = true;
    for (const control of dialog.querySelectorAll("input, button")) control.disabled = true;
    if (await controller.finish(keep)) {
      try { sessionStorage.removeItem("knotty.paintSession"); } catch {}
      paintPicker = null; dialog.close(); dialog.remove();
      $("edit-status").textContent = controller.finishError || (keep ? "Color kept." : "Color preview cancelled.");
      $("edit-status").classList.toggle("error", Boolean(controller.finishError));
      await load();
    } else {
      closing = false;
      for (const control of dialog.querySelectorAll("input, button")) control.disabled = false;
    }
  }
  dialog.querySelector('[data-action="ok"]').addEventListener("click", () => finish(true));
  dialog.querySelector('[data-action="cancel"]').addEventListener("click", () => finish(false));
  dialog.addEventListener("cancel", event => { event.preventDefault(); finish(false); });
  sync(false); document.body.append(dialog); dialog.showModal();
}

async function recoverPaintPreview() {
  let session;
  try { session = sessionStorage.getItem("knotty.paintSession"); } catch {}
  if (!session) return;
  const result = await paintRequest({ action: "cancel", session });
  if (result.ok || result.closed || result.reason === "Property editor session expired.") sessionStorage.removeItem("knotty.paintSession");
}
window.addEventListener("pagehide", () => {
  if (paintPicker && !document.body.dataset.demo) navigator.sendBeacon("/api/paint", new Blob([JSON.stringify({ action: "cancel", session: paintPicker.session })], { type: "application/json" }));
});

// Panels, keyboard, preferences -------------------------------------------------------

const narrowScreen = matchMedia("(max-width: 720px)");
function panelKey(name) { return (narrowScreen.matches ? "mobile.panel." : "panel.") + name; }

function setPanel(name, open) {
  const workspace = document.querySelector(".workspace");
  workspace.classList.toggle("no-" + name, !open);
  $("toggle-" + name).setAttribute("aria-pressed", String(open));

  updateBoxes();
}

function setLockedStyle(style) {
  const properties = $("properties");
  properties.classList.remove("locked-dim", "locked-plain", "locked-lock");
  properties.classList.add("locked-" + style);
  $("locked-style").value = style;
  storage.set("lockedStyle", style);
}

function typing(event) {
  const target = event.target;
  return Boolean(target.closest("#source-panel")) || target instanceof HTMLInputElement || target instanceof HTMLSelectElement || target instanceof HTMLTextAreaElement;
}

function setupControls() {
  const panels = ["tree", "properties"];
  function restorePanels() {
    for (const name of panels) setPanel(name, storage.get(panelKey(name), !narrowScreen.matches));
    if (state.scene) fit();
  }
  restorePanels();
  narrowScreen.addEventListener("change", restorePanels);
  for (const name of panels) {
    $("toggle-" + name).addEventListener("click", () => {
      const open = $("toggle-" + name).getAttribute("aria-pressed") !== "true";
      if (open && narrowScreen.matches) {
        const other = name === "tree" ? "properties" : "tree";
        storage.set(panelKey(other), false);
        setPanel(other, false);
      }
      storage.set(panelKey(name), open);
      setPanel(name, open);
    });
  }
  const sourceButton = $("toggle-source");
  const showSource = (open) => {
    $("source-panel").hidden = !open;
    sourceButton.setAttribute("aria-pressed", String(open));
    if (open) KnotSource.open();
    if (state.scene) fit();
  };
  showSource(storage.get("panel.source", document.body.hasAttribute("data-demo")));
  sourceButton.onclick = () => {
    const open = $("source-panel").hidden;
    storage.set("panel.source", open);
    showSource(open);
  };
  setLockedStyle(storage.get("lockedStyle", "dim"));
  $("locked-style").addEventListener("change", (event) => setLockedStyle(event.target.value));
  $("add-circle").addEventListener("click", activateCircle);
  $("fit").addEventListener("click", () => {if (!circleTool?.gesture && !circleTool?.committing) fit();});
  $("actual-size").addEventListener("click", () => {if (!circleTool?.gesture && !circleTool?.committing) actualSize();});

  document.addEventListener("keydown", (event) => {
    if (circleTool && event.key==="Escape") {event.preventDefault();stopCircle();return;}
    if (circleTool?.gesture || circleTool?.committing) return;
    if (geometryDrag && event.key==="Escape") { event.preventDefault(); finishGeometryDrag(false); return; }
    if (paintPicker || typing(event)) return;
    if ((event.key==="c" || event.key==="C") && !event.ctrlKey && !event.metaKey && !event.altKey) {event.preventDefault();activateCircle();return;}
    if (event.code === "Space") {
      spaceHeld = true;
      $("canvas").classList.add("space");
      event.preventDefault();
    } else if (event.key === "f" || event.key === "F") {
      fit();
    } else if (event.key === "0") {
      actualSize();
    } else if (event.key === "Escape") {
      select(null);
    }
  });
  document.addEventListener("keyup", (event) => {
    if (event.code === "Space") {
      spaceHeld = false;
      $("canvas").classList.remove("space");
    }
  });
}

$("select-curve").addEventListener("click", () => {
  const curve = state.scene?.children?.find(n => n.id === "snake-curve");
  if (curve) select(curve.key);
});

setupControls();
setupCanvas();
if (document.body.dataset.demo) {
  load().catch(error => { showError(error.message); $("file-status").textContent = "Could not load demo"; });
} else {
  load().then(poll, poll);
}

$("undo-edit").addEventListener("click", async () => {
  const status = $("edit-status");
  try {
    const response = await drawingRequest("/api/undo", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ snapshot: state.snapshot }),
    });
    const result = await response.json();
    if (!result.ok) throw new Error(result.reason);
    await load();
    status.textContent = "Undid source edit.";
    status.classList.remove("error");
  } catch (error) {
    status.textContent = error.message;
    status.classList.add("error");
  }
});
