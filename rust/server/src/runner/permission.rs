//! Permission evaluation for the Rust runner.
//!
//! Port of PermissionV2.evaluate (packages/core/src/permission.ts) over the
//! built-in agent rulesets from packages/core/src/plugin/agent.ts plus global
//! `permissions` from the merged config. The Rust runner has no interactive
//! permission-reply flow yet, so `ask` outcomes settle the tool as failed with
//! the same generic message Bun's error mapping produces for unapproved calls.

pub struct Rule {
    pub action: &'static str,
    pub resource: String,
    pub effect: &'static str,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Effect {
    Allow,
    Ask,
    Deny,
}

/// Wildcard.match: `*` spans anything, `?` one char, backslashes normalize to
/// slashes, and a trailing " *" also matches the bare prefix.
pub fn wildcard(input: &str, pattern: &str) -> bool {
    let normalized = input.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    let bytes: Vec<char> = pattern.chars().collect();
    if pattern.ends_with(" *") && matches_glob(&normalized, &bytes[..bytes.len() - 2]) {
        return true;
    }
    matches_glob(&normalized, &bytes)
}

fn matches_glob(input: &str, pattern: &[char]) -> bool {
    let input: Vec<char> = input.chars().collect();
    // Iterative star backtracking.
    let (mut i, mut p) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while i < input.len() {
        // The star branch must win over literal equality: inputs may contain
        // literal `*` characters (e.g. glob patterns used as resources).
        if p < pattern.len() && pattern[p] == '*' {
            star = p;
            mark = i;
            p += 1;
            continue;
        }
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == input[i]) {
            i += 1;
            p += 1;
            continue;
        }
        if star != usize::MAX {
            p = star + 1;
            mark += 1;
            i = mark;
            continue;
        }
        return false;
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn rule(action: &'static str, resource: &str, effect: &'static str) -> Rule {
    Rule {
        action,
        resource: resource.to_string(),
        effect,
    }
}

/// AgentPlugin `defaults` ruleset.
fn defaults() -> Vec<Rule> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        rule("*", "*", "allow"),
        rule("external_directory", "*", "ask"),
        rule(
            "external_directory",
            &format!("{home}/.local/share/opencode/tool-output/*"),
            "allow",
        ),
        rule("external_directory", "/tmp/opencode/*", "allow"),
        rule("question", "*", "deny"),
        rule("goal_set", "*", "deny"),
        rule("goal_pause", "*", "deny"),
        rule("goal_resume", "*", "deny"),
        rule("goal_complete", "*", "deny"),
        rule("goal_status", "*", "deny"),
        rule("goal_summarize_state", "*", "deny"),
        rule("plan_enter", "*", "deny"),
        rule("plan_exit", "*", "deny"),
        rule("read", "*", "allow"),
        rule("read", "*.env", "ask"),
        rule("read", "*.env.*", "ask"),
        rule("read", "*.env.example", "allow"),
    ]
}

/// Built-in agent rulesets (plugin/agent.ts). Unknown agents fall back to the
/// missing-agent deny-all ruleset like PermissionV2.configured.
pub fn agent_rules(agent: &str) -> Vec<Rule> {
    let mut rules = defaults();
    match agent {
        "build" => {
            rules.push(rule("question", "*", "allow"));
            rules.push(rule("plan_enter", "*", "allow"));
        }
        "plan" => {
            rules.push(rule("question", "*", "allow"));
            rules.push(rule("plan_exit", "*", "allow"));
            rules.push(rule("edit", "*", "deny"));
            rules.push(rule("edit", ".opencode/plans/*.md", "allow"));
        }
        "goal" => {
            rules.push(rule("question", "*", "allow"));
            for action in [
                "goal_set",
                "goal_pause",
                "goal_resume",
                "goal_complete",
                "goal_status",
                "goal_summarize_state",
            ] {
                rules.push(rule(action, "*", "allow"));
            }
        }
        "general" => {
            rules.push(rule("todowrite", "*", "deny"));
        }
        "explore" => {
            rules.push(rule("*", "*", "deny"));
            for action in ["grep", "glob", "webfetch", "websearch", "read"] {
                rules.push(rule(action, "*", "allow"));
            }
            rules.push(rule("external_directory", "*", "ask"));
        }
        "compaction" | "title" | "summary" => {
            rules.push(rule("*", "*", "deny"));
        }
        // Markdown/config agents carry their own rules which the Rust runner
        // does not load yet; deny-all matches missingAgentPermissions.
        _ => return vec![rule("*", "*", "deny")],
    }
    rules
}

/// ToolRegistry `whollyDisabled`: the last rule matching the action denies
/// every resource, so the tool is removed from the advertised definitions.
pub fn wholly_denied(agent: &str, action: &str) -> bool {
    agent_rules(agent)
        .iter()
        .rev()
        .find(|rule| wildcard(action, rule.action))
        .is_some_and(|rule| rule.resource == "*" && rule.effect == "deny")
}

/// PermissionV2.evaluate + evaluateInput: agent-rule denials win outright;
/// otherwise the last matching rule decides, defaulting to ask.
#[cfg(test)]
pub fn evaluate(agent: &str, action: &str, resources: &[&str]) -> Effect {
    evaluate_with(agent, &[], action, resources)
}

/// Session-level permission overrides stored on the session row (the fork's
/// out-of-workspace toggle writes `[{permission, pattern, action}]` there).
/// They apply after the agent ruleset so last-match-wins lets them override.
pub fn session_rules(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Vec<(String, String, String)> {
    let raw: Option<Option<String>> = conn
        .query_row(
            "SELECT permission FROM session WHERE id = ?",
            [session_id],
            |row| row.get(0),
        )
        .ok();
    raw.flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|rule| {
            Some((
                rule.get("permission")?.as_str()?.to_string(),
                rule.get("pattern")?.as_str()?.to_string(),
                rule.get("action")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

pub fn evaluate_with(
    agent: &str,
    session: &[(String, String, String)],
    action: &str,
    resources: &[&str],
) -> Effect {
    let rules = agent_rules(agent);
    let effect_for = |resource: &str| {
        session
            .iter()
            .rev()
            .find(|(rule_action, pattern, _)| {
                wildcard(action, rule_action) && wildcard(resource, pattern)
            })
            .map(|(_, _, effect)| effect.as_str())
            .or_else(|| {
                rules
                    .iter()
                    .rev()
                    .find(|rule| {
                        wildcard(action, rule.action) && wildcard(resource, &rule.resource)
                    })
                    .map(|rule| rule.effect)
            })
            .unwrap_or("ask")
    };
    let effects: Vec<&str> = resources
        .iter()
        .map(|resource| effect_for(resource))
        .collect();
    if effects.contains(&"deny") {
        return Effect::Deny;
    }
    if effects.contains(&"ask") {
        return Effect::Ask;
    }
    Effect::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_agent_allows_mutating_tools() {
        assert_eq!(evaluate("build", "bash", &["cargo test"]), Effect::Allow);
        assert_eq!(evaluate("build", "edit", &["src/main.rs"]), Effect::Allow);
        assert_eq!(evaluate("build", "todowrite", &["*"]), Effect::Allow);
    }

    #[test]
    fn env_files_ask_even_for_build() {
        assert_eq!(evaluate("build", "read", &["config/.env"]), Effect::Ask);
        assert_eq!(evaluate("build", "read", &[".env.local"]), Effect::Ask);
        assert_eq!(evaluate("build", "read", &[".env.example"]), Effect::Allow);
    }

    #[test]
    fn plan_agent_denies_edits_except_plans() {
        assert_eq!(evaluate("plan", "edit", &["src/main.rs"]), Effect::Deny);
        assert_eq!(
            evaluate("plan", "edit", &[".opencode/plans/x.md"]),
            Effect::Allow
        );
        assert_eq!(evaluate("plan", "read", &["src/main.rs"]), Effect::Allow);
    }

    #[test]
    fn explore_agent_is_read_only() {
        assert_eq!(evaluate("explore", "bash", &["ls"]), Effect::Deny);
        assert_eq!(evaluate("explore", "grep", &["pattern"]), Effect::Allow);
    }

    #[test]
    fn unknown_agents_deny_everything() {
        assert_eq!(evaluate("custom-md-agent", "read", &["x"]), Effect::Deny);
    }

    #[test]
    fn wildcard_matches_like_bun() {
        assert!(wildcard("anything", "*"));
        assert!(wildcard("a/b/c.md", "a/*/c.md"));
        assert!(wildcard("x.env", "*.env"));
        assert!(!wildcard("x.environment", "*.env"));
        assert!(wildcard("git status", "git *"));
        assert!(wildcard("git", "git *"));
        // Inputs containing literal stars still match universal patterns.
        assert!(wildcard("**/*.rs", "*"));
        assert!(wildcard("echo *", "*"));
    }
}
