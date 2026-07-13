//! Goal-mode tools, ported from packages/opencode/src/tool/goal.ts and the
//! session goal storage (packages/opencode/src/session/session.ts): goals are
//! durable JSON under `session.metadata.goal` with set/update/summarize/clear
//! semantics, revision counters, and a bounded summary history.

use serde_json::{json, Map, Value};

const GOAL_SUMMARY_LIMIT: usize = 25;
const REQUIRED_SUMMARY_SECTIONS: [&str; 4] =
    ["Progress", "Current State", "Blockers", "Next Steps"];

pub fn definitions() -> Vec<Value> {
    let empty = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "goal_set",
                "description": "Set or replace the durable goal for this session and mark it active.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "The durable session goal to work toward" }
                    },
                    "required": ["text"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "goal_pause",
                "description": "Pause the active session goal. This does not switch agents.",
                "parameters": empty
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "goal_resume",
                "description": "Resume a paused session goal and mark it active. This does not switch agents by itself.",
                "parameters": empty
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "goal_complete",
                "description": "Mark the current session goal as completed and clear it from the session.",
                "parameters": empty
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "goal_status",
                "description": "Read the current durable session goal and status.",
                "parameters": empty
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "goal_summarize_state",
                "description": "Persist a structured progress snapshot for the active session goal. Use this periodically after meaningful progress, before pausing, and before completing the goal.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "progress": { "type": "integer", "minimum": 0, "maximum": 100, "description": "Estimated goal completion percentage as an integer from 0 to 100." },
                        "summary": { "type": "string", "description": "A structured markdown state summary. Must include ## Progress, ## Current State, ## Blockers, and ## Next Steps sections, each with bullet items." },
                        "headline": { "type": "string", "description": "Optional one-line preview of the current state for compact UI displays." }
                    },
                    "required": ["progress", "summary"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

pub struct Outcome {
    pub structured: Value,
    pub text: String,
    pub error: Option<String>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64
}

fn read_metadata(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> rusqlite::Result<Map<String, Value>> {
    let raw: Option<String> = conn.query_row(
        "SELECT metadata FROM session WHERE id = ?",
        [session_id],
        |row| row.get(0),
    )?;
    Ok(raw
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default())
}

fn write_metadata(
    conn: &rusqlite::Connection,
    session_id: &str,
    metadata: &Map<String, Value>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session SET metadata = ?, time_updated = ? WHERE id = ?",
        rusqlite::params![
            Value::Object(metadata.clone()).to_string(),
            now(),
            session_id
        ],
    )?;
    Ok(())
}

fn format_goal(goal: Option<&Value>) -> String {
    let Some(goal) = goal else {
        return "No session goal is currently set.".into();
    };
    let mut lines = vec![
        format!(
            "Goal: {}",
            goal.get("text").and_then(Value::as_str).unwrap_or_default()
        ),
        format!(
            "Status: {}",
            goal.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
    ];
    if let Some(progress) = goal.get("progress").and_then(Value::as_i64) {
        lines.push(format!("Progress: {progress}%"));
    }
    lines.push(format!(
        "Revision: {}",
        goal.get("revision").and_then(Value::as_i64).unwrap_or(0)
    ));
    lines.join("\n")
}

/// validateSummaryFormat: size-2 headers, bullet-only content, all four
/// required sections present with at least one bullet each.
fn validate_summary(summary: &str) -> Result<(), String> {
    let mut sections: Vec<(String, usize)> = vec![];
    for raw in summary.trim().lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let hashes = line.chars().take_while(|ch| *ch == '#').count();
        if hashes > 0 && line.chars().nth(hashes) == Some(' ') {
            if hashes != 2 {
                return Err(format!(
                    "Goal summary headings must use size 2 markdown headers: {line}"
                ));
            }
            let title = line[hashes + 1..].trim().to_string();
            if title.is_empty() {
                return Err("Goal summary headings cannot be empty.".into());
            }
            sections.push((title, 0));
            continue;
        }
        let Some(current) = sections.last_mut() else {
            return Err("Goal summary content must appear under size 2 markdown headers.".into());
        };
        if !line.trim_start().starts_with("- ") {
            return Err(format!(
                "Goal summary section \"{}\" must use bullet list items.",
                current.0
            ));
        }
        current.1 += 1;
    }
    for required in REQUIRED_SUMMARY_SECTIONS {
        let Some(section) = sections.iter().find(|(title, _)| title == required) else {
            return Err(format!(
                "Goal summary is missing the ## {required} section."
            ));
        };
        if section.1 == 0 {
            return Err(format!(
                "Goal summary section ## {required} needs at least one bullet."
            ));
        }
    }
    if let Some((title, _)) = sections.iter().find(|(_, bullets)| *bullets == 0) {
        return Err(format!(
            "Goal summary section ## {title} needs at least one bullet."
        ));
    }
    Ok(())
}

pub fn execute(
    conn: &rusqlite::Connection,
    session_id: &str,
    name: &str,
    input: &Value,
) -> Outcome {
    let run = || -> rusqlite::Result<Outcome> {
        let mut metadata = read_metadata(conn, session_id)?;
        let existing = metadata.get("goal").cloned();
        match name {
            "goal_set" => {
                let text = input
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let timestamp = now();
                // Goal literal order from setGoal: text, status, created,
                // updated, completed?, revision.
                let mut goal = Map::new();
                goal.insert("text".into(), json!(text));
                goal.insert("status".into(), json!("active"));
                goal.insert(
                    "created".into(),
                    existing
                        .as_ref()
                        .and_then(|goal| goal.get("created"))
                        .cloned()
                        .unwrap_or(json!(timestamp)),
                );
                goal.insert("updated".into(), json!(timestamp));
                goal.insert(
                    "revision".into(),
                    json!(
                        existing
                            .as_ref()
                            .and_then(|goal| goal.get("revision"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            + 1
                    ),
                );
                let goal = Value::Object(goal);
                metadata.insert("goal".into(), goal.clone());
                write_metadata(conn, session_id, &metadata)?;
                Ok(Outcome {
                    text: format_goal(Some(&goal)),
                    structured: json!({ "goal": goal }),
                    error: None,
                })
            }
            "goal_pause" | "goal_resume" | "goal_complete" => {
                let Some(mut goal) = existing.and_then(|goal| goal.as_object().cloned()) else {
                    return Ok(Outcome {
                        text: format_goal(None),
                        structured: json!({ "goal": null }),
                        error: None,
                    });
                };
                let timestamp = now();
                let status = match name {
                    "goal_pause" => "paused",
                    "goal_resume" => "active",
                    _ => "completed",
                };
                goal.insert("status".into(), json!(status));
                goal.insert("updated".into(), json!(timestamp));
                if status == "completed" && !goal.contains_key("completed") {
                    goal.insert("completed".into(), json!(timestamp));
                }
                goal.insert(
                    "revision".into(),
                    json!(goal.get("revision").and_then(Value::as_i64).unwrap_or(0) + 1),
                );
                let goal = Value::Object(goal);
                if name == "goal_complete" {
                    // Completion clears the goal from the session (clearGoal).
                    metadata.remove("goal");
                } else {
                    metadata.insert("goal".into(), goal.clone());
                }
                write_metadata(conn, session_id, &metadata)?;
                Ok(Outcome {
                    text: format_goal(Some(&goal)),
                    structured: json!({ "goal": goal }),
                    error: None,
                })
            }
            "goal_status" => Ok(Outcome {
                text: format_goal(existing.as_ref()),
                structured: json!({ "goal": existing.unwrap_or(Value::Null) }),
                error: None,
            }),
            "goal_summarize_state" => {
                let summary_text = input
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Err(message) = validate_summary(summary_text) {
                    return Ok(Outcome {
                        text: String::new(),
                        structured: json!({}),
                        error: Some(message),
                    });
                }
                let Some(mut goal) = existing.and_then(|goal| goal.as_object().cloned()) else {
                    return Ok(Outcome {
                        text: "No session goal is currently set.".into(),
                        structured: json!({ "goal": null }),
                        error: None,
                    });
                };
                let timestamp = now();
                let revision = goal.get("revision").and_then(Value::as_i64).unwrap_or(0) + 1;
                let progress = input.get("progress").and_then(Value::as_i64).unwrap_or(0);
                // GoalSummary order: id, created, progress, summary, headline?, revision.
                let mut summary = Map::new();
                summary.insert("id".into(), json!(format!("{timestamp}-{revision}")));
                summary.insert("created".into(), json!(timestamp));
                summary.insert("progress".into(), json!(progress));
                summary.insert("summary".into(), json!(summary_text.trim()));
                if let Some(headline) = input
                    .get("headline")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|headline| !headline.is_empty())
                {
                    summary.insert("headline".into(), json!(headline));
                }
                summary.insert("revision".into(), json!(revision));
                let mut summaries = goal
                    .get("summaries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                summaries.push(Value::Object(summary));
                let start = summaries.len().saturating_sub(GOAL_SUMMARY_LIMIT);
                let summaries: Vec<Value> = summaries[start..].to_vec();
                goal.insert("progress".into(), json!(progress));
                goal.insert("summaries".into(), Value::Array(summaries));
                goal.insert("updated".into(), json!(timestamp));
                goal.insert("revision".into(), json!(revision));
                let goal = Value::Object(goal);
                metadata.insert("goal".into(), goal.clone());
                write_metadata(conn, session_id, &metadata)?;
                let latest = goal
                    .get("summaries")
                    .and_then(Value::as_array)
                    .and_then(|items| items.last())
                    .cloned()
                    .unwrap_or(Value::Null);
                Ok(Outcome {
                    text: format!(
                        "{}\n\nLatest summary:\n{}",
                        format_goal(Some(&goal)),
                        summary_text
                    ),
                    structured: json!({ "goal": goal, "summary": latest }),
                    error: None,
                })
            }
            other => Ok(Outcome {
                text: String::new(),
                structured: json!({}),
                error: Some(format!("Unknown tool: {other}")),
            }),
        }
    };
    match run() {
        Ok(outcome) => outcome,
        Err(error) => Outcome {
            text: String::new(),
            structured: json!({}),
            error: Some(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE session (id text PRIMARY KEY, metadata text, time_updated integer NOT NULL DEFAULT 0);
             INSERT INTO session (id) VALUES ('ses_goal0000000000000000000000');",
        )
        .expect("schema");
        conn
    }

    const SID: &str = "ses_goal0000000000000000000000";

    #[test]
    fn full_goal_lifecycle() {
        let conn = conn();
        let set = execute(
            &conn,
            SID,
            "goal_set",
            &json!({ "text": "Ship the feature" }),
        );
        assert!(set.error.is_none());
        assert!(set
            .text
            .starts_with("Goal: Ship the feature\nStatus: active"));
        assert_eq!(set.structured["goal"]["revision"], 1);

        let paused = execute(&conn, SID, "goal_pause", &json!({}));
        assert!(paused.text.contains("Status: paused"));
        assert_eq!(paused.structured["goal"]["revision"], 2);

        let status = execute(&conn, SID, "goal_status", &json!({}));
        assert!(status.text.contains("Status: paused"));

        let resumed = execute(&conn, SID, "goal_resume", &json!({}));
        assert!(resumed.text.contains("Status: active"));

        let summary = "## Progress\n- Half done\n## Current State\n- Compiles\n## Blockers\n- None\n## Next Steps\n- Tests";
        let summarized = execute(
            &conn,
            SID,
            "goal_summarize_state",
            &json!({ "progress": 50, "summary": summary }),
        );
        assert!(summarized.error.is_none(), "{:?}", summarized.error);
        assert!(summarized.text.contains("Progress: 50%"));
        assert_eq!(summarized.structured["summary"]["progress"], 50);

        let completed = execute(&conn, SID, "goal_complete", &json!({}));
        assert!(completed.text.contains("Status: completed"));
        // Completion clears the goal from session metadata.
        let metadata = read_metadata(&conn, SID).unwrap();
        assert!(!metadata.contains_key("goal"));
        let after = execute(&conn, SID, "goal_status", &json!({}));
        assert_eq!(after.text, "No session goal is currently set.");
    }

    #[test]
    fn pause_without_goal_reports_none() {
        let conn = conn();
        let paused = execute(&conn, SID, "goal_pause", &json!({}));
        assert!(paused.error.is_none());
        assert_eq!(paused.text, "No session goal is currently set.");
    }

    #[test]
    fn summary_validation_rejects_bad_structure() {
        let conn = conn();
        execute(&conn, SID, "goal_set", &json!({ "text": "x" }));
        let wrong_header = execute(
            &conn,
            SID,
            "goal_summarize_state",
            &json!({ "progress": 10, "summary": "# Progress\n- a" }),
        );
        assert!(wrong_header
            .error
            .as_deref()
            .unwrap()
            .contains("size 2 markdown headers"));
        let missing_section = execute(
            &conn,
            SID,
            "goal_summarize_state",
            &json!({ "progress": 10, "summary": "## Progress\n- a" }),
        );
        assert!(missing_section
            .error
            .as_deref()
            .unwrap()
            .contains("missing the ## Current State"));
        let not_bullets = execute(
            &conn,
            SID,
            "goal_summarize_state",
            &json!({ "progress": 10, "summary": "## Progress\nplain text" }),
        );
        assert!(not_bullets
            .error
            .as_deref()
            .unwrap()
            .contains("must use bullet list items"));
    }

    #[test]
    fn summaries_are_bounded() {
        let conn = conn();
        execute(&conn, SID, "goal_set", &json!({ "text": "x" }));
        let summary =
            "## Progress\n- p\n## Current State\n- c\n## Blockers\n- b\n## Next Steps\n- n";
        for progress in 0..30 {
            execute(
                &conn,
                SID,
                "goal_summarize_state",
                &json!({ "progress": progress.min(100), "summary": summary }),
            );
        }
        let status = read_metadata(&conn, SID).unwrap();
        assert_eq!(
            status["goal"]["summaries"].as_array().unwrap().len(),
            GOAL_SUMMARY_LIMIT
        );
    }
}
