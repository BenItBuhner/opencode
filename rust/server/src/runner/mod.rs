//! The serialized Session runner: a Rust port of
//! packages/core/src/session/runner/llm.ts scoped to OpenAI-compatible
//! providers (OpenCode Zen) and the read-only tool registry.
//!
//! One advisory wake drains eligible durable inbox rows: pending steers are
//! promoted into visible user messages, exactly one `llm.stream(request)`
//! provider turn runs per step, tool calls settle durably, and the loop
//! continues until the Session settles. Queue inputs open FIFO activities
//! after the active one settles. All durable writes go through the same
//! event + projection transactions the Bun server uses, so either server can
//! continue a Session the other started.

pub mod context;
pub mod goal;
pub mod llm;
pub mod permission;
pub mod publish;
pub mod tools;
pub mod translate;

use crate::identifier;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;

type Pool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone)]
pub struct Env {
    pub pool: Pool,
    pub worktree: String,
}

/// Guard against unbounded tool loops on free models; the Bun runner bounds
/// steps only when the agent configures a limit.
const MAX_STEPS: i64 = 24;

/// Process-local Session run coordinator: one active drain per Session,
/// wakeups during a drain coalesce into one re-check.
static RUNS: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

fn with_runs<T>(f: impl FnOnce(&mut HashMap<String, bool>) -> T) -> T {
    let mut guard = RUNS.lock().expect("runner coordinator poisoned");
    f(guard.get_or_insert_with(HashMap::new))
}

pub fn active() -> Vec<String> {
    with_runs(|runs| runs.keys().cloned().collect())
}

pub fn is_active(session_id: &str) -> bool {
    with_runs(|runs| runs.contains_key(session_id))
}

/// Advisory wake: schedule a drain for the Session unless one is already
/// running, in which case the running drain re-checks the inbox afterwards.
pub fn wake(env: Env, session_id: String) {
    let joined = with_runs(|runs| {
        if let Some(rerun) = runs.get_mut(&session_id) {
            *rerun = true;
            return true;
        }
        runs.insert(session_id.clone(), false);
        false
    });
    if joined {
        return;
    }
    tokio::spawn(async move {
        loop {
            let run_env = env.clone();
            let run_id = session_id.clone();
            let outcome = tokio::task::spawn_blocking(move || drain(&run_env, &run_id)).await;
            if let Ok(Err(error)) = outcome {
                eprintln!("runner drain failed for {session_id}: {error}");
            }
            let rerun = with_runs(|runs| {
                let rerun = runs.get(&session_id).copied().unwrap_or(false);
                if rerun {
                    runs.insert(session_id.clone(), false);
                } else {
                    runs.remove(&session_id);
                }
                rerun
            });
            if !rerun {
                break;
            }
        }
    });
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64
}

fn has_pending(conn: &Connection, session_id: &str, delivery: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM session_input WHERE session_id = ? AND promoted_seq IS NULL AND delivery = ? LIMIT 1",
        [session_id, delivery],
        |_| Ok(true),
    )
    .or_else(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(other),
    })
}

/// Port of SessionRunner.run: promote, run provider turns, continue for tool
/// settlement or accepted steers, then drain queued inputs one at a time.
fn drain(env: &Env, session_id: &str) -> Result<(), String> {
    let mut conn = env.pool.get().map_err(|error| error.to_string())?;
    let has_steer = has_pending(&conn, session_id, "steer").map_err(|error| error.to_string())?;
    let has_queue =
        !has_steer && has_pending(&conn, session_id, "queue").map_err(|error| error.to_string())?;
    if !has_steer && !has_queue {
        return Ok(());
    }
    fail_interrupted_tools(&mut conn, session_id).map_err(|error| error.to_string())?;

    let mut promotion: Option<&str> = Some(if has_steer { "steer" } else { "queue" });
    let mut should_run = true;
    while should_run {
        let mut needs_continuation = true;
        let mut step: i64 = 1;
        while needs_continuation {
            let result = run_turn(env, &mut conn, session_id, promotion, step)?;
            needs_continuation = result.needs_continuation;
            step = result.step + 1;
            promotion = Some("steer");
            if !needs_continuation {
                needs_continuation =
                    has_pending(&conn, session_id, "steer").map_err(|error| error.to_string())?;
            }
        }
        should_run = has_pending(&conn, session_id, "queue").map_err(|error| error.to_string())?;
        promotion = should_run.then_some("queue");
    }
    Ok(())
}

struct TurnResult {
    needs_continuation: bool,
    step: i64,
}

fn run_turn(
    env: &Env,
    conn: &mut r2d2::PooledConnection<SqliteConnectionManager>,
    session_id: &str,
    promotion: Option<&str>,
    step: i64,
) -> Result<TurnResult, String> {
    let session: (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT directory, agent, model FROM session WHERE id = ?",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| error.to_string())?;
    let directory = session.0;
    let agent = session.1.unwrap_or_else(|| "build".into());
    let worktree = if directory.starts_with(&env.worktree) {
        env.worktree.clone()
    } else {
        directory.clone()
    };

    let publisher = publish::Publisher {
        session_id: session_id.to_string(),
    };
    let epoch = context::ensure(conn, session_id, &directory, &worktree)
        .map_err(|error| error.to_string())?;

    let mut current_step = step;
    if let Some(promotion) = promotion {
        let promoted =
            promote(conn, &publisher, session_id, promotion).map_err(|error| error.to_string())?;
        if promoted > 0 {
            current_step = 1;
        }
    }

    let model = match resolve_model(conn, session_id, &directory, &worktree) {
        Ok(model) => model,
        Err(message) => {
            // Surface resolution failures as a failed step (Bun maps
            // SessionRunnerModel errors into RunError the same way).
            let mut failed = Assistant {
                publisher: &publisher,
                session_id,
                agent: &agent,
                model: &Model {
                    id: "unavailable".into(),
                    provider: "unavailable".into(),
                    variant: None,
                    base_url: String::new(),
                    api_key: String::new(),
                },
                id: None,
            };
            failed.fail(conn, &message)?;
            return Ok(TurnResult {
                needs_continuation: false,
                step: current_step,
            });
        }
    };
    let history = translate::entries(conn, session_id, epoch.baseline_seq)
        .map_err(|error| error.to_string())?;
    let is_last_step = current_step >= MAX_STEPS;
    // LLM.request system parts: [agent.info?.system, epoch baseline], which
    // OpenAI Chat lowering joins with a newline.
    let system = match agent_system(&agent) {
        Some(prompt) => format!("{prompt}\n{}", epoch.baseline),
        None => epoch.baseline.clone(),
    };
    let mut messages = translate::to_chat_messages(&system, &history);
    if is_last_step {
        messages.push(json!({ "role": "assistant", "content": MAX_STEPS_PROMPT }));
    }

    // OpenAI Chat body in bodyFields order (openai-chat.ts fromRequest).
    let mut body = Map::new();
    body.insert("model".into(), json!(model.id));
    body.insert("messages".into(), Value::Array(messages));
    if !is_last_step {
        body.insert("tools".into(), Value::Array(tools::definitions_for(&agent)));
    } else {
        body.insert("tool_choice".into(), json!("none"));
    }
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));

    let mut assistant = Assistant {
        publisher: &publisher,
        session_id,
        agent: &agent,
        model: &model,
        id: None,
    };

    let turn = match llm::stream(&model.base_url, &model.api_key, &Value::Object(body)) {
        Ok(turn) => turn,
        Err(error) => {
            assistant.fail(conn, &format!("Provider request failed: {error}"))?;
            return Ok(TurnResult {
                needs_continuation: false,
                step: current_step,
            });
        }
    };
    if let Some(message) = turn.error {
        assistant.fail(conn, &message)?;
        return Ok(TurnResult {
            needs_continuation: false,
            step: current_step,
        });
    }

    let mut needs_continuation = false;
    let mut text_index = 0;
    let mut reasoning_index = 0;
    for part in &turn.parts {
        match part {
            llm::Part::Reasoning(text) => {
                let assistant_id = assistant.ensure(conn)?;
                let id = format!("reasoning-{reasoning_index}");
                reasoning_index += 1;
                publisher
                    .publish(
                        conn,
                        "session.next.reasoning.started",
                        1,
                        &ordered(&[
                            ("timestamp", json!(now())),
                            ("sessionID", json!(session_id)),
                            ("assistantMessageID", json!(assistant_id)),
                            ("reasoningID", json!(id)),
                        ]),
                    )
                    .map_err(|error| error.to_string())?;
                publisher
                    .publish(
                        conn,
                        "session.next.reasoning.ended",
                        1,
                        &ordered(&[
                            ("timestamp", json!(now())),
                            ("sessionID", json!(session_id)),
                            ("assistantMessageID", json!(assistant_id)),
                            ("reasoningID", json!(id)),
                            ("text", json!(text)),
                        ]),
                    )
                    .map_err(|error| error.to_string())?;
            }
            llm::Part::Text(text) => {
                let assistant_id = assistant.ensure(conn)?;
                let id = format!("text-{text_index}");
                text_index += 1;
                publisher
                    .publish(
                        conn,
                        "session.next.text.started",
                        1,
                        &ordered(&[
                            ("timestamp", json!(now())),
                            ("sessionID", json!(session_id)),
                            ("assistantMessageID", json!(assistant_id)),
                            ("textID", json!(id)),
                        ]),
                    )
                    .map_err(|error| error.to_string())?;
                publisher
                    .publish(
                        conn,
                        "session.next.text.ended",
                        1,
                        &ordered(&[
                            ("timestamp", json!(now())),
                            ("sessionID", json!(session_id)),
                            ("assistantMessageID", json!(assistant_id)),
                            ("textID", json!(id)),
                            ("text", json!(text)),
                        ]),
                    )
                    .map_err(|error| error.to_string())?;
            }
            llm::Part::ToolCall {
                id,
                name,
                arguments,
            } => {
                needs_continuation = true;
                let assistant_id = assistant.ensure(conn)?;
                let base = |extra: &[(&str, Value)]| {
                    let mut fields = vec![
                        ("timestamp", json!(now())),
                        ("sessionID", json!(session_id)),
                        ("assistantMessageID", json!(assistant_id)),
                        ("callID", json!(id)),
                    ];
                    fields.extend(extra.iter().cloned());
                    ordered(&fields)
                };
                publisher
                    .publish(
                        conn,
                        "session.next.tool.input.started",
                        1,
                        &base(&[("name", json!(name))]),
                    )
                    .map_err(|error| error.to_string())?;
                publisher
                    .publish(
                        conn,
                        "session.next.tool.input.ended",
                        1,
                        &base(&[("text", json!(arguments))]),
                    )
                    .map_err(|error| error.to_string())?;
                let input = llm::parse_tool_input(arguments);
                publisher
                    .publish(
                        conn,
                        "session.next.tool.called",
                        1,
                        &base(&[
                            ("tool", json!(name)),
                            ("input", input.clone()),
                            ("provider", json!({ "executed": false })),
                        ]),
                    )
                    .map_err(|error| error.to_string())?;
                // Durably record the call before side effects begin, then settle.
                let settlement = tools::execute(
                    &tools::ToolEnv {
                        directory: &directory,
                        worktree: &worktree,
                        agent: &agent,
                        session_id,
                        conn,
                    },
                    name,
                    &input,
                );
                if settlement.interrupt {
                    publisher
                        .publish(
                            conn,
                            "session.next.tool.failed",
                            1,
                            &base(&[
                                (
                                    "error",
                                    json!({
                                        "type": "unknown",
                                        "message": settlement.error.unwrap_or_else(|| "Tool execution interrupted".into()),
                                    }),
                                ),
                                ("provider", json!({ "executed": false })),
                            ]),
                        )
                        .map_err(|error| error.to_string())?;
                    return Ok(TurnResult {
                        needs_continuation: false,
                        step: current_step,
                    });
                }
                match settlement.error {
                    None => publisher
                        .publish(
                            conn,
                            "session.next.tool.success",
                            1,
                            &base(&[
                                ("structured", settlement.structured),
                                ("content", Value::Array(settlement.content)),
                                ("outputPaths", json!([])),
                                ("provider", json!({ "executed": false })),
                            ]),
                        )
                        .map_err(|error| error.to_string())?,
                    Some(message) => publisher
                        .publish(
                            conn,
                            "session.next.tool.failed",
                            1,
                            &base(&[
                                ("error", json!({ "type": "unknown", "message": message })),
                                ("provider", json!({ "executed": false })),
                            ]),
                        )
                        .map_err(|error| error.to_string())?,
                };
            }
        }
    }

    let assistant_id = assistant.ensure(conn)?;
    publisher
        .publish(
            conn,
            "session.next.step.ended",
            2,
            &ordered(&[
                ("timestamp", json!(now())),
                ("sessionID", json!(session_id)),
                ("assistantMessageID", json!(assistant_id)),
                ("finish", json!(turn.finish)),
                ("cost", json!(0)),
                (
                    "tokens",
                    json!({
                        "input": turn.tokens.input,
                        "output": turn.tokens.output,
                        "reasoning": turn.tokens.reasoning,
                        "cache": { "read": turn.tokens.cache_read, "write": 0 },
                    }),
                ),
            ]),
        )
        .map_err(|error| error.to_string())?;

    Ok(TurnResult {
        needs_continuation: needs_continuation && !is_last_step,
        step: current_step,
    })
}

struct Assistant<'a> {
    publisher: &'a publish::Publisher,
    session_id: &'a str,
    agent: &'a str,
    model: &'a Model,
    id: Option<String>,
}

impl Assistant<'_> {
    /// startAssistant: the first output event opens the durable assistant step.
    fn ensure(&mut self, conn: &mut Connection) -> Result<String, String> {
        if let Some(id) = &self.id {
            return Ok(id.clone());
        }
        let id = format!("msg_{}", identifier::ascending());
        let mut model = Map::new();
        model.insert("id".into(), json!(self.model.id));
        model.insert("providerID".into(), json!(self.model.provider));
        if let Some(variant) = &self.model.variant {
            model.insert("variant".into(), json!(variant));
        }
        self.publisher
            .publish(
                conn,
                "session.next.step.started",
                1,
                &ordered(&[
                    ("timestamp", json!(now())),
                    ("sessionID", json!(self.session_id)),
                    ("assistantMessageID", json!(id)),
                    ("agent", json!(self.agent)),
                    ("model", Value::Object(model)),
                ]),
            )
            .map_err(|error| error.to_string())?;
        self.id = Some(id.clone());
        Ok(id)
    }

    fn fail(&mut self, conn: &mut Connection, message: &str) -> Result<(), String> {
        let id = self.ensure(conn)?;
        self.publisher
            .publish(
                conn,
                "session.next.step.failed",
                2,
                &ordered(&[
                    ("timestamp", json!(now())),
                    ("sessionID", json!(self.session_id)),
                    ("assistantMessageID", json!(id)),
                    ("error", json!({ "type": "unknown", "message": message })),
                ]),
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn ordered(fields: &[(&str, Value)]) -> Value {
    let mut out = Map::new();
    for (key, value) in fields {
        out.insert((*key).into(), value.clone());
    }
    Value::Object(out)
}

/// SessionInput.promoteSteers / promoteNextQueued: publish a durable Prompted
/// event (original message ID and admission timestamp) per eligible row.
fn promote(
    conn: &mut Connection,
    publisher: &publish::Publisher,
    session_id: &str,
    promotion: &str,
) -> rusqlite::Result<usize> {
    let cutoff: i64 = conn
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?",
            [session_id],
            |row| row.get(0),
        )
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(-1),
            other => Err(other),
        })?;
    let mut rows: Vec<(String, String, String, i64)> = vec![];
    if promotion == "queue" {
        let mut statement = conn.prepare_cached(
            "SELECT id, prompt, delivery, time_created FROM session_input \
             WHERE session_id = ? AND promoted_seq IS NULL AND delivery = 'queue' \
             ORDER BY admitted_seq ASC LIMIT 1",
        )?;
        let queued = statement
            .query_map([session_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        rows.extend(queued);
    }
    {
        let mut statement = conn.prepare_cached(
            "SELECT id, prompt, delivery, time_created FROM session_input \
             WHERE session_id = ? AND promoted_seq IS NULL AND delivery = 'steer' AND admitted_seq <= ? \
             ORDER BY admitted_seq ASC",
        )?;
        let steers = statement
            .query_map(rusqlite::params![session_id, cutoff], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        rows.extend(steers);
    }
    for (id, prompt, delivery, time_created) in &rows {
        let prompt: Value = serde_json::from_str(prompt).unwrap_or(Value::Null);
        publisher.publish(
            conn,
            "session.next.prompted",
            1,
            &ordered(&[
                ("timestamp", json!(time_created)),
                ("sessionID", json!(session_id)),
                ("messageID", json!(id)),
                ("prompt", prompt),
                ("delivery", json!(delivery)),
            ]),
        )?;
    }
    Ok(rows.len())
}

/// failInterruptedTools: settle stale pending/running tool projections from a
/// previous crashed drain before starting new provider work.
fn fail_interrupted_tools(conn: &mut Connection, session_id: &str) -> rusqlite::Result<()> {
    let rows: Vec<(String, String)> = {
        let mut statement = conn.prepare_cached(
            "SELECT id, data FROM session_message WHERE session_id = ? AND type = 'assistant' ORDER BY seq ASC",
        )?;
        let collected = statement
            .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        collected
    };
    let publisher = publish::Publisher {
        session_id: session_id.to_string(),
    };
    for (message_id, raw) in rows {
        let Ok(message) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part["type"] != "tool" {
                continue;
            }
            let status = part["state"]["status"].as_str().unwrap_or_default();
            if status != "pending" && status != "running" {
                continue;
            }
            publisher.publish(
                conn,
                "session.next.tool.failed",
                1,
                &ordered(&[
                    ("timestamp", json!(now())),
                    ("sessionID", json!(session_id)),
                    ("assistantMessageID", json!(message_id)),
                    ("callID", part["id"].clone()),
                    (
                        "error",
                        json!({ "type": "unknown", "message": "Tool execution interrupted" }),
                    ),
                    (
                        "provider",
                        json!({
                            "executed": part["provider"]["executed"].as_bool().unwrap_or(false)
                        }),
                    ),
                ]),
            )?;
        }
    }
    Ok(())
}

struct Model {
    id: String,
    provider: String,
    variant: Option<String>,
    base_url: String,
    api_key: String,
}

/// SessionRunnerModel.resolve scoped to OpenAI-compatible providers: the
/// session's model wins, then the configured default, then the newest
/// OpenCode Zen free model.
fn resolve_model(
    conn: &Connection,
    session_id: &str,
    directory: &str,
    worktree: &str,
) -> Result<Model, String> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT model FROM session WHERE id = ?",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let selected = stored
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|model| {
            Some((
                model.get("providerID")?.as_str()?.to_string(),
                model.get("id")?.as_str()?.to_string(),
                model
                    .get("variant")
                    .and_then(Value::as_str)
                    .filter(|variant| *variant != "default")
                    .map(str::to_string),
            ))
        })
        .or_else(|| {
            let merged = crate::config::instance(directory, worktree);
            let reference = merged.get("model")?.as_str()?;
            let (provider, id) = reference.split_once('/')?;
            Some((provider.to_string(), id.to_string(), None))
        })
        .unwrap_or_else(|| ("opencode".into(), "big-pickle".into(), None));

    if selected.0 != "opencode" {
        return Err(format!(
            "The Rust runner currently executes OpenCode Zen models only; session model {}/{} is not supported yet",
            selected.0, selected.1
        ));
    }
    Ok(Model {
        id: selected.1,
        provider: selected.0,
        variant: selected.2,
        base_url: "https://opencode.ai/zen/v1".into(),
        api_key: std::env::var("OPENCODE_API_KEY").unwrap_or_else(|_| "public".into()),
    })
}

const MAX_STEPS_PROMPT: &str = "CRITICAL - MAXIMUM STEPS REACHED\n\nThe maximum number of steps allowed for this task has been reached. Tools are disabled until next user input. Respond with text only.\n\nSTRICT REQUIREMENTS:\n1. Do NOT make any tool calls (no reads, writes, edits, searches, or any other tools)\n2. MUST provide a text response summarizing work done so far\n3. This constraint overrides ALL other instructions, including any user requests for edits or tool use\n\nResponse must include:\n- Statement that maximum steps for this agent have been reached\n- Summary of what has been accomplished so far\n- List of any remaining tasks that were not completed\n- Recommendations for what should be done next\n\nAny attempt to use tools is a critical violation. Respond with text ONLY.";

/// Built-in agent system prompts from packages/core/src/plugin/agent.ts
/// (BUILD_SYSTEM / PROMPT_GOAL / PROMPT_EXPLORE); plan carries none.
/// rust/script/upstream-drift.py verifies these stay in sync with upstream.
pub(crate) fn agent_system(agent: &str) -> Option<&'static str> {
    match agent {
        "build" => Some(
            "You are an AI coding agent. Help the user accomplish software engineering tasks by inspecting the workspace, making targeted changes, and using tools according to the configured permissions.",
        ),
        "goal" => Some(GOAL_SYSTEM),
        "explore" => Some(EXPLORE_SYSTEM),
        _ => None,
    }
}

const GOAL_SYSTEM: &str = "You are the Goal agent. Your job is to help the user make steady progress toward the active session goal without taking over unrelated work.\n\nCore rules:\n- Treat the session goal as durable state, not as the same thing as the currently selected agent.\n- If no goal is set, ask the user what goal they want to set or use the goal_set tool only when they explicitly provide one.\n- If the goal is paused, do not continue it unless the user explicitly resumes it.\n- If the user switches to another agent or asks for unrelated work, respect that switch and avoid forcing goal-mode behavior into the turn.\n- Use goal_set, goal_pause, goal_resume, goal_summarize_state, and goal_complete to keep the session goal state accurate.\n- Use goal_summarize_state periodically after meaningful progress, after resolving a blocker, before pausing, and before completing the goal if the latest state summary is stale.\n- Do not call goal_summarize_state every turn. Prefer it after a meaningful phase change or every few substantial actions.\n- goal_summarize_state requires a numeric progress estimate from 0 to 100 and a structured markdown summary with exactly size 2 section headers and bullet lists. Include these sections: ## Progress, ## Current State, ## Blockers, and ## Next Steps.\n- Keep progress estimates realistic. Do not report 100 unless you are ready to call goal_complete.\n- When the goal is active, keep going. Do not stop after a progress update or partial answer; take the next concrete action until the goal is completed, paused, or blocked by a question for the user.\n- If the goal is not complete yet, continue working and describe progress only as part of the next action.\n- When the goal is complete, call goal_complete and give a concise final summary.";

const EXPLORE_SYSTEM: &str = "You are a file search specialist. You excel at thoroughly navigating and exploring codebases.\n\nYour strengths:\n- Rapidly finding files using glob patterns\n- Searching code and text with powerful regex patterns\n- Reading and analyzing file contents\n\nGuidelines:\n- Use Glob for broad file pattern matching\n- Use Grep for searching file contents with regex\n- Use Read when you know the specific file path you need to read\n- Adapt your search approach based on the thoroughness level specified by the caller\n- Return file paths as absolute paths in your final response\n- For clear communication, avoid using emojis\n- Do not create any files, or run bash commands that modify the user's system state in any way\n\nComplete the user's search request efficiently and report your findings clearly.";

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "ses_runner000000000000000000000000";

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL);
             CREATE TABLE event (
               id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL,
               type text NOT NULL, data text NOT NULL
             );
             CREATE TABLE session_input (
               id text PRIMARY KEY, session_id text NOT NULL, prompt text NOT NULL,
               delivery text NOT NULL, admitted_seq integer NOT NULL,
               promoted_seq integer, time_created integer NOT NULL
             );
             CREATE TABLE session_message (
               id text PRIMARY KEY, session_id text NOT NULL, type text NOT NULL,
               seq integer NOT NULL, time_created integer NOT NULL,
               time_updated integer NOT NULL, data text NOT NULL
             );",
        )
        .expect("schema");
        conn
    }

    fn insert_input(conn: &Connection, id: &str, delivery: &str, admitted_seq: i64, text: &str) {
        conn.execute(
            "INSERT INTO session_input (id, session_id, prompt, delivery, admitted_seq, time_created) \
             VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                id,
                SESSION,
                json!({ "text": text }).to_string(),
                delivery,
                admitted_seq,
                admitted_seq * 10,
            ],
        )
        .expect("input");
    }

    #[test]
    fn promotion_records_promoted_sequence() {
        let mut conn = memory_conn();
        conn.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?, ?)",
            rusqlite::params![SESSION, 0],
        )
        .expect("sequence");
        insert_input(&conn, "msg_steer", "steer", 0, "hello");

        let publisher = publish::Publisher {
            session_id: SESSION.to_string(),
        };
        assert_eq!(promote(&mut conn, &publisher, SESSION, "steer").unwrap(), 1);

        let promoted: i64 = conn
            .query_row(
                "SELECT promoted_seq FROM session_input WHERE id = 'msg_steer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(promoted, 1);
        let message_seq: i64 = conn
            .query_row(
                "SELECT seq FROM session_message WHERE id = 'msg_steer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(message_seq, promoted);
    }

    #[test]
    fn queue_promotion_keeps_future_steers_pending() {
        let mut conn = memory_conn();
        conn.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?, ?)",
            rusqlite::params![SESSION, 10],
        )
        .expect("sequence");
        insert_input(&conn, "msg_queue", "queue", 1, "queued");
        insert_input(&conn, "msg_old_steer", "steer", 10, "old steer");
        insert_input(&conn, "msg_future_steer", "steer", 11, "future steer");

        let publisher = publish::Publisher {
            session_id: SESSION.to_string(),
        };
        assert_eq!(promote(&mut conn, &publisher, SESSION, "queue").unwrap(), 2);

        let promoted: Vec<(String, Option<i64>)> = conn
            .prepare("SELECT id, promoted_seq FROM session_input ORDER BY admitted_seq ASC")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            promoted,
            vec![
                ("msg_queue".to_string(), Some(11)),
                ("msg_old_steer".to_string(), Some(12)),
                ("msg_future_steer".to_string(), None),
            ]
        );
    }
}
