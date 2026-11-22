"use strict";

// All coordinates are in the selected SVG element's local user space. The UI
// converts pointers through its captured screen CTM (including nested viewBox).
const CanvasGeometry = (() => {
  const FIELDS = {
    rect: ["x", "y", "width", "height"], circle: ["cx", "cy", "r"],
    ellipse: ["cx", "cy", "rx", "ry"], line: ["x1", "y1", "x2", "y2"],
    polyline: ["points"], polygon: ["points"], path: ["commands"], text: ["x", "y", "font.size"],
  };
  const HANDLES = { nw: [0,0], n: [0.5,0], ne: [1,0], e: [1,0.5], se: [1,1], s: [0.5,1], sw: [0,1], w: [0,0.5] };
  function model(node, bounds) {
    if (!FIELDS[node.kind]) return null;
    const values = Object.fromEntries(FIELDS[node.kind].map(f => [f, structuredClone(node.props[f])]));
    const uniform = node.kind === "circle" || node.kind === "text";
    const origin = ["circle", "ellipse"].includes(node.kind) ? [values.cx, values.cy]
      : ["rect", "text"].includes(node.kind) ? [values.x, values.y] : [bounds.x, bounds.y];
    return { kind: node.kind, values, bounds: { x:bounds.x, y:bounds.y, width:bounds.width, height:bounds.height }, origin, uniform };
  }
  function affine(m, sx, sy, tx, ty) {
    const v = structuredClone(m.values), x = n => sx * n + tx, y = n => sy * n + ty;
    switch (m.kind) {
      case "rect": v.x=x(v.x); v.y=y(v.y); v.width*=sx; v.height*=sy; break;
      case "circle": v.cx=x(v.cx); v.cy=y(v.cy); v.r*=sx; break;
      case "ellipse": v.cx=x(v.cx); v.cy=y(v.cy); v.rx*=sx; v.ry*=sy; break;
      case "text": v.x=x(v.x); v.y=y(v.y); v["font.size"]*=sx; break;
      case "line": v.x1=x(v.x1); v.y1=y(v.y1); v.x2=x(v.x2); v.y2=y(v.y2); break;
      case "polyline": case "polygon": v.points=v.points.map(([a,b])=>[x(a),y(b)]); break;
      case "path": v.commands=v.commands.map(([cmd,...args])=>[cmd,...args.map((n,i)=>i%2 ? y(n) : x(n))]); break;
    }
    return v;
  }
  // Each anchor owns its adjacent cubic handles. Moving an anchor translates
  // those handles. A shared cubic join is smooth: rotating either handle rotates
  // the opposite handle onto the same tangent, preserving its length.
  function curveControls(m) {
    // Match the structured-property editor's 128-command request limit.
    if (m.kind !== "path" || !Array.isArray(m.values.commands) || m.values.commands.length > 128) return [];
    const controls = [];
    let current = null, start = null;
    for (const [index, command] of m.values.commands.entries()) {
      const [kind] = command;
      if (kind === "Z") {
        // A cubic that explicitly ends at the subpath start shares that anchor.
        // Keep both source coordinates in sync rather than opening the seam.
        if (current && start && current !== start && current.x === start.x && current.y === start.y) {
          start.positions.push(...current.positions);
          start.attached.push(...current.attached);
          controls.splice(controls.indexOf(current),1);
        }
        current = start; continue;
      }
      if (!["M", "L", "C"].includes(kind)) continue;
      const offset = kind === "C" ? 5 : 1;
      const anchor = { id:`anchor-${index}`, kind:"anchor", index, offset,
        x:command[offset], y:command[offset+1], positions:[[index,offset]], attached:[] };
      if (kind === "C") {
        const first = { id:`control-${index}-1`, kind:"control", index, offset:1,
          x:command[1], y:command[2], anchor:current ? [current.x,current.y] : [0,0] };
        const second = { id:`control-${index}-3`, kind:"control", index, offset:3,
          x:command[3], y:command[4], anchor:[anchor.x,anchor.y] };
        if (current) current.attached.push([index,1]);
        anchor.attached.push([index,3]);
        controls.push(first,second);
      }
      controls.push(anchor);
      current = anchor;
      if (kind === "M") start = anchor;
    }
    return controls;
  }
  function alignOpposite(commands, anchor, index, offset) {
    // Only two cubic tangents share smoothness. A line segment has no handle;
    // neither a separate subpath nor another outgoing branch is its opposite.
    const other = anchor.attached.find(([i,o])=>o !== offset);
    if (!other) return;
    const [i,o] = other;
    const dx=commands[index][offset]-anchor.x, dy=commands[index][offset+1]-anchor.y;
    const distance=Math.hypot(dx,dy);
    if (distance < 1e-12) return; // collapsed handle: retain the last usable tangent
    const length=Math.hypot(commands[i][o]-anchor.x,commands[i][o+1]-anchor.y);
    commands[i][o]=anchor.x-dx/distance*length;
    commands[i][o+1]=anchor.y-dy/distance*length;
  }
  function curveDrag(m, id, dx, dy) {
    const controls=curveControls(m), control=controls.find(c=>c.id===id);
    if (!control) return null;
    const values = structuredClone(m.values);
    const positions=control.kind==="anchor" ? [...control.positions,...control.attached]
      : [[control.index,control.offset]];
    for (const [index,offset] of positions) {
      values.commands[index][offset] += dx;
      values.commands[index][offset+1] += dy;
    }
    if (dx!==0 || dy!==0) {
      if (control.kind==="anchor") {
        // Existing corner-shaped source is smoothed when its join is moved,
        // taking the incoming tangent as the direction. Selection never edits it.
        const incoming=control.attached.find(([,offset])=>offset===3);
        if (incoming) alignOpposite(values.commands,{...control,x:control.x+dx,y:control.y+dy},...incoming);
      } else {
        const anchor=controls.find(c=>c.kind==="anchor" && c.attached.some(([i,o])=>i===control.index && o===control.offset));
        if (anchor) alignOpposite(values.commands,anchor,control.index,control.offset);
      }
    }
    return { values, bounds:m.bounds };
  }
  function probes(m) {
    const [ox,oy]=m.origin, factor=1.05;
    return {
      moveX: affine(m,1,1,1,0), moveY: affine(m,1,1,0,1),
      sizeX: affine(m,factor,m.uniform?factor:1,ox*(1-factor),m.uniform?oy*(1-factor):0),
      sizeY: affine(m,m.uniform?factor:1,factor,m.uniform?ox*(1-factor):0,oy*(1-factor)),
    };
  }
  function handles(m,caps) {
    const can=n=>Boolean(caps[n]?.editable), b=m.bounds, [ox,oy]=m.origin;
    return Object.entries(HANDLES).filter(([name,[hx,hy]])=>{
      if (m.uniform && name.length!==2) return false;
      const xs=hx!==0.5, ys=hy!==0.5;
      if ((xs && (b.width<=0 || !can("sizeX"))) || (ys && (b.height<=0 || !can("sizeY")))) return false;
      // Holding the opposite edge may require translation as well as size.
      if (xs && Math.abs(ox-(b.x+(1-hx)*b.width))>1e-9 && !can("moveX")) return false;
      if (ys && Math.abs(oy-(b.y+(1-hy)*b.height))>1e-9 && !can("moveY")) return false;
      return true;
    }).map(([name])=>name);
  }
  function drag(m,handle,dx,dy,caps,keepRatio=false) {
    if (handle==="move") {
      dx=caps.moveX?.editable?dx:0; dy=caps.moveY?.editable?dy:0;
      return { values:affine(m,1,1,dx,dy), bounds:{...m.bounds,x:m.bounds.x+dx,y:m.bounds.y+dy} };
    }
    const [hx,hy]=HANDLES[handle], b=m.bounds;
    let sx=hx===0.5?1:Math.max(0.001,1+(hx===0?-dx:dx)/b.width);
    let sy=hy===0.5?1:Math.max(0.001,1+(hy===0?-dy:dy)/b.height);
    if (m.uniform || (keepRatio && hx!==0.5 && hy!==0.5)) {
      const scale=Math.abs(sx-1)>=Math.abs(sy-1)?sx:sy; sx=scale; sy=scale;
    }
    const ax=b.x+(1-hx)*b.width, ay=b.y+(1-hy)*b.height;
    const tx=ax*(1-sx),ty=ay*(1-sy);
    return { values:affine(m,sx,sy,tx,ty), bounds:{x:b.x*sx+tx,y:b.y*sy+ty,width:b.width*sx,height:b.height*sy} };
  }
  return { model, probes, handles, drag, curveControls, curveDrag, HANDLES };
})();
if (typeof module !== "undefined") module.exports = CanvasGeometry;
