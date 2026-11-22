"use strict";

// Preserve unchanged SVG elements across revisions. Keys identify positions in
// this scene, not persistent object identities; every attribute/text is compared.
const SceneReconcile = (() => {
  function compatible(a, b) {
    return a.nodeType === b.nodeType && a.nodeName === b.nodeName && a.namespaceURI === b.namespaceURI &&
      (a.nodeType !== 1 || a.getAttribute("data-key") === b.getAttribute("data-key"));
  }
  function reconcile(old, node) {
    if (!compatible(old, node)) { old.replaceWith(node.cloneNode(true)); return; }
    if (node.nodeType === 1) {
      for (const attr of [...old.attributes]) {
        if (!node.hasAttributeNS(attr.namespaceURI, attr.localName)) old.removeAttributeNS(attr.namespaceURI, attr.localName);
      }
      for (const attr of node.attributes) {
        if (old.getAttributeNS(attr.namespaceURI, attr.localName) !== attr.value) old.setAttributeNS(attr.namespaceURI, attr.name, attr.value);
      }
      patch(old, node);
    } else if (old.nodeValue !== node.nodeValue) old.nodeValue = node.nodeValue;
  }
  function patch(parent, next) {
    const oldChildren = [...parent.childNodes], newChildren = [...next.childNodes];
    for (let i = 0; i < newChildren.length; i++) {
      const old = oldChildren[i], node = newChildren[i];
      if (!old) { parent.appendChild(node.cloneNode(true)); continue; }
      reconcile(old, node);
    }
    for (let i = newChildren.length; i < oldChildren.length; i++) oldChildren[i].remove();
  }
  function svg(stage, source) {
    const next = document.createElement("template");
    next.innerHTML = source;
    patch(stage, next.content);
  }
  // Validate every position before changing the DOM. The caller can reload a
  // complete revision if its scene and document have fallen out of sync.
  function children(stage, scene, changes, expand) {
    const replacements = [];
    function plan(oldScene, element, branch) {
      if (!element || element.children.length !== oldScene.children.length) return null;
      const next = oldScene.children.slice();
      let last = -1;
      for (const change of branch) {
        const i = change.index;
        if (!Number.isInteger(i) || i <= last || i >= next.length) return null;
        const old = element.children[i];
        if (old.getAttribute("data-key") !== next[i].key) return null;
        if (change.node) {
          if (next[i].key !== change.node.key) return null;
          replacements.push({old, change});
          expand(change.node);
          next[i] = change.node;
        } else {
          next[i] = plan(next[i], old, change.children);
          if (!next[i]) return null;
        }
        last = i;
      }
      return {...oldScene, children:next};
    }
    const next = plan(scene, stage.firstElementChild, changes);
    if (!next) return null;
    if (replacements.length) {
      const template = document.createElement("template");
      template.innerHTML = "<svg>" + replacements.map(r => r.change.svg).join("") + "</svg>";
      const nodes = template.content.firstElementChild.children;
      if (nodes.length !== replacements.length ||
          replacements.some(({change}, i) => nodes[i].getAttribute("data-key") !== change.node.key)) return null;
      replacements.forEach(({old}, i) => reconcile(old, nodes[i]));
    }
    return next;
  }
  return { svg, children };
})();
if (typeof module !== "undefined") module.exports = SceneReconcile;
