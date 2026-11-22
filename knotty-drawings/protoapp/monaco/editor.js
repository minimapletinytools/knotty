import * as monaco from "monaco-editor/editor/editor.api.js";
import "monaco-editor/editor/standalone/browser/standaloneLanguages.js";
self.MonacoEnvironment = { getWorker: () => new Worker(new URL("./editor.worker.js", import.meta.url), { type: "module" }) };
monaco.languages.register({ id: "knot" });
monaco.languages.setMonarchTokensProvider("knot", {
  tokenizer: {
    root: [
      [/--.*/, "comment"], [/\{-/, "comment", "@comment"], [/"([^"\\]|\\.)*"/, "string"],
      [/@[\w_]+/, "annotation"], [/\b(module|import|exposing|as|let|in|if|then|else|case|of|do|type|alias|interface|instance)\b/, "keyword"],
      [/\b[A-Z]\w*/, "type.identifier"], [/\b(0x[\da-fA-F]+|\d+(\.\d+)?)/, "number"],
    ],
    comment: [[/\{-/, "comment", "@push"], [/-\}/, "comment", "@pop"], [/./, "comment"]],
  },
});
export function create(host, source) {
  const editor = monaco.editor.create(host, { value: source, language: "knot", readOnly: true,
    ariaLabel: "Live Knot source", automaticLayout: true, minimap: { enabled: false },
    scrollBeyondLastLine: false, fontSize: 12, lineNumbersMinChars: 3 });
  return next => {
    const model = editor.getModel();
    const before = model.getValue();
    let first = 0;
    while (first < before.length && first < next.length && before[first] === next[first]) first++;
    let oldEnd = before.length, newEnd = next.length;
    while (oldEnd > first && newEnd > first && before[oldEnd - 1] === next[newEnd - 1]) { oldEnd--; newEnd--; }
    const start = model.getPositionAt(first);
    model.applyEdits([{ range: monaco.Range.fromPositions(start, model.getPositionAt(oldEnd)),
      text: next.slice(first, newEnd), forceMoveMarkers: true }]);
    editor.revealLineInCenterIfOutsideViewport(start.lineNumber);
  };
}
