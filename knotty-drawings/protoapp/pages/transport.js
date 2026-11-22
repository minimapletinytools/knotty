// Static Pages draws through the same request interface as the local HTTP app.
const browserDrawing = (() => {
  const worker = new Worker(new URL("runtime.worker.js", document.currentScript.src), { type: "module" });
  const pending = new Map();
  let serial = 0, failure;
  worker.onmessage = ({ data: { id, value, error, log } }) => {
    if (log) { window.dispatchEvent(new CustomEvent("drawing-log", { detail: log })); return; }
    const call = pending.get(id);
    if (!call) return;
    pending.delete(id);
    if (error) call.reject(new Error(error));
    else call.resolve({ json: async () => value });
  };
  worker.onerror = event => {
    failure = new Error(event.message || "Drawing worker failed to load");
    for (const call of pending.values()) call.reject(failure);
    pending.clear();
  };
  return (url, { method = "GET", body = "", signal } = {}) => {
    if (failure) return Promise.reject(failure);
    signal?.throwIfAborted();
    return new Promise((resolve, reject) => {
      const id = ++serial;
      const abort = () => { pending.delete(id); reject(signal.reason); };
      signal?.addEventListener("abort", abort, { once: true });
      const finish = callback => value => { signal?.removeEventListener("abort", abort); callback(value); };
      pending.set(id, { resolve: finish(resolve), reject: finish(reject) });
      worker.postMessage({ id, demo: document.body.dataset.demo, request: { url, method, body } });
    });
  };
})();
