//! One drawing session per browser worker. The WASI host supplies an isolated
//! in-memory directory for the entry, its imports and two JSON message files.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use std::cell::RefCell;

thread_local! { static SESSION: RefCell<Option<(Drawing, Editor)>> = const { RefCell::new(None) }; }

#[derive(Deserialize)]
struct Message {
    method: String,
    url: String,
    #[serde(default)]
    body: String,
}

fn handle(msg: Message) -> Result<Value, String> {
    SESSION.with(|session| {
        let mut session = session.borrow_mut();
        if msg.method == "OPEN" {
            let drawing = Drawing::with_renderer(
                PathBuf::from(&msg.url),
                Path::new("/project"),
                None,
                SvgRenderer::Direct,
            )?;
            if let Some(reason) = drawing.error() {
                return Err(reason.to_owned());
            }
            *session = Some((drawing, Editor::new(false)));
            return Ok(json!({"ok": true}));
        }
        let (drawing, editor) = session.as_mut().ok_or("Open a drawing first")?;
        let (path, query) = msg.url.split_once('?').unwrap_or((&msg.url, ""));
        match (msg.method.as_str(), path) {
            ("POST", "/api/geometry") => {
                let check = serde_json::from_str(&msg.body).map_err(|e| e.to_string())?;
                let task = editor.geometry_task(drawing, check)?;
                let engine = edit::new_engine().map_err(|e| e.to_string())?;
                Ok(task.evaluate(&engine, Default::default()))
            }
            ("POST", "/api/geometry/cancel") => Ok(json!({"ok":true})),
            _ => Ok(api::dispatch(
                &msg.method,
                path,
                query,
                &msg.body,
                drawing,
                editor,
                false,
                |_, _| None,
            )),
        }
    })
}

#[no_mangle]
pub extern "C" fn drawing_request() {
    let request = std::fs::read("/project/request.json").expect("host request");
    let result = serde_json::from_slice(&request)
        .map_err(|e| e.to_string())
        .and_then(handle);
    let response = result.unwrap_or_else(|reason| json!({"ok":false,"reason":reason}));
    std::fs::write(
        "/project/response.json",
        serde_json::to_vec(&response).unwrap(),
    )
    .expect("host response");
}
