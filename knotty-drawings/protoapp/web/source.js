// Keep the latest rendered revision while Monaco loads or the pane is closed.
const KnotSource = (() => {
  let source = "", editor, loading;
  const host = document.getElementById("source-editor");
  const fallback = document.getElementById("source-text");
  return {
    update(next) {
      if (source === next) return;
      source = next;
      if (editor) editor(source);
      else fallback.textContent = source;
    },
    open() {
      if (loading) return;
      const url = new URL("source-editor/editor.js", document.baseURI);
      const css = document.createElement("link");
      css.rel = "stylesheet";
      css.href = new URL("editor.css", url);
      document.head.append(css);
      loading = import(url.href).then(({ create }) => {
        editor = create(host, source);
        fallback.remove();
      }).catch(() => { fallback.title = "Plain source view. Build the Monaco assets to enable highlighting."; });
    },
  };
})();
