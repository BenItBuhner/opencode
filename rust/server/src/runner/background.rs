//! Process-local background job registry.
//!
//! This intentionally has no durable table. Jobs represent work owned by this
//! process only; after restart, session rows and admitted prompts remain, but
//! live job ownership/status does not.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Running,
    Completed,
    Error,
    Cancelled,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Info {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub status: Status,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub metadata: Value,
}

pub struct WaitResult {
    pub info: Option<Info>,
    #[allow(dead_code)]
    pub timed_out: bool,
}

#[derive(Clone)]
pub struct Control {
    cancelled: Arc<AtomicBool>,
}

impl Control {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

pub struct StartInput {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub metadata: Value,
    pub run: Box<dyn FnOnce(Control) -> Result<String, String> + Send + 'static>,
    pub on_complete: Option<Box<dyn FnOnce(Info) + Send + 'static>>,
    pub on_cancel: Option<Box<dyn Fn() + Send + Sync + 'static>>,
}

struct StartEntry {
    entry: Arc<Entry>,
    inserted: bool,
}

struct Job {
    info: Info,
    cancelled: Arc<AtomicBool>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

struct Entry {
    job: Mutex<Job>,
    done: Condvar,
}

static JOBS: Mutex<Option<HashMap<String, Arc<Entry>>>> = Mutex::new(None);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64
}

fn with_jobs<T>(f: impl FnOnce(&mut HashMap<String, Arc<Entry>>) -> T) -> T {
    let mut guard = JOBS.lock().expect("background registry poisoned");
    f(guard.get_or_insert_with(HashMap::new))
}

fn snapshot(entry: &Entry) -> Info {
    entry
        .job
        .lock()
        .expect("background job poisoned")
        .info
        .clone()
}

pub fn list() -> Vec<Info> {
    let mut jobs = with_jobs(|jobs| {
        jobs.values()
            .map(|entry| snapshot(entry))
            .collect::<Vec<_>>()
    });
    jobs.sort_by_key(|job| job.started_at);
    jobs
}

pub fn status(id: &str) -> Option<Info> {
    with_jobs(|jobs| jobs.get(id).map(|entry| snapshot(entry)))
}

pub fn start(input: StartInput) -> Info {
    let StartInput {
        id,
        kind,
        title,
        metadata,
        run,
        on_complete,
        on_cancel,
    } = input;
    let job_id = id.clone();
    let started = with_jobs(|jobs| {
        if let Some(entry) = jobs.get(&job_id) {
            if snapshot(entry).status == Status::Running {
                return StartEntry {
                    entry: entry.clone(),
                    inserted: false,
                };
            }
        }
        let entry = Arc::new(Entry {
            job: Mutex::new(Job {
                info: Info {
                    id,
                    kind,
                    title,
                    status: Status::Running,
                    started_at: now(),
                    completed_at: None,
                    output: None,
                    error: None,
                    metadata,
                },
                cancelled: Arc::new(AtomicBool::new(false)),
                on_cancel: on_cancel.map(Arc::from),
            }),
            done: Condvar::new(),
        });
        jobs.insert(job_id.clone(), entry.clone());
        StartEntry {
            entry,
            inserted: true,
        }
    });

    if !started.inserted {
        return snapshot(&started.entry);
    }
    let info = snapshot(&started.entry);
    if info.status != Status::Running {
        return info;
    }

    let control = Control {
        cancelled: started
            .entry
            .job
            .lock()
            .expect("background job poisoned")
            .cancelled
            .clone(),
    };
    let entry = started.entry.clone();
    std::thread::spawn(move || {
        let result = run(control);
        let completed = settle(&entry, result);
        if let (Some(info), Some(on_complete)) = (completed, on_complete) {
            on_complete(info);
        }
    });
    snapshot(&started.entry)
}

fn settle(entry: &Entry, result: Result<String, String>) -> Option<Info> {
    let mut job = entry.job.lock().expect("background job poisoned");
    if job.info.status != Status::Running {
        return Some(job.info.clone());
    }
    job.info.completed_at = Some(now());
    match result {
        Ok(output) => {
            job.info.status = Status::Completed;
            job.info.output = Some(output);
        }
        Err(error) => {
            job.info.status = if job.cancelled.load(Ordering::SeqCst) {
                Status::Cancelled
            } else {
                Status::Error
            };
            if job.info.status == Status::Error {
                job.info.error = Some(error);
            }
        }
    }
    let info = job.info.clone();
    entry.done.notify_all();
    Some(info)
}

pub fn extend(id: &str) -> bool {
    status(id).is_some_and(|info| info.status == Status::Running)
}

pub fn promote(id: &str) -> Option<Info> {
    let entry = with_jobs(|jobs| jobs.get(id).cloned())?;
    let mut job = entry.job.lock().expect("background job poisoned");
    if job.info.status != Status::Running {
        return Some(job.info.clone());
    }
    let mut metadata = job.info.metadata.as_object().cloned().unwrap_or_default();
    metadata.insert("background".into(), Value::Bool(true));
    job.info.metadata = Value::Object(metadata);
    let info = job.info.clone();
    entry.done.notify_all();
    Some(info)
}

pub fn cancel(id: &str) -> Option<Info> {
    let entry = with_jobs(|jobs| jobs.get(id).cloned())?;
    let on_cancel = {
        let mut job = entry.job.lock().expect("background job poisoned");
        if job.info.status != Status::Running {
            return Some(job.info.clone());
        }
        job.cancelled.store(true, Ordering::SeqCst);
        job.info.status = Status::Cancelled;
        job.info.completed_at = Some(now());
        job.on_cancel.clone()
    };
    if let Some(on_cancel) = on_cancel {
        on_cancel();
    }
    entry.done.notify_all();
    Some(snapshot(&entry))
}

#[allow(dead_code)]
pub fn wait(id: &str, timeout: Option<Duration>) -> WaitResult {
    wait_until(id, timeout, |info| info.status != Status::Running)
}

pub fn wait_done_or_promoted(id: &str) -> Option<Info> {
    wait_until(id, None, |info| {
        info.status != Status::Running
            || info
                .metadata
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    })
    .info
}

fn wait_until(id: &str, timeout: Option<Duration>, done: impl Fn(&Info) -> bool) -> WaitResult {
    let Some(entry) = with_jobs(|jobs| jobs.get(id).cloned()) else {
        return WaitResult {
            info: None,
            timed_out: false,
        };
    };
    let mut guard = entry.job.lock().expect("background job poisoned");
    if done(&guard.info) {
        return WaitResult {
            info: Some(guard.info.clone()),
            timed_out: false,
        };
    }
    match timeout {
        None => {
            while !done(&guard.info) {
                guard = entry.done.wait(guard).expect("background job poisoned");
            }
            WaitResult {
                info: Some(guard.info.clone()),
                timed_out: false,
            }
        }
        Some(timeout) => {
            let (guard, result) = entry
                .done
                .wait_timeout_while(guard, timeout, |job| !done(&job.info))
                .expect("background job poisoned");
            WaitResult {
                info: Some(guard.info.clone()),
                timed_out: result.timed_out(),
            }
        }
    }
}

pub fn cancel_task_tree(session_id: &str) {
    let ids = list()
        .into_iter()
        .filter(|job| {
            job.kind == "task"
                && job.status == Status::Running
                && (job.id == session_id
                    || job.metadata.get("parentSessionId").and_then(Value::as_str)
                        == Some(session_id))
        })
        .map(|job| job.id)
        .collect::<Vec<_>>();
    for id in ids {
        let _ = cancel(&id);
    }
}

#[cfg(test)]
pub fn reset_for_tests() {
    with_jobs(|jobs| jobs.clear());
}

pub fn is_promoted(info: &Info) -> bool {
    info.metadata
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn metadata(fields: &[(&str, Value)]) -> Value {
    let mut metadata = Map::new();
    for (key, value) in fields {
        metadata.insert((*key).into(), value.clone());
    }
    Value::Object(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn start_status_wait_and_reset_are_process_local() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_for_tests();
        let (finish_tx, finish_rx) = mpsc::channel();
        let info = start(StartInput {
            id: "ses_bg_registry".into(),
            kind: "task".into(),
            title: Some("registry".into()),
            metadata: metadata(&[("parentSessionId", Value::String("ses_parent".into()))]),
            run: Box::new(move |_| {
                finish_rx.recv().unwrap();
                Ok("done".into())
            }),
            on_complete: None,
            on_cancel: None,
        });
        assert_eq!(info.status, Status::Running);

        finish_tx.send(()).unwrap();
        let waited = wait("ses_bg_registry", Some(Duration::from_secs(2)));
        assert!(!waited.timed_out);
        assert_eq!(waited.info.unwrap().output.as_deref(), Some("done"));

        reset_for_tests();
        assert!(status("ses_bg_registry").is_none());
    }

    #[test]
    fn promote_unblocks_foreground_wait_without_finishing() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_for_tests();
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        start(StartInput {
            id: "ses_bg_promote".into(),
            kind: "task".into(),
            title: None,
            metadata: metadata(&[("parentSessionId", Value::String("ses_parent".into()))]),
            run: Box::new(move |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok("promoted done".into())
            }),
            on_complete: None,
            on_cancel: None,
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let promoted = promote("ses_bg_promote").unwrap();
        assert!(is_promoted(&promoted));

        let waited = wait_done_or_promoted("ses_bg_promote").unwrap();
        assert_eq!(waited.status, Status::Running);
        assert!(is_promoted(&waited));

        finish_tx.send(()).unwrap();
        let completed = wait("ses_bg_promote", Some(Duration::from_secs(2)))
            .info
            .unwrap();
        assert_eq!(completed.status, Status::Completed);
        assert_eq!(completed.output.as_deref(), Some("promoted done"));
    }

    #[test]
    fn cancel_marks_running_jobs_and_calls_cancel_hook() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_for_tests();
        let (started_tx, started_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        start(StartInput {
            id: "ses_bg_cancel".into(),
            kind: "task".into(),
            title: None,
            metadata: metadata(&[]),
            run: Box::new(move |control| {
                started_tx.send(()).unwrap();
                while !control.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err("cancelled".into())
            }),
            on_complete: None,
            on_cancel: Some(Box::new(move || {
                cancel_tx.send(()).unwrap();
            })),
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let cancelled = cancel("ses_bg_cancel").unwrap();
        assert_eq!(cancelled.status, Status::Cancelled);
        cancel_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let waited = wait("ses_bg_cancel", Some(Duration::from_secs(2)))
            .info
            .unwrap();
        assert_eq!(waited.status, Status::Cancelled);
    }

    #[test]
    fn extend_reports_only_running_jobs() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_for_tests();
        let (finish_tx, finish_rx) = mpsc::channel();
        start(StartInput {
            id: "ses_bg_extend".into(),
            kind: "task".into(),
            title: None,
            metadata: metadata(&[]),
            run: Box::new(move |_| {
                finish_rx.recv().unwrap();
                Ok("done".into())
            }),
            on_complete: None,
            on_cancel: None,
        });
        assert!(extend("ses_bg_extend"));
        finish_tx.send(()).unwrap();
        let _ = wait("ses_bg_extend", Some(Duration::from_secs(2)));
        assert!(!extend("ses_bg_extend"));
    }
}
