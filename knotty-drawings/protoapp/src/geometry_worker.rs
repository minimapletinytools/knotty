//! One running read-only job and one replaceable pending job. Editing and source
//! publication stay on the main thread; workers own immutable snapshots only.
use crate::edit::GeometryTask;
use knot_workbench::traversal::PreparedContext;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};
use tiny_http::Request;
struct Job {
    task: GeometryTask,
    request: Request,
    cancel: Arc<AtomicBool>,
    ready: ReadyPath,
}
#[derive(Default)]
struct Queue {
    pending: Option<Job>,
    stopped: bool,
}
// Each submission has its own slot, so a cancelled job cannot displace a newer one.
type ReadyPath = Arc<OnceLock<PreparedContext>>;
pub struct GeometryWorker {
    selection: Option<(String, ReadyPath)>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    latest: Option<(String, Arc<AtomicBool>)>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl GeometryWorker {
    pub fn new() -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let shared = queue.clone();
        let thread = std::thread::spawn(move || {
            let mut engine = None;
            let mut context: Option<knot_workbench::traversal::PreparedContext> = None;
            loop {
                let job = {
                    let (lock, wake) = &*shared;
                    let mut state = lock.lock().unwrap();
                    while state.pending.is_none() && !state.stopped {
                        state = wake.wait(state).unwrap();
                    }
                    if state.stopped {
                        break;
                    }
                    state.pending.take().unwrap()
                };
                if !job.cancel.load(Ordering::Relaxed) && engine.is_none() {
                    match crate::edit::new_engine() {
                        Ok(value) => engine = Some(value),
                        Err(error) => {
                            let _ = job
                                .request
                                .respond(crate::json_response(&job.task.error(&error.to_string())));
                            continue;
                        }
                    }
                }
                let result = if job.cancel.load(Ordering::Relaxed) {
                    job.task.cancelled()
                } else {
                    let engine = engine.as_ref().unwrap();
                    if context
                        .as_ref()
                        .is_none_or(|c| c.snapshot() != job.task.snapshot.id())
                    {
                        match engine.context(&job.task.snapshot) {
                            Ok(value) => context = Some(value),
                            Err(error) => {
                                let _ = job.request.respond(crate::json_response(
                                    &job.task.error(&error.to_string()),
                                ));
                                continue;
                            }
                        }
                    }
                    // After handoff, no subsequent worker job can mutate this context.
                    let probes = if job.task.prepare_path {
                        context.take().unwrap()
                    } else {
                        context.as_ref().unwrap().clone()
                    };
                    job.task
                        .evaluate_in(engine, probes, job.cancel.clone(), |path| {
                            let _ = job.ready.set(path);
                        })
                };
                let _ = job.request.respond(crate::json_response(&result));
            }
        });
        Self {
            queue,
            latest: None,
            selection: None,
            thread: Some(thread),
        }
    }
    pub fn submit(&mut self, ticket: String, task: GeometryTask, request: Request) {
        self.cancel(None);
        let cancel = Arc::new(AtomicBool::new(false));
        self.latest = Some((ticket, cancel.clone()));
        let (lock, wake) = &*self.queue;
        let ready = ReadyPath::default();
        self.selection = Some((task.selection_key().into(), ready.clone()));
        let previous = lock.lock().unwrap().pending.replace(Job {
            task,
            ready,
            request,
            cancel,
        });
        if let Some(old) = previous {
            let _ = old
                .request
                .respond(crate::json_response(&old.task.cancelled()));
        }
        wake.notify_one();
    }
    pub fn prepared_path(&self, snapshot: &str, key: &str) -> Option<PreparedContext> {
        let (selected, ready) = self.selection.as_ref()?;
        if selected != key {
            return None;
        }
        // Selection work is optional. A gesture never waits for an unfinished probe.
        ready.get().filter(|c| c.snapshot() == snapshot).cloned()
    }
    // A delayed cancellation from an old selection must not cancel a newer one.
    pub fn cancel(&mut self, ticket: Option<&str>) {
        if let Some((id, cancel)) = &self.latest {
            if ticket.is_none_or(|ticket| ticket == id) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}
impl Drop for GeometryWorker {
    fn drop(&mut self) {
        self.cancel(None);
        let (lock, wake) = &*self.queue;
        let pending = {
            let mut state = lock.lock().unwrap();
            state.stopped = true;
            state.pending.take()
        };
        if let Some(job) = pending {
            let _ = job
                .request
                .respond(crate::json_response(&job.task.cancelled()));
        }
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
