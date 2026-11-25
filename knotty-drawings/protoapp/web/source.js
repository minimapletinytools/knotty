// Editable Knot source. Typing publishes after a short pause; invalid source is
// marked and never published, so the drawing keeps its last valid revision.
// Any other change to the drawing (a property, Tangle or Undo) replaces the
// buffer, discarding text that never became valid.
const KnotSource = (() => {
  const PAUSE_MS = 400, BURST_MS = 2000;
  let source = "", editor, loading;
  let timer, saving = null, again = false, submitted = null, lastSave = 0;
  const host = document.getElementById("source-editor");
  const fallback = document.getElementById("source-text");
  const status = document.getElementById("source-status");

  function show(text, error = false) {
    status.textContent = text;
    status.classList.toggle("error", error);
  }

  // "path/File.knot:10:3: message" or "path/File.knot: 10:3: message".
  function locate(reason) {
    const match = /^(.*?):\s?(\d+):(\d+):\s*([\s\S]*)$/.exec(reason);
    const file = document.getElementById("file-name").textContent;
    if (!match || !file || !match[1].endsWith(file)) return null;
    return { line: Number(match[2]), column: Number(match[3]), message: match[4] || reason };
  }

  async function publish() {
    timer = undefined;
    if (saving) { again = true; return; }
    const text = editor.value();
    if (text === source) { editor.markers([]); show(""); return; }
    show("Checking…");
    const coalesce = performance.now() - lastSave < BURST_MS;
    saving = text;
    try {
      const response = await drawingRequest("/api/source", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ snapshot: state.snapshot, source: text, coalesce }),
      });
      const result = await response.json();
      if (result.ok) {
        lastSave = performance.now();
        submitted = text;
        editor.markers([]);
        show("Applied");
        await load();
      } else if (/^Drawing changed/.test(result.reason)) {
        // Another edit won the race; it replaces this buffer when it loads.
        await load();
      } else {
        const at = locate(result.reason);
        editor.markers(at ? [at] : []);
        show(at ? `Error on line ${at.line} · not applied` : result.reason, true);
      }
    } catch (error) {
      show(error.message, true);
    } finally {
      saving = null;
      if (again) { again = false; schedule(); }
    }
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(publish, PAUSE_MS);
  }

  return {
    update(next) {
      if (source === next) return;
      source = next;
      if (!editor) { fallback.textContent = source; return; }
      // Our own publish coming back: keep anything typed since.
      if (next === submitted) { submitted = null; return; }
      submitted = null;
      lastSave = 0;
      clearTimeout(timer);
      timer = undefined;
      again = false;
      editor.markers([]);
      editor.set(source);
      show("");
    },
    open() {
      if (loading) return;
      const url = new URL("source-editor/editor.js", document.baseURI);
      const css = document.createElement("link");
      css.rel = "stylesheet";
      css.href = new URL("editor.css", url);
      document.head.append(css);
      loading = import(url.href).then(({ create }) => {
        editor = create(host, source, () => { show("Editing…"); schedule(); });
        fallback.remove();
      }).catch(() => { fallback.title = "Plain source view. Build the Monaco assets to enable editing."; });
    },
  };
})();
