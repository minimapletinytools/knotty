import { WASI, Directory, File, OpenFile, ConsoleStdout, PreopenDirectory } from "@bjorn3/browser_wasi_shim";

const encode = new TextEncoder(), decode = new TextDecoder();
async function load(demo) {
  const get = async name => {
    const response = await fetch(new URL(name, self.location.href));
    if (!response.ok) throw Error(`Could not load ${name} (${response.status})`);
    return response;
  };
  // Without a demo (the storybook) the runtime opens no drawing.
  const [module, imports, entry] = await Promise.all([
    get("drawing.wasm").then(WebAssembly.compileStreaming),
    demo ? get("libraries.json").then(r => r.json()) : {},
    demo ? get(`../drawings/${encodeURIComponent(demo)}/source.knot`).then(r => r.text()) : null,
  ]);
  const request = new File([]), response = new File([]);
  const files = new Directory([
    ...Object.entries(imports).map(([name, source]) => [name, new File(encode.encode(source), { readonly: true })]),
    ...(demo ? [[demo + ".knot", new File(encode.encode(entry))]] : []),
    ["request.json", request], ["response.json", response],
  ]);
  const output = level => ConsoleStdout.lineBuffered(message => {
    console[level](message);
    self.postMessage({ log: { level, message } });
  });
  const wasi = new WASI([], [], [new OpenFile(new File([])), output("log"),
    output("warn"), new PreopenDirectory("/", [["project", files]])], { debug: false });
  const instance = await WebAssembly.instantiate(module, { wasi_snapshot_preview1: wasi.wasiImport });
  wasi.initialize(instance);
  function dispatch(message) {
    request.data = encode.encode(JSON.stringify(message));
    instance.exports.drawing_request();
    return JSON.parse(decode.decode(response.data));
  }
  if (demo) {
    const opened = dispatch({ method: "OPEN", url: `/project/${demo}.knot` });
    if (!opened.ok) throw Error(opened.reason);
  }
  return dispatch;
}
let session;
self.onmessage = async ({ data: { id, demo, request } }) => {
  try {
    session ??= load(demo);
    const dispatch = await session;
    self.postMessage({ id, value: dispatch(request) });
  } catch (err) {
    self.postMessage({ id, error: err.message });
  }
};
