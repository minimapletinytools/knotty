use super::*;
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../web/index.html");
const PAINT: &str = include_str!("../web/paint.js");
const GEOMETRY: &str = include_str!("../web/geometry.js");
const RECONCILE: &str = include_str!("../web/reconcile.js");
const APP: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");

struct Options {
    file: PathBuf,
    port: Option<u16>,
    open: bool,
    library: PathBuf,
    preview_editability: bool,
    svg_renderer: SvgRenderer,
}

fn parse_options() -> Result<Options, String> {
    let usage = "usage: knottydrawings FILE.knot [--port N] [--no-open] [--library DIR] [--preview-editability] [--renderer knot|scene|direct]";
    let mut file = None;
    let mut port = None;
    let mut open = true;
    let mut library = std::env::var_os("KNOTTY_DRAWINGS_LIBRARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(".."));
    let mut preview_editability = false;
    let mut svg_renderer = SvgRenderer::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args.next().ok_or(usage)?;
                port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid port: {value}"))?,
                );
            }
            "--no-open" => open = false,
            "--library" => library = args.next().ok_or(usage)?.into(),
            "--preview-editability" => preview_editability = true,
            "--renderer" => {
                svg_renderer = match args.next().as_deref() {
                    Some("knot") => SvgRenderer::Knot,
                    Some("scene") => SvgRenderer::Scene,
                    Some("direct") => SvgRenderer::Direct,
                    _ => return Err("expected --renderer knot|scene|direct".into()),
                }
            }
            "-h" | "--help" => return Err(usage.into()),
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}\n{usage}")),
            _ if file.is_none() => file = Some(PathBuf::from(arg)),
            _ => return Err(usage.into()),
        }
    }
    Ok(Options {
        file: file.ok_or(usage)?,
        port,
        open,
        library,
        preview_editability,
        svg_renderer,
    })
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("KNOT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(|p| PathBuf::from(p).join("knot")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache/knot")))
}

/// Binds the requested port, or the first free one from 4747.
fn bind(port: Option<u16>) -> Result<Server, String> {
    let ports: Vec<u16> = match port {
        Some(p) => vec![p],
        None => (4747..4767).collect(),
    };
    for p in ports {
        if let Ok(server) = Server::http(("127.0.0.1", p)) {
            return Ok(server);
        }
    }
    Err("no free port".into())
}

fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(program).arg(url).spawn() {
        eprintln!("could not open a browser ({e}); visit {url}");
    }
}

pub fn start() {
    if let [flag, file] = std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        if flag == "--export-svg" {
            let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
            let result = Drawing::with_renderer(file.into(), &library, None, SvgRenderer::Direct)
                .and_then(|drawing| {
                    drawing
                        .last_good()
                        .map(|r| r.svg.clone())
                        .ok_or_else(|| drawing.error().unwrap_or("No drawing").to_owned())
                });
            match result {
                Ok(svg) => print!("{svg}"),
                Err(reason) => {
                    eprintln!("{reason}");
                    std::process::exit(1);
                }
            }
            return;
        }
    }
    if std::env::args().skip(1).eq(["--export-stories"]) {
        let catalog = storybook::catalog();
        if let Some(error) = catalog.get("error") {
            eprintln!("{error}");
            std::process::exit(1);
        }
        println!("{catalog}");
        return;
    }
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    let mut drawing = Drawing::with_renderer(
        options.file,
        &options.library,
        cache_dir().as_deref(),
        options.svg_renderer,
    )?;
    if let Some(e) = drawing.error() {
        eprintln!("{e}");
    }
    let mut editor = Editor::new(options.preview_editability);
    let server = bind(options.port)?;
    let url = format!("http://{}", server.server_addr());
    println!("knottydrawings: {} at {url}", drawing.path().display());
    if options.open {
        open_browser(&url);
    }
    let mut geometry = geometry_worker::GeometryWorker::new();
    for request in server.incoming_requests() {
        if let Err(e) = handle(request, &mut drawing, &mut editor, &mut geometry) {
            eprintln!("request failed: {e}");
        }
    }
    Ok(())
}

fn handle(
    mut request: Request,
    drawing: &mut Drawing,
    editor: &mut Editor,
    geometry: &mut geometry_worker::GeometryWorker,
) -> std::io::Result<()> {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let method = request.method().clone();
    match (&method, path) {
        (Method::Get, "/") => request.respond(text(INDEX, "text/html; charset=utf-8")),
        (Method::Get, "/api/tangle/stories") => {
            request.respond(json_response(storybook::catalog()))
        }
        (Method::Get, "/tangle" | "/tangle/stories") => request.respond(text(
            include_str!("../web/tangle/index.html"),
            "text/html; charset=utf-8",
        )),
        (Method::Get, "/tangle.js") => request.respond(text(
            include_str!("../web/tangle/editor.js"),
            "text/javascript; charset=utf-8",
        )),
        (Method::Get, "/tangle.css") => request.respond(text(
            include_str!("../web/tangle/editor.css"),
            "text/css; charset=utf-8",
        )),
        (Method::Get, "/source.js") => request.respond(text(
            include_str!("../web/source.js"),
            "text/javascript; charset=utf-8",
        )),
        (
            Method::Get,
            "/source-editor/editor.js"
            | "/source-editor/editor.css"
            | "/source-editor/editor.worker.js",
        ) => {
            let file = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("web")
                .join(&path[1..]);
            match std::fs::read(file) {
                Ok(bytes) => {
                    let mime = if path.ends_with(".css") {
                        "text/css"
                    } else {
                        "text/javascript"
                    };
                    request.respond(
                        Response::from_data(bytes)
                            .with_header(Header::from_bytes("Content-Type", mime).unwrap()),
                    )
                }
                Err(_) => request.respond(
                    Response::from_string("Build the source editor with npm ci && npm run build.")
                        .with_status_code(404),
                ),
            }
        }
        (Method::Get, "/paint.js") => {
            request.respond(text(PAINT, "text/javascript; charset=utf-8"))
        }
        (Method::Get, "/reconcile.js") => {
            request.respond(text(RECONCILE, "text/javascript; charset=utf-8"))
        }
        (Method::Get, "/geometry.js") => {
            request.respond(text(GEOMETRY, "text/javascript; charset=utf-8"))
        }
        (Method::Get, "/app.js") => request.respond(text(APP, "text/javascript; charset=utf-8")),
        (Method::Get, "/style.css") => request.respond(text(STYLE, "text/css; charset=utf-8")),
        (Method::Post, "/api/geometry/cancel") => {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body)?;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(ticket) = value["ticket"].as_str() {
                    geometry.cancel(Some(ticket));
                }
            }
            request.respond(json_response(&json!({"ok":true})))
        }
        (Method::Post, "/api/geometry") => {
            drawing.refresh();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body)?;
            let parsed = (|| -> Result<_, String> {
                let value: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                let ticket = value["ticket"].as_str().unwrap_or("").to_string();
                if ticket.len() > 128 {
                    return Err("Invalid geometry ticket.".into());
                }
                let check: edit::GeometryCheck =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                Ok((ticket, editor.geometry_task(drawing, check)?))
            })();
            match parsed {
                Ok((ticket, task)) => {
                    geometry.submit(ticket, task, request);
                    Ok(())
                }
                Err(reason) => request.respond(json_response(&json!({"ok":false,"reason":reason}))),
            }
        }
        (_, path) if path.starts_with("/api/") => {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body)?;
            let sparse = request
                .headers()
                .iter()
                .any(|h| h.field.equiv("X-Knot-Scene-Patch") && h.value.as_str() == "subtrees-v1");
            if method == Method::Post {
                geometry.cancel(None);
            }
            let result = api::dispatch(
                method.as_str(),
                path,
                query,
                &body,
                drawing,
                editor,
                sparse,
                |snapshot, key| geometry.prepared_path(snapshot, key),
            );
            request.respond(json_response(&result))
        }
        _ => request.respond(Response::from_string("not found").with_status_code(404)),
    }
}
fn text(body: impl Into<String>, content_type: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_header(Header::from_bytes("Content-Type", content_type).unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
}

pub(super) fn json_response(value: &serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    text(value.to_string(), "application/json")
}
