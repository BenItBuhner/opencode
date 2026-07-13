//! Port of packages/core/src/permission.ts + packages/core/src/permission/saved.ts.
//!
//! Runtime is process-local: pending permission requests live in an in-memory
//! registry that outlives individual HTTP handlers so a `POST .../permission`
//! from a client can settle a tool-side `assert` waiting on the same request
//! ID. Saved permissions persist in the `permission` SQLite table shared with
//! the Bun server.

use crate::identifier;
use crate::runner::permission as perm_eval;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Reply values the client can send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Once,
    Always,
    Reject(Option<String>),
}

impl Reply {
    fn as_str(&self) -> &'static str {
        match self {
            Reply::Once => "once",
            Reply::Always => "always",
            Reply::Reject(_) => "reject",
        }
    }
}

/// Result of a resolved wait on a pending permission (matches
/// `Deferred<void, DeclinedError | CorrectedError>` on the TS side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Allowed,
    Declined,
    Corrected(String),
}

/// AssertInput mirror: describes a permission request the caller (the tool
/// registry) wants to evaluate + optionally admit.
#[derive(Debug, Clone)]
pub struct AssertInput {
    pub id: Option<String>,
    pub session_id: String,
    pub action: String,
    pub resources: Vec<String>,
    pub save: Option<Vec<String>>,
    pub metadata: Option<Value>,
    pub source: Option<Value>,
    pub agent: Option<String>,
}

/// AskResult (Schema.Struct { id, effect }) in field order.
pub fn ask_result_json(id: &str, effect: &str) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(id.to_string()));
    out.insert("effect".into(), Value::String(effect.to_string()));
    Value::Object(out)
}

/// Permission.Request encoded key order:
///   id, sessionID, action, resources, save?, metadata?, source?.
pub fn request_json(request: &Request) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(request.id.clone()));
    out.insert(
        "sessionID".into(),
        Value::String(request.session_id.clone()),
    );
    out.insert("action".into(), Value::String(request.action.clone()));
    out.insert(
        "resources".into(),
        Value::Array(
            request
                .resources
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    if let Some(save) = &request.save {
        out.insert(
            "save".into(),
            Value::Array(save.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(metadata) = &request.metadata {
        out.insert("metadata".into(), metadata.clone());
    }
    if let Some(source) = &request.source {
        out.insert("source".into(), source.clone());
    }
    Value::Object(out)
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub session_id: String,
    pub action: String,
    pub resources: Vec<String>,
    pub save: Option<Vec<String>>,
    pub metadata: Option<Value>,
    pub source: Option<Value>,
}

struct Pending {
    request: Request,
    agent: Option<String>,
    tx: SyncSender<Resolution>,
}

type SnapshotItem = (String, String, Option<String>, Vec<String>, Vec<String>);

/// Process-local pending Permission registry: one Location's Permission
/// service in TS terms.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    pending: Vec<Pending>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the currently pending requests.
    pub fn list(&self) -> Vec<Request> {
        self.inner
            .lock()
            .expect("permission registry poisoned")
            .pending
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

    /// Enqueue a pending request. Returns a receiver the caller (tool
    /// runtime) uses to wait for the resolution.
    fn enqueue(
        &self,
        request: Request,
        agent: Option<String>,
    ) -> Result<std::sync::mpsc::Receiver<Resolution>, String> {
        let (tx, rx) = sync_channel::<Resolution>(1);
        let mut guard = self.inner.lock().expect("permission registry poisoned");
        if guard
            .pending
            .iter()
            .any(|item| item.request.id == request.id)
        {
            return Err(format!("Duplicate pending permission ID: {}", request.id));
        }
        guard.pending.push(Pending { request, agent, tx });
        Ok(rx)
    }

    fn remove(&self, id: &str) -> Option<Pending> {
        let mut guard = self.inner.lock().expect("permission registry poisoned");
        guard
            .pending
            .iter()
            .position(|item| item.request.id == id)
            .map(|index| guard.pending.remove(index))
    }

    fn take_matching(&self, session_id: &str) -> Vec<Pending> {
        let mut guard = self.inner.lock().expect("permission registry poisoned");
        let mut out = Vec::new();
        let mut index = 0;
        while index < guard.pending.len() {
            if guard.pending[index].request.session_id == session_id {
                out.push(guard.pending.remove(index));
            } else {
                index += 1;
            }
        }
        out
    }

    fn snapshot_all(&self) -> Vec<SnapshotItem> {
        let guard = self.inner.lock().expect("permission registry poisoned");
        guard
            .pending
            .iter()
            .map(|item| {
                (
                    item.request.id.clone(),
                    item.request.session_id.clone(),
                    item.agent.clone(),
                    item.request.resources.clone(),
                    vec![item.request.action.clone()],
                )
            })
            .collect()
    }
}

/// Effect of an evaluation (mirrors Permission.Effect literal).
pub type Effect = perm_eval::Effect;

/// Evaluate a request against the agent ruleset and any session overrides
/// plus saved rules pulled from the DB. Mirrors `evaluateInput`.
pub fn evaluate_input(
    conn: &Connection,
    project_id: &str,
    session_id: &str,
    agent: &str,
    action: &str,
    resources: &[&str],
) -> Effect {
    let session_rules = perm_eval::session_rules(conn, session_id);
    // First compute against agent rules only — agent-rule denials win outright.
    let denied = resources.iter().any(|resource| {
        perm_eval::evaluate_with(agent, &session_rules, action, &[*resource]) == Effect::Deny
    });
    if denied {
        return Effect::Deny;
    }
    // Otherwise fold saved rules in as `allow` for the exact (action, resource).
    let saved = saved_list(conn, Some(project_id)).unwrap_or_default();
    let extra: Vec<(String, String, String)> = saved
        .into_iter()
        .map(|row| (row.action, row.resource, "allow".to_string()))
        .collect();
    let combined: Vec<(String, String, String)> = session_rules.into_iter().chain(extra).collect();
    perm_eval::evaluate_with(agent, &combined, action, resources)
}

fn effect_str(effect: Effect) -> &'static str {
    match effect {
        Effect::Allow => "allow",
        Effect::Ask => "ask",
        Effect::Deny => "deny",
    }
}

/// Errors surfaced through the /api HTTP handlers.
#[derive(Debug, Clone)]
pub enum ApiError {
    /// The referenced request is not pending in this process (404).
    NotFound { request_id: String },
}

/// AskOutcome result of `ask`.
pub struct AskOutcome {
    pub id: String,
    pub effect: &'static str,
    /// Receiver present when `effect == "ask"`; the tool runtime should
    /// wait on it until the client posts a reply.
    pub wait: Option<std::sync::mpsc::Receiver<Resolution>>,
}

pub struct Env<'a> {
    pub bus: &'a crate::bus::Bus,
    pub conn: &'a Connection,
    pub project_id: &'a str,
    pub registry: &'a Registry,
}

/// Port of PermissionV2.ask. Evaluates; on `ask` it durably admits a pending
/// request into the registry and publishes the transient asked event so
/// clients can see it via SSE.
pub fn ask(env: &Env, input: &AssertInput) -> Result<AskOutcome, ApiError> {
    let resource_refs: Vec<&str> = input.resources.iter().map(String::as_str).collect();
    let effect = evaluate_input(
        env.conn,
        env.project_id,
        &input.session_id,
        input.agent.as_deref().unwrap_or("build"),
        &input.action,
        &resource_refs,
    );
    let effect_label = effect_str(effect);
    let request = Request {
        id: input
            .id
            .clone()
            .unwrap_or_else(|| format!("per_{}", identifier::ascending())),
        session_id: input.session_id.clone(),
        action: input.action.clone(),
        resources: input.resources.clone(),
        save: input.save.clone(),
        metadata: input.metadata.clone(),
        source: input.source.clone(),
    };
    let wait = if effect == Effect::Ask {
        let receiver = env
            .registry
            .enqueue(request.clone(), input.agent.clone())
            .map_err(|_| ApiError::NotFound {
                request_id: request.id.clone(),
            })?;
        env.bus
            .publish("permission.v2.asked", request_json(&request));
        Some(receiver)
    } else {
        None
    };
    Ok(AskOutcome {
        id: request.id,
        effect: effect_label,
        wait,
    })
}

/// Port of PermissionV2.reply: settle the pending deferred with the reply.
/// Rejecting cascades declines to every other pending request in the same
/// Session (matching TS behavior).
pub fn reply(env: &Env, request_id: &str, reply: Reply) -> Result<(), ApiError> {
    let Some(existing) = env.registry.remove(request_id) else {
        return Err(ApiError::NotFound {
            request_id: request_id.to_string(),
        });
    };
    // Publish the Replied event before settling (mirrors TS ordering).
    env.bus.publish(
        "permission.v2.replied",
        json!({
            "sessionID": existing.request.session_id,
            "requestID": existing.request.id,
            "reply": reply.as_str(),
        }),
    );

    match &reply {
        Reply::Reject(message) => {
            let resolution = match message {
                Some(message) if !message.is_empty() => Resolution::Corrected(message.clone()),
                _ => Resolution::Declined,
            };
            let _ = existing.tx.send(resolution);
            // Cascade decline for any other pending item in this Session.
            for pending in env.registry.take_matching(&existing.request.session_id) {
                env.bus.publish(
                    "permission.v2.replied",
                    json!({
                        "sessionID": pending.request.session_id,
                        "requestID": pending.request.id,
                        "reply": "reject",
                    }),
                );
                let _ = pending.tx.send(Resolution::Declined);
            }
            return Ok(());
        }
        Reply::Once => {
            let _ = existing.tx.send(Resolution::Allowed);
        }
        Reply::Always => {
            let _ = existing.tx.send(Resolution::Allowed);
            if let Some(save) = &existing.request.save {
                if !save.is_empty() {
                    let _ = saved_add(
                        env.conn,
                        env.project_id,
                        &existing.request.action,
                        save.iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .as_slice(),
                    );
                }
            }
            // Re-evaluate every remaining pending request: if the new rules
            // (session + saved) make them fully allow, settle them silently.
            let snapshot = env.registry.snapshot_all();
            for (id, session_id, agent, resources, actions) in snapshot {
                let action = actions.first().cloned().unwrap_or_default();
                let resource_refs: Vec<&str> = resources.iter().map(String::as_str).collect();
                let new_effect = evaluate_input(
                    env.conn,
                    env.project_id,
                    &session_id,
                    agent.as_deref().unwrap_or("build"),
                    &action,
                    &resource_refs,
                );
                if new_effect != Effect::Allow {
                    continue;
                }
                if let Some(pending) = env.registry.remove(&id) {
                    env.bus.publish(
                        "permission.v2.replied",
                        json!({
                            "sessionID": pending.request.session_id,
                            "requestID": pending.request.id,
                            "reply": "always",
                        }),
                    );
                    let _ = pending.tx.send(Resolution::Allowed);
                }
            }
        }
    }
    Ok(())
}

/// Await a pending permission with cancellation support. Returns Declined
/// when the wait is cancelled to keep the tool runtime paths simple.
pub fn wait_for(
    rx: std::sync::mpsc::Receiver<Resolution>,
    cancelled: impl Fn() -> bool,
) -> Resolution {
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(resolution) => return resolution,
            Err(RecvTimeoutError::Timeout) => {
                if cancelled() {
                    return Resolution::Declined;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Resolution::Declined,
        }
    }
}

// ---------------------------------------------------------------------------
// Saved permissions (persistent, shared with Bun via `permission` table)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub id: String,
    pub project_id: String,
    pub action: String,
    pub resource: String,
}

/// Encoded key order:  id, projectID, action, resource.
pub fn saved_json(saved: &Saved) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), Value::String(saved.id.clone()));
    out.insert("projectID".into(), Value::String(saved.project_id.clone()));
    out.insert("action".into(), Value::String(saved.action.clone()));
    out.insert("resource".into(), Value::String(saved.resource.clone()));
    Value::Object(out)
}

pub fn saved_list(conn: &Connection, project_id: Option<&str>) -> rusqlite::Result<Vec<Saved>> {
    let mut sql = "SELECT id, project_id, action, resource FROM permission".to_string();
    let rows = if let Some(project_id) = project_id {
        sql.push_str(" WHERE project_id = ?");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map([project_id], |row| {
                Ok(Saved {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    action: row.get(2)?,
                    resource: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    } else {
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Saved {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    action: row.get(2)?,
                    resource: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    Ok(rows)
}

pub fn saved_add(
    conn: &Connection,
    project_id: &str,
    action: &str,
    resources: &[&str],
) -> rusqlite::Result<()> {
    if resources.is_empty() {
        return Ok(());
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO permission (id, project_id, action, resource, time_created, time_updated) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )?;
    for resource in resources {
        stmt.execute(rusqlite::params![
            format!("psv_{}", identifier::ascending()),
            project_id,
            action,
            resource,
            timestamp,
            timestamp,
        ])?;
    }
    Ok(())
}

pub fn saved_remove(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM permission WHERE id = ?", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE session (
                id text PRIMARY KEY, permission text
             );
             CREATE TABLE permission (
                id text PRIMARY KEY,
                project_id text NOT NULL,
                action text NOT NULL,
                resource text NOT NULL,
                time_created integer NOT NULL,
                time_updated integer NOT NULL,
                UNIQUE(project_id, action, resource)
             );",
        )
        .unwrap();
    }

    fn env<'a>(
        bus: &'a crate::bus::Bus,
        conn: &'a Connection,
        registry: &'a Registry,
        project: &'a str,
    ) -> Env<'a> {
        Env {
            bus,
            conn,
            project_id: project,
            registry,
        }
    }

    #[test]
    fn allow_returns_no_wait() {
        let bus = crate::bus::Bus::new();
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let registry = Registry::new();
        let env = env(&bus, &conn, &registry, "prj");
        let outcome = ask(
            &env,
            &AssertInput {
                id: None,
                session_id: "ses_x".into(),
                action: "read".into(),
                resources: vec!["src/main.rs".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap();
        assert_eq!(outcome.effect, "allow");
        assert!(outcome.wait.is_none());
    }

    #[test]
    fn deny_returns_no_wait_and_effect_deny() {
        let bus = crate::bus::Bus::new();
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let registry = Registry::new();
        let env = env(&bus, &conn, &registry, "prj");
        let outcome = ask(
            &env,
            &AssertInput {
                id: None,
                session_id: "ses_x".into(),
                action: "question".into(),
                resources: vec!["*".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("explore".into()),
            },
        )
        .unwrap();
        assert_eq!(outcome.effect, "deny");
        assert!(outcome.wait.is_none());
    }

    #[test]
    fn ask_publishes_asked_and_waits_for_reply_once() {
        let bus = crate::bus::Bus::new();
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let registry = Registry::new();
        let mut receiver = bus.subscribe_v2();

        let env_here = env(&bus, &conn, &registry, "prj");
        let outcome = ask(
            &env_here,
            &AssertInput {
                id: Some("per_test0000000000000000000".into()),
                session_id: "ses_x".into(),
                action: "read".into(),
                resources: vec![".env".into()],
                save: Some(vec![".env".into()]),
                metadata: Some(json!({ "kind": "file" })),
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap();
        assert_eq!(outcome.effect, "ask");
        let wait = outcome.wait.unwrap();
        assert_eq!(registry.list().len(), 1);

        // Consume the asked event first (drop the message).
        let asked = receiver.try_recv().unwrap();
        assert_eq!(asked["type"], "permission.v2.asked");
        assert_eq!(asked["data"]["id"], "per_test0000000000000000000");
        assert_eq!(asked["data"]["action"], "read");

        reply(&env_here, "per_test0000000000000000000", Reply::Once).unwrap();
        let replied = receiver.try_recv().unwrap();
        assert_eq!(replied["type"], "permission.v2.replied");
        assert_eq!(replied["data"]["reply"], "once");
        assert!(registry.list().is_empty());
        assert_eq!(wait.recv().unwrap(), Resolution::Allowed);
    }

    #[test]
    fn reject_cascades_to_other_pending_in_same_session() {
        let bus = crate::bus::Bus::new();
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let registry = Registry::new();
        let env_here = env(&bus, &conn, &registry, "prj");
        let first = ask(
            &env_here,
            &AssertInput {
                id: Some("per_first000000000000000000".into()),
                session_id: "ses_a".into(),
                action: "read".into(),
                resources: vec![".env".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap()
        .wait
        .unwrap();
        let second = ask(
            &env_here,
            &AssertInput {
                id: Some("per_secondaaaaaaaaaaaaaaaaa".into()),
                session_id: "ses_a".into(),
                action: "read".into(),
                resources: vec!["config/.env".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap()
        .wait
        .unwrap();
        // Third belongs to a different session and must NOT be cascaded.
        let third = ask(
            &env_here,
            &AssertInput {
                id: Some("per_thirdaaaaaaaaaaaaaaaaaa".into()),
                session_id: "ses_b".into(),
                action: "read".into(),
                resources: vec![".env".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap()
        .wait
        .unwrap();

        reply(
            &env_here,
            "per_first000000000000000000",
            Reply::Reject(Some("nope".into())),
        )
        .unwrap();
        assert_eq!(first.recv().unwrap(), Resolution::Corrected("nope".into()));
        assert_eq!(second.recv().unwrap(), Resolution::Declined);
        // Third is untouched.
        assert!(third.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(registry.for_session("ses_b").len(), 1);
    }

    #[test]
    fn always_persists_save_and_settles_other_matching_pending() {
        let bus = crate::bus::Bus::new();
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let registry = Registry::new();
        let env_here = env(&bus, &conn, &registry, "prj-1");
        let first = ask(
            &env_here,
            &AssertInput {
                id: Some("per_saveaaaaaaaaaaaaaaaaaaa".into()),
                session_id: "ses_x".into(),
                action: "read".into(),
                resources: vec![".env.local".into()],
                save: Some(vec![".env.local".into()]),
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap()
        .wait
        .unwrap();
        let secondary = ask(
            &env_here,
            &AssertInput {
                id: Some("per_matchaaaaaaaaaaaaaaaaaa".into()),
                session_id: "ses_y".into(),
                action: "read".into(),
                resources: vec![".env.local".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            },
        )
        .unwrap()
        .wait
        .unwrap();

        reply(&env_here, "per_saveaaaaaaaaaaaaaaaaaaa", Reply::Always).unwrap();
        assert_eq!(first.recv().unwrap(), Resolution::Allowed);
        assert_eq!(secondary.recv().unwrap(), Resolution::Allowed);

        let saved = saved_list(&conn, Some("prj-1")).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].resource, ".env.local");
        assert_eq!(saved[0].action, "read");
    }

    #[test]
    fn saved_add_dedupes_and_remove_deletes_by_id() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        saved_add(&conn, "prj", "read", &[".env", ".env"]).unwrap();
        let rows = saved_list(&conn, Some("prj")).unwrap();
        assert_eq!(rows.len(), 1);
        saved_remove(&conn, &rows[0].id).unwrap();
        assert!(saved_list(&conn, Some("prj")).unwrap().is_empty());
    }

    #[test]
    fn request_json_key_order_matches_schema() {
        let json = request_json(&Request {
            id: "per_1".into(),
            session_id: "ses_1".into(),
            action: "read".into(),
            resources: vec!["a".into()],
            save: Some(vec!["a".into()]),
            metadata: Some(json!({ "k": "v" })),
            source: Some(json!({ "type": "tool", "messageID": "m", "callID": "c" })),
        });
        assert_eq!(
            json.to_string(),
            r#"{"id":"per_1","sessionID":"ses_1","action":"read","resources":["a"],"save":["a"],"metadata":{"k":"v"},"source":{"type":"tool","messageID":"m","callID":"c"}}"#
        );
    }
}
