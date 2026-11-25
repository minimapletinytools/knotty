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

// `onEdit` hears only the user's typing; `set` applies published source as a
// minimal edit so the cursor and scroll position survive.
export function create(host, source, onEdit) {
  const editor = monaco.editor.create(host, { value: source, language: "knot",
    ariaLabel: "Knot source", automaticLayout: true, minimap: { enabled: false },
    scrollBeyondLastLine: false, fontSize: 12, lineNumbersMinChars: 3 });
  const model = editor.getModel();
  let applying = false;
  model.onDidChangeContent(() => { if (!applying) onEdit(model.getValue()); });
  return {
    value: () => model.getValue(),
    set(next) {
      const before = model.getValue();
      let first = 0;
      while (first < before.length && first < next.length && before[first] === next[first]) first++;
      let oldEnd = before.length, newEnd = next.length;
      while (oldEnd > first && newEnd > first && before[oldEnd - 1] === next[newEnd - 1]) { oldEnd--; newEnd--; }
      const start = model.getPositionAt(first);
      applying = true;
      try {
        model.applyEdits([{ range: monaco.Range.fromPositions(start, model.getPositionAt(oldEnd)),
          text: next.slice(first, newEnd), forceMoveMarkers: true }]);
      } finally { applying = false; }
      if (!editor.hasTextFocus()) editor.revealLineInCenterIfOutsideViewport(start.lineNumber);
    },
    // errors: [{ line, column, message }], 1-based.
    markers(errors) {
      monaco.editor.setModelMarkers(model, "knot", errors.map(({ line, column, message }) => {
        line = Math.min(Math.max(line, 1), model.getLineCount());
        column = Math.min(Math.max(column, 1), model.getLineMaxColumn(line));
        const word = model.getWordAtPosition({ lineNumber: line, column });
        return { severity: monaco.MarkerSeverity.Error, message,
          startLineNumber: line, startColumn: word?.startColumn ?? column,
          endLineNumber: line, endColumn: word?.endColumn ?? model.getLineMaxColumn(line) };
      }));
    },
  };
}
