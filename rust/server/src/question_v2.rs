//! Port of packages/core/src/question.ts.
//!
//! Runtime: pending question requests live in a process-local registry that
//! outlives HTTP handlers. Tools call `ask(...)` which enqueues the request
//! and returns a `Waiter` the tool runtime blocks on until the client posts a
//! reply or reject, or the built-in timeout elapses.

use crate::identifier;
use serde_json::{json, Map, Value};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 5 * 60;
pub const MAX_TIMEOUT_SECONDS: u64 = 10 * 60;

/// Message shown when the user fails to answer before the timeout elapses.
pub fn timeout_message(timeout: u64) -> String {
    format!("User failed to answer in time ({timeout}s)")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Answered(Vec<Vec<String>>),
    Rejected,
}

/// QuestionV2.Info key order: question, header, options, multiple?, custom?.
pub fn info_json(info: &Info) -> Value {
    let mut out = Map::new();
    out.insert("question".into(), Value::String(info.question.clone()));
    out.insert("header".into(), Value::String(info.header.clone()));
    out.insert(
        "options".into(),
        Value::Array(info.options.iter().map(option_json).collect()),
    );
    if let Some(multiple) = info.multiple {
        out.insert("multiple".into(), Value::Bool(multiple));
    }
    if let Some(custom) = info.custom {
        out.insert("custom".into(), Value::Bool(custom));
    }
    Value::Object(out)
}

/// QuestionV2.Option key order: label, description.
pub fn option_json(option: &QOption) -> Value {
    let mut out = Map::new();
    out.insert("label".into(), Value::String(option.label.clone()));
    out.insert(
        "description".into(),
        Value::String(option.description.clone()),
    );
    Value::Object(out)
}

/// QuestionV2.Tool key order: messageID, callID.
pub fn tool_json(tool: &Tool) -> Value {
    let mut out = Map::new();
    out.insert("messageID".into(), Value::String(tool.message_id.clone()));
    out.insert("callID".into(), Value::String(tool.call_id.clone()));
    Value::Object(out)
}

/// QuestionV2.Request key order: id, sessionID, questions, tool?, timeout?.
pub fn request_json(request: &Request) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(request.id.clone()));
    out.insert(
        "sessionID".into(),
        Value::String(request.session_id.clone()),
    );
    out.insert(
        "questions".into(),
        Value::Array(request.questions.iter().map(info_json).collect()),
    );
    if let Some(tool) = &request.tool {
        out.insert("tool".into(), tool_json(tool));
    }
    if let Some(timeout) = request.timeout {
        out.insert("timeout".into(), Value::Number(timeout.into()));
    }
    Value::Object(out)
}

#[derive(Debug, Clone)]
pub struct QOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct Info {
    pub question: String,
    pub header: String,
    pub options: Vec<QOption>,
    pub multiple: Option<bool>,
    pub custom: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub message_id: String,
    pub call_id: String,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<Info>,
    pub tool: Option<Tool>,
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AskInput {
    pub session_id: String,
    pub questions: Vec<Info>,
    pub tool: Option<Tool>,
    pub timeout: Option<u64>,
}

struct Pending {
    request: Request,
    tx: SyncSender<Resolution>,
}

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Vec<Pending>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<Request> {
        self.inner
            .lock()
            .expect("question registry poisoned")
            .iter()
            .map(|item| item.request.clone())
            .collect()
    }

    pub fn for_session(&self, session_id: &str) -> Vec<Request> {
        self.list()
            .into_iter()
            .filter(|item| item.session_id == session_id)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Request> {
        self.list().into_iter().find(|item| item.id == id)
    }

    fn remove(&self, id: &str) -> Option<Pending> {
        let mut guard = self.inner.lock().expect("question registry poisoned");
        guard
            .iter()
            .position(|item| item.request.id == id)
            .map(|index| guard.remove(index))
    }

    fn enqueue(&self, request: Request) -> std::sync::mpsc::Receiver<Resolution> {
        let (tx, rx) = sync_channel::<Resolution>(1);
        self.inner
            .lock()
            .expect("question registry poisoned")
            .push(Pending { request, tx });
        rx
    }
}

#[derive(Debug, Clone)]
pub enum ApiError {
    NotFound {
        #[allow(dead_code)]
        request_id: String,
    },
}

pub struct Env<'a> {
    pub bus: &'a crate::bus::Bus,
    pub registry: &'a Registry,
}

/// Ask outcome. `wait` receives once the client replies/rejects or the
/// deadline elapses (matching QuestionV2.ask + timeoutOrElse).
pub struct AskOutcome {
    pub id: String,
    pub wait: std::sync::mpsc::Receiver<Resolution>,
    pub timeout_seconds: u64,
    pub question_count: usize,
}

pub fn ask(env: &Env, input: &AskInput) -> AskOutcome {
    let timeout = clamp_timeout(input.timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS));
    let request = Request {
        id: format!("que_{}", identifier::ascending()),
        session_id: input.session_id.clone(),
        questions: input.questions.clone(),
        tool: input.tool.clone(),
        timeout: Some(timeout),
    };
    let question_count = request.questions.len();
    let rx = env.registry.enqueue(request.clone());
    env.bus.publish("question.v2.asked", request_json(&request));
    AskOutcome {
        id: request.id,
        wait: rx,
        timeout_seconds: timeout,
        question_count,
    }
}

/// Await the reply/reject with cancellation and timeout support (mirrors the
/// TS `timeoutOrElse`). Timeouts settle by publishing a Replied event with
/// the per-question timeout string, matching the TS behavior.
pub fn wait_for(env: &Env, outcome: AskOutcome, cancelled: impl Fn() -> bool) -> Resolution {
    let deadline = std::time::Instant::now() + Duration::from_secs(outcome.timeout_seconds);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let tick = remaining.min(Duration::from_millis(200));
        match outcome.wait.recv_timeout(tick) {
            Ok(resolution) => return resolution,
            Err(RecvTimeoutError::Timeout) => {
                if cancelled() {
                    // Silent cancel: remove pending row without publishing a
                    // reply — the tool caller will surface the interrupt
                    // through the settlement path.
                    env.registry.remove(&outcome.id);
                    return Resolution::Rejected;
                }
                if std::time::Instant::now() >= deadline {
                    // Timeout path: publish Replied with per-question
                    // timeout messages and settle.
                    let answers: Vec<Vec<String>> = (0..outcome.question_count)
                        .map(|_| vec![timeout_message(outcome.timeout_seconds)])
                        .collect();
                    let request = env.registry.remove(&outcome.id);
                    if let Some(pending) = request {
                        env.bus.publish(
                            "question.v2.replied",
                            json!({
                                "sessionID": pending.request.session_id,
                                "requestID": pending.request.id,
                                "answers": answers.iter().map(|answers| Value::Array(
                                    answers.iter().cloned().map(Value::String).collect()
                                )).collect::<Vec<_>>(),
                            }),
                        );
                    }
                    return Resolution::Answered(answers);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Resolution::Rejected,
        }
    }
}

fn clamp_timeout(seconds: u64) -> u64 {
    seconds.clamp(1, MAX_TIMEOUT_SECONDS)
}

pub fn reply(env: &Env, request_id: &str, answers: Vec<Vec<String>>) -> Result<(), ApiError> {
    let Some(existing) = env.registry.remove(request_id) else {
        return Err(ApiError::NotFound {
            request_id: request_id.to_string(),
        });
    };
    env.bus.publish(
        "question.v2.replied",
        json!({
            "sessionID": existing.request.session_id,
            "requestID": existing.request.id,
            "answers": answers.iter().map(|answers| Value::Array(
                answers.iter().cloned().map(Value::String).collect()
            )).collect::<Vec<_>>(),
        }),
    );
    let _ = existing.tx.send(Resolution::Answered(answers));
    Ok(())
}

pub fn reject(env: &Env, request_id: &str) -> Result<(), ApiError> {
    let Some(existing) = env.registry.remove(request_id) else {
        return Err(ApiError::NotFound {
            request_id: request_id.to_string(),
        });
    };
    env.bus.publish(
        "question.v2.rejected",
        json!({
            "sessionID": existing.request.session_id,
            "requestID": existing.request.id,
        }),
    );
    let _ = existing.tx.send(Resolution::Rejected);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(bus: &'a crate::bus::Bus, registry: &'a Registry) -> Env<'a> {
        Env { bus, registry }
    }

    fn options() -> Vec<QOption> {
        vec![
            QOption {
                label: "Yes".into(),
                description: "Confirm".into(),
            },
            QOption {
                label: "No".into(),
                description: "Cancel".into(),
            },
        ]
    }

    #[test]
    fn ask_and_reply_settles_pending_and_publishes_events() {
        let bus = crate::bus::Bus::new();
        let registry = Registry::new();
        let mut sub = bus.subscribe_v2();
        let env_here = env(&bus, &registry);
        let outcome = ask(
            &env_here,
            &AskInput {
                session_id: "ses_x".into(),
                questions: vec![Info {
                    question: "Continue?".into(),
                    header: "confirm".into(),
                    options: options(),
                    multiple: Some(false),
                    custom: Some(true),
                }],
                tool: Some(Tool {
                    message_id: "msg_1".into(),
                    call_id: "call_1".into(),
                }),
                timeout: None,
            },
        );
        assert_eq!(registry.list().len(), 1);
        let asked = sub.try_recv().unwrap();
        assert_eq!(asked["type"], "question.v2.asked");
        assert_eq!(asked["data"]["questions"][0]["question"], "Continue?");
        let id = outcome.id.clone();
        reply(&env_here, &id, vec![vec!["Yes".into()]]).unwrap();
        let replied = sub.try_recv().unwrap();
        assert_eq!(replied["type"], "question.v2.replied");
        let resolution = outcome.wait.recv().unwrap();
        assert_eq!(resolution, Resolution::Answered(vec![vec!["Yes".into()]]));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn reject_removes_pending_and_returns_rejected() {
        let bus = crate::bus::Bus::new();
        let registry = Registry::new();
        let env_here = env(&bus, &registry);
        let outcome = ask(
            &env_here,
            &AskInput {
                session_id: "ses_y".into(),
                questions: vec![Info {
                    question: "?".into(),
                    header: "h".into(),
                    options: vec![],
                    multiple: None,
                    custom: None,
                }],
                tool: None,
                timeout: Some(5),
            },
        );
        reject(&env_here, &outcome.id).unwrap();
        assert_eq!(outcome.wait.recv().unwrap(), Resolution::Rejected);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn reply_for_missing_request_returns_not_found() {
        let bus = crate::bus::Bus::new();
        let registry = Registry::new();
        let env_here = env(&bus, &registry);
        let err = reply(&env_here, "que_missing", vec![vec![]]).unwrap_err();
        assert!(matches!(err, ApiError::NotFound { .. }));
    }

    #[test]
    fn clamp_bounds_timeout_between_1_and_max() {
        assert_eq!(clamp_timeout(0), 1);
        assert_eq!(clamp_timeout(1), 1);
        assert_eq!(clamp_timeout(30), 30);
        assert_eq!(clamp_timeout(9999), MAX_TIMEOUT_SECONDS);
    }

    #[test]
    fn info_and_request_keys_serialize_in_schema_order() {
        let info = info_json(&Info {
            question: "q".into(),
            header: "h".into(),
            options: vec![QOption {
                label: "l".into(),
                description: "d".into(),
            }],
            multiple: Some(true),
            custom: Some(false),
        });
        assert_eq!(
            info.to_string(),
            r#"{"question":"q","header":"h","options":[{"label":"l","description":"d"}],"multiple":true,"custom":false}"#
        );

        let request = request_json(&Request {
            id: "que_1".into(),
            session_id: "ses_1".into(),
            questions: vec![],
            tool: Some(Tool {
                message_id: "m".into(),
                call_id: "c".into(),
            }),
            timeout: Some(30),
        });
        assert_eq!(
            request.to_string(),
            r#"{"id":"que_1","sessionID":"ses_1","questions":[],"tool":{"messageID":"m","callID":"c"},"timeout":30}"#
        );
    }
}
