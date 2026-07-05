//! The Rust runner's tool registry: read, glob, grep, bash, edit, write,
//! todowrite, skill, and webfetch, ported from packages/core/src/tool/*.
//! Definitions mirror the Bun registry's names, descriptions, and output
//! shapes so durable tool events look the same regardless of which server
//! executed the turn.
//!
//! Permission-gated: every call is evaluated against the built-in agent
//! rulesets (runner/permission.rs). `ask` outcomes fail with the same generic
//! messages Bun's error mapping produces because the Rust server has no
//! interactive permission-reply flow yet. Interactive tools (question) and
//! provider-backed tools (websearch) remain with the Bun runner.

use crate::runner::permission;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub struct Settlement {
    pub structured: Value,
    pub content: Vec<Value>,
    pub error: Option<String>,
}

pub struct ToolEnv<'a> {
    pub directory: &'a str,
    pub worktree: &'a str,
    pub agent: &'a str,
    pub session_id: &'a str,
    pub conn: &'a rusqlite::Connection,
}

const MAX_READ_LINES: usize = 2_000;
const MAX_READ_BYTES: usize = 50 * 1024;
const MAX_LINE_LENGTH: usize = 2_000;

const BASH_DEFAULT_TIMEOUT_MS: u64 = 2 * 60 * 1_000;
const BASH_MAX_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const BASH_MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const WEBFETCH_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Tool definitions the agent can actually use: the registry filtered by
/// wholly-denied permission rules, like ToolRegistry.materialize.
pub fn definitions_for(agent: &str) -> Vec<Value> {
    definitions()
        .into_iter()
        .chain(crate::runner::goal::definitions())
        .filter(|definition| {
            let name = definition["function"]["name"].as_str().unwrap_or_default();
            !permission::wholly_denied(agent, permission_action(name))
        })
        .collect()
}

/// OpenAI-facing tool definitions mirroring the Bun registry.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a text file or supported image, page through a large UTF-8 text file by line offset, or list a directory page. Relative paths resolve from the current location; absolute paths inside it are accepted, while external absolute paths require external_directory approval.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "offset": { "type": "integer", "description": "The 1-based directory entry or text line offset to start reading from" },
                        "limit": { "type": "integer", "description": "The maximum number of directory entries or text lines to read" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files by glob pattern within the active Location. Returns concise relative file resources. Use a relative path to narrow the search and limit to bound the result count.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern to match files against" },
                        "path": { "type": "string", "description": "Relative directory to search. Defaults to the active Location." },
                        "limit": { "type": "integer", "description": "Maximum results to return" }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search file contents by regular expression within the active Location or an absolute managed tool-output file. Use a path to narrow the search, include to filter files by glob, and limit to bound the match count. Returns concise file resources, line numbers, and bounded line previews.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern to search for in file contents" },
                        "path": { "type": "string", "description": "Relative directory to search. Defaults to the active Location." },
                        "include": { "type": "string", "description": "File glob to include in the search (for example, \"*.js\" or \"*.{ts,tsx}\")" },
                        "limit": { "type": "integer", "description": "Maximum matches to return" }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": format!("Execute one shell command string with the host user's filesystem, process, and network authority. The active Location is the default working directory. Relative workdir values resolve from that Location. External workdir values require external_directory approval; best-effort command-argument path warnings are advisory only. Timeout values are milliseconds (default: {BASH_DEFAULT_TIMEOUT_MS}; maximum: {BASH_MAX_TIMEOUT_MS}). Uses the configured shell when set; otherwise uses /bin/sh on POSIX and COMSPEC or cmd.exe on Windows."),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command string to execute" },
                        "workdir": { "type": "string", "description": "Working directory. Defaults to the active Location; relative paths resolve from that Location." },
                        "timeout": { "type": "integer", "description": format!("Timeout in milliseconds. Defaults to {BASH_DEFAULT_TIMEOUT_MS} and may not exceed {BASH_MAX_TIMEOUT_MS}.") }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace exact text in one file. Relative paths resolve within the active Location. Absolute paths inside the Location are accepted. Explicit external absolute paths require external_directory approval before edit approval.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to edit. Relative paths resolve within the active Location. Absolute paths inside that Location are accepted; external absolute paths require external_directory approval." },
                        "oldString": { "type": "string", "description": "Exact text to replace" },
                        "newString": { "type": "string", "description": "Replacement text, which must differ from oldString" },
                        "replaceAll": { "type": "boolean", "description": "Replace all exact occurrences of oldString (default false)" }
                    },
                    "required": ["path", "oldString", "newString"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write",
                "description": "Write content to one file. Relative paths resolve within the active Location. Absolute paths inside the Location are accepted. Explicit external absolute paths require external_directory approval before edit approval.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to write. Relative paths resolve within the active Location. Absolute paths inside that Location are accepted; external absolute paths require external_directory approval." },
                        "content": { "type": "string", "description": "Content to write to the file" }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "todowrite",
                "description": "Create and maintain a structured task list for the current coding session. Use it to track progress during multi-step work and keep todo statuses current.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The updated todo list",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": { "type": "string" },
                                    "status": { "type": "string", "description": "Current status of the task: pending, in_progress, completed, cancelled" },
                                    "priority": { "type": "string", "description": "Priority of the task: high, medium, low" }
                                },
                                "required": ["content", "status", "priority"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["todos"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Load a specialized skill when the task at hand matches one of the available skills in the system context.\n\nUse this tool to inject the skill's instructions and resources into the current conversation. The output may contain detailed workflow guidance as well as references to scripts, files, etc. in the same directory as the skill.\n\nThe skill name must match one of the available skills in the system context.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "The name of the skill from the available skills list" }
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "webfetch",
                "description": "Fetch content from an HTTP or HTTPS URL and return it as text, markdown, or HTML. Markdown is the default.\n\nUse a more targeted tool when one is available. This tool is read-only. Large text results may be replaced with a preview while the complete output is retained in managed storage.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "The HTTP or HTTPS URL to fetch content from" },
                        "format": { "type": "string", "enum": ["text", "markdown", "html"], "description": "The format to return the content in. Defaults to markdown." },
                        "timeout": { "type": "number", "description": "Optional timeout in seconds (maximum: 120)" }
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

pub fn execute(env: &ToolEnv, name: &str, input: &Value) -> Settlement {
    let resources = permission_resources(env, name, input);
    let resource_refs: Vec<&str> = resources.iter().map(String::as_str).collect();
    match permission::evaluate(env.agent, permission_action(name), &resource_refs) {
        permission::Effect::Allow => {}
        // Denied and unapproved calls surface the same generic tool messages
        // Bun's ToolFailure mapping produces.
        permission::Effect::Deny | permission::Effect::Ask => {
            return failure(denied_message(name, input));
        }
    }
    if name.starts_with("goal_") {
        let outcome = crate::runner::goal::execute(env.conn, env.session_id, name, input);
        if let Some(message) = outcome.error {
            return failure(message);
        }
        return Settlement {
            structured: outcome.structured,
            content: vec![json!({ "type": "text", "text": outcome.text })],
            error: None,
        };
    }
    match name {
        "read" => read(env.directory, input),
        "glob" => glob(env.directory, input),
        "grep" => grep(env.directory, input),
        "bash" => bash(env.directory, input),
        "edit" => edit(env, input),
        "write" => write(env, input),
        "todowrite" => todowrite(env, input),
        "skill" => skill(env.worktree, input),
        "webfetch" => webfetch(input),
        other => failure(format!("Unknown tool: {other}")),
    }
}

/// The write tool asserts the `edit` action (Tool.withPermission).
fn permission_action(name: &str) -> &str {
    if name == "write" {
        return "edit";
    }
    name
}

fn permission_resources(env: &ToolEnv, name: &str, input: &Value) -> Vec<String> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match name {
        "bash" => vec![text("command")],
        "edit" | "write" => {
            // LocationMutation resource: the location-relative path.
            let path = text("path");
            let relative = Path::new(&path)
                .strip_prefix(env.directory)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(path);
            vec![relative]
        }
        "grep" | "glob" => vec![text("pattern")],
        "read" => vec![text("path")],
        "skill" => vec![text("name")],
        "webfetch" => vec![text("url")],
        _ => vec!["*".to_string()],
    }
}

fn denied_message(name: &str, input: &Value) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    if name.starts_with("goal_") {
        return format!("The {name} tool is not available for this agent.");
    }
    match name {
        "bash" => format!("Unable to execute command: {}", text("command")),
        "edit" => format!("Unable to edit {}", text("path")),
        "write" => format!("Unable to write {}", text("path")),
        "read" => format!("Unable to read {}", text("path")),
        "glob" => format!("Unable to find files matching {}", text("pattern")),
        "grep" => format!("Unable to search for {}", text("pattern")),
        "todowrite" => "Unable to update todos".to_string(),
        "skill" => format!("Unable to load skill {}", text("name")),
        "webfetch" => format!("Unable to fetch {}", text("url")),
        other => format!("Unable to run {other}"),
    }
}

fn failure(message: String) -> Settlement {
    Settlement {
        structured: Value::Object(Map::new()),
        content: vec![],
        error: Some(message),
    }
}

fn success_json(structured: Value) -> Settlement {
    Settlement {
        structured,
        content: vec![],
        error: None,
    }
}

fn success_text(structured: Value, text: String) -> Settlement {
    Settlement {
        structured,
        content: vec![json!({ "type": "text", "text": text })],
        error: None,
    }
}

fn resolve(directory: &str, input: &str) -> Option<PathBuf> {
    let joined = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        Path::new(directory).join(input)
    };
    let canonical = joined.canonicalize().ok()?;
    // External absolute paths require the permission runtime; scope the Rust
    // registry to the active worktree like a denied external_directory rule.
    canonical
        .starts_with(worktree_root(directory))
        .then_some(canonical)
}

fn worktree_root(directory: &str) -> PathBuf {
    let mut current = PathBuf::from(directory);
    loop {
        if current.join(".git").exists() {
            return current;
        }
        if !current.pop() {
            return PathBuf::from(directory);
        }
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

fn read(directory: &str, input: &Value) -> Settlement {
    let path = match input.get("path").and_then(Value::as_str) {
        Some(path) => path,
        None => return failure("Invalid tool input: path is required".into()),
    };
    let Some(target) = resolve(directory, path) else {
        return failure(format!("Unable to read {path}"));
    };
    if target.is_dir() {
        return list_directory(&target, input);
    }
    let Ok(bytes) = std::fs::read(&target) else {
        return failure(format!("Unable to read {path}"));
    };
    if is_binary(&target, &bytes) {
        return failure(format!("Cannot read binary file: {path}"));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return failure(format!("File is not valid UTF-8: {path}"));
    };
    let offset = input
        .get("offset")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let paged = text.len() > MAX_READ_BYTES || offset.is_some() || limit.is_some();
    if !paged {
        return success_json(json!({
            "uri": format!("file://{}", target.display()),
            "name": target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "content": text,
            "encoding": "utf8",
            "mime": crate::v2::mime_for_tool(&target.to_string_lossy()),
        }));
    }
    let offset = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(MAX_READ_LINES).min(MAX_READ_LINES);
    let mut lines: Vec<String> = vec![];
    let mut bytes_used = 0usize;
    let mut next: Option<usize> = None;
    for (index, raw) in text.split('\n').enumerate() {
        let line_number = index + 1;
        if line_number < offset {
            continue;
        }
        if lines.len() >= limit || bytes_used >= MAX_READ_BYTES {
            next = Some(line_number);
            break;
        }
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let truncated_line = if raw.chars().count() > MAX_LINE_LENGTH {
            let prefix: String = raw.chars().take(MAX_LINE_LENGTH).collect();
            format!("{prefix}... (line truncated to {MAX_LINE_LENGTH} chars)")
        } else {
            raw.to_string()
        };
        let size = truncated_line.len() + usize::from(!lines.is_empty());
        if bytes_used + size > MAX_READ_BYTES {
            next = Some(line_number);
            break;
        }
        bytes_used += size;
        lines.push(truncated_line);
    }
    if lines.is_empty() && offset != 1 {
        return failure(format!("Offset {offset} is out of range"));
    }
    let mut page = Map::new();
    page.insert("type".into(), json!("text-page"));
    page.insert("content".into(), json!(lines.join("\n")));
    page.insert(
        "mime".into(),
        json!(crate::v2::mime_for_tool(&target.to_string_lossy())),
    );
    page.insert("offset".into(), json!(offset));
    page.insert("truncated".into(), json!(next.is_some()));
    if let Some(next) = next {
        page.insert("next".into(), json!(next));
    }
    success_json(Value::Object(page))
}

fn list_directory(target: &Path, input: &Value) -> Settlement {
    let Ok(entries) = std::fs::read_dir(target) else {
        return failure(format!("Unable to read {}", target.display()));
    };
    let mut visible: Vec<(String, &'static str)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                return Some((format!("{name}/"), "directory"));
            }
            if kind.is_file() {
                return Some((name, "file"));
            }
            None
        })
        .collect();
    visible.sort_by(|a, b| {
        if a.1 == b.1 {
            return a.0.cmp(&b.0);
        }
        if a.1 == "directory" {
            return std::cmp::Ordering::Less;
        }
        std::cmp::Ordering::Greater
    });
    let offset = input
        .get("offset")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(1)
        .max(1);
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(MAX_READ_LINES)
        .min(MAX_READ_LINES);
    let selected: Vec<Value> = visible
        .iter()
        .skip(offset - 1)
        .take(limit)
        .map(|(path, kind)| json!({ "path": path, "type": kind }))
        .collect();
    let truncated = offset - 1 + selected.len() < visible.len();
    let mut page = Map::new();
    page.insert("entries".into(), Value::Array(selected.clone()));
    page.insert("truncated".into(), json!(truncated));
    if truncated {
        page.insert("next".into(), json!(offset + selected.len()));
    }
    success_json(Value::Object(page))
}

fn is_binary(path: &Path, bytes: &[u8]) -> bool {
    const BINARY_EXTENSIONS: &[&str] = &[
        "zip", "tar", "gz", "exe", "dll", "so", "class", "jar", "war", "7z", "doc", "docx", "xls",
        "xlsx", "ppt", "pptx", "odt", "ods", "odp", "bin", "dat", "obj", "o", "a", "lib", "wasm",
        "pyc", "pyo", "pdf",
    ];
    if path.extension().is_some_and(|ext| {
        BINARY_EXTENSIONS.contains(&ext.to_string_lossy().to_lowercase().as_str())
    }) {
        return true;
    }
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    if sample.is_empty() {
        return false;
    }
    let mut non_printable = 0usize;
    for byte in sample {
        if *byte == 0 {
            return true;
        }
        if *byte < 9 || (*byte > 13 && *byte < 32) {
            non_printable += 1;
        }
    }
    non_printable as f64 / sample.len() as f64 > 0.3
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

fn glob(directory: &str, input: &Value) -> Settlement {
    let pattern = match input.get("pattern").and_then(Value::as_str) {
        Some(pattern) => pattern,
        None => return failure("Invalid tool input: pattern is required".into()),
    };
    let root = input
        .get("path")
        .and_then(Value::as_str)
        .map(|p| Path::new(directory).join(p))
        .unwrap_or_else(|| PathBuf::from(directory));
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(usize::MAX);
    let Ok(matcher) = build_glob(pattern) else {
        return failure(format!("Unable to find files matching {pattern}"));
    };
    let mut entries: Vec<Value> = vec![];
    let walker = ignore::WalkBuilder::new(&root).hidden(false).build();
    for item in walker {
        if entries.len() >= limit {
            break;
        }
        let Ok(item) = item else { continue };
        if !item.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative_to_root) = item.path().strip_prefix(&root) else {
            continue;
        };
        if !matcher.matched(relative_to_root, false).is_ignore() {
            continue;
        }
        let relative = item
            .path()
            .strip_prefix(directory)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| item.path().to_string_lossy().into_owned());
        entries.push(json!({ "path": relative, "type": "file" }));
    }
    let lines: Vec<String> = if entries.is_empty() {
        vec!["No files found".into()]
    } else {
        entries
            .iter()
            .filter_map(|entry| entry.get("path").and_then(Value::as_str))
            .map(|path| {
                Path::new(directory)
                    .join(path)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    success_text(Value::Array(entries), lines.join("\n"))
}

fn build_glob(pattern: &str) -> Result<ignore::gitignore::Gitignore, ignore::Error> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new("");
    // Anchor like ripgrep --glob: bare names match anywhere; leading "/" pins
    // to the root; the gitignore matcher gives us brace/star semantics.
    builder.add_line(None, pattern)?;
    builder.build()
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

fn grep(directory: &str, input: &Value) -> Settlement {
    let pattern = match input.get("pattern").and_then(Value::as_str) {
        Some(pattern) => pattern,
        None => return failure("Invalid tool input: pattern is required".into()),
    };
    let root = input
        .get("path")
        .and_then(Value::as_str)
        .map(|p| Path::new(directory).join(p))
        .unwrap_or_else(|| PathBuf::from(directory));
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(usize::MAX);
    let include = input.get("include").and_then(Value::as_str);
    let include_matcher = match include.map(build_glob) {
        None => None,
        Some(Ok(matcher)) => Some(matcher),
        Some(Err(_)) => return failure(format!("Unable to search for {pattern}")),
    };
    let mut command = std::process::Command::new("rg");
    command
        .arg("--line-number")
        .arg("--no-heading")
        .arg("--max-count=100")
        .arg("--regexp")
        .arg(pattern)
        .current_dir(&root);
    if let Some(include) = include {
        command.arg("--glob").arg(include);
    }
    let _ = include_matcher;
    let output = match command.output() {
        Ok(output) => output,
        Err(_) => return failure(format!("Unable to search for {pattern}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matches: Vec<Value> = stdout
        .lines()
        .take(limit)
        .filter_map(|line| {
            let (file, rest) = line.split_once(':')?;
            let (line_number, text) = rest.split_once(':')?;
            let relative = Path::new(&root)
                .join(file)
                .strip_prefix(directory)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| file.to_string());
            Some(json!({
                "entry": { "path": relative, "type": "file" },
                "line": line_number.parse::<i64>().ok()?,
                "text": text,
            }))
        })
        .collect();
    let mut lines: Vec<String> = if matches.is_empty() {
        vec!["No files found".into()]
    } else {
        vec![format!("Found {} matches", matches.len())]
    };
    let mut current = String::new();
    for item in &matches {
        let path = item["entry"]["path"].as_str().unwrap_or_default();
        let absolute = Path::new(directory)
            .join(path)
            .to_string_lossy()
            .into_owned();
        if current != absolute {
            if !current.is_empty() {
                lines.push(String::new());
            }
            current = absolute.clone();
            lines.push(format!("{absolute}:"));
        }
        lines.push(format!(
            "  Line {}: {}",
            item["line"],
            item["text"].as_str().unwrap_or_default()
        ));
    }
    success_text(Value::Array(matches), lines.join("\n"))
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

fn bash(directory: &str, input: &Value) -> Settlement {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return failure("Invalid tool input: command is required".into());
    };
    let workdir = input.get("workdir").and_then(Value::as_str).unwrap_or(".");
    let target = if Path::new(workdir).is_absolute() {
        PathBuf::from(workdir)
    } else {
        Path::new(directory).join(workdir)
    };
    let Ok(canonical) = target.canonicalize() else {
        return failure(format!("Unable to execute command: {command}"));
    };
    if !canonical.starts_with(worktree_root(directory)) {
        // External workdir requires external_directory approval (ask).
        return failure(format!("Unable to execute command: {command}"));
    }
    if !canonical.is_dir() {
        return failure(format!("Unable to execute command: {command}"));
    }
    let timeout_ms = input
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(BASH_DEFAULT_TIMEOUT_MS)
        .min(BASH_MAX_TIMEOUT_MS);

    let mut child = match std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&canonical)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        // combineOutput: interleave stderr into the same capture stream.
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return failure(format!("Unable to execute command: {command}")),
    };

    fn collect<R: std::io::Read + Send + 'static>(
        stream: Option<R>,
    ) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(mut stream) = stream {
                let mut chunk = [0u8; 8192];
                while let Ok(read) = std::io::Read::read(&mut stream, &mut chunk) {
                    if read == 0 {
                        break;
                    }
                    if buffer.len() < BASH_MAX_CAPTURE_BYTES + 1 {
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                }
            }
            buffer
        })
    }
    let stdout_thread = collect(child.stdout.take());
    let stderr_thread = collect(child.stderr.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };
    let mut output_bytes = stdout_thread.join().unwrap_or_default();
    output_bytes.extend(stderr_thread.join().unwrap_or_default());

    if exit.is_none() {
        let output = format!(
            "Command exceeded timeout of {timeout_ms} ms. Retry with a larger timeout if the command is expected to take longer."
        );
        // StructuredOutput schema order: exit?, truncated, timeout?.
        return Settlement {
            structured: json!({ "truncated": false, "timeout": true }),
            content: vec![
                json!({ "type": "text", "text": output }),
                json!({ "type": "text", "text": "Command timed out before completion." }),
            ],
            error: None,
        };
    }

    let truncated = output_bytes.len() > BASH_MAX_CAPTURE_BYTES;
    if truncated {
        output_bytes.truncate(BASH_MAX_CAPTURE_BYTES);
    }
    let raw = String::from_utf8_lossy(&output_bytes).into_owned();
    let output = if raw.is_empty() {
        "(no output)".to_string()
    } else {
        raw
    };
    let output = if truncated {
        format!("{output}\n\n[output capture truncated at the in-memory safety limit]")
    } else {
        output
    };
    let code = exit.and_then(|status| status.code()).unwrap_or(-1);
    Settlement {
        structured: json!({ "exit": code, "truncated": truncated }),
        content: vec![
            json!({ "type": "text", "text": output }),
            json!({ "type": "text", "text": format!("Command exited with code {code}.") }),
        ],
        error: None,
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

fn mutation_target(env: &ToolEnv, path: &str) -> Option<(PathBuf, String)> {
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        Path::new(env.directory).join(path)
    };
    // Canonicalize the parent (the file may not exist yet).
    let parent = joined.parent()?.canonicalize().ok()?;
    let canonical = parent.join(joined.file_name()?);
    if !canonical.starts_with(worktree_root(env.directory)) {
        return None;
    }
    let resource = canonical
        .strip_prefix(env.directory)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| canonical.to_string_lossy().into_owned());
    Some((canonical, resource))
}

fn write(env: &ToolEnv, input: &Value) -> Settlement {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return failure("Invalid tool input: path is required".into());
    };
    let Some(content) = input.get("content").and_then(Value::as_str) else {
        return failure(format!("Unable to write {path}"));
    };
    // Create parent directories first so canonicalization succeeds for new
    // nested targets (FSUtil.writeWithDirs).
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        Path::new(env.directory).join(path)
    };
    if let Some(parent) = joined.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Some((canonical, resource)) = mutation_target(env, path) else {
        return failure(format!("Unable to write {path}"));
    };
    let existed = canonical.exists();
    // Preserve an existing UTF-8 BOM (FileMutation.writeTextPreservingBom).
    let had_bom = existed
        && std::fs::read(&canonical)
            .map(|bytes| bytes.starts_with(&[0xEF, 0xBB, 0xBF]))
            .unwrap_or(false);
    let body = if had_bom && !content.starts_with('\u{FEFF}') {
        format!("\u{FEFF}{content}")
    } else {
        content.to_string()
    };
    if std::fs::write(&canonical, body).is_err() {
        return failure(format!("Unable to write {path}"));
    }
    // WriteResult schema order: operation, target, resource, existed.
    Settlement {
        structured: json!({
            "operation": "write",
            "target": canonical.to_string_lossy(),
            "resource": resource,
            "existed": existed,
        }),
        content: vec![json!({
            "type": "text",
            "text": format!("{} file successfully: {resource}", if existed { "Wrote" } else { "Created" }),
        })],
        error: None,
    }
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

fn edit(env: &ToolEnv, input: &Value) -> Settlement {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return failure("Invalid tool input: path is required".into());
    };
    let old_string = input
        .get("oldString")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let new_string = input
        .get("newString")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let replace_all = input
        .get("replaceAll")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if old_string == new_string {
        return failure("No changes to apply: oldString and newString are identical.".into());
    }
    if old_string.is_empty() {
        return failure(
            "oldString must not be empty. Use write to create or overwrite a file.".into(),
        );
    }
    let Some((canonical, resource)) = mutation_target(env, path) else {
        return failure(format!("Unable to edit {path}"));
    };
    let Ok(source) = std::fs::read_to_string(&canonical) else {
        return failure(format!("Unable to edit {path}"));
    };
    // Normalize the replacement strings to the file's line ending.
    let ending = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let normalize = |text: &str| text.replace("\r\n", "\n").replace('\n', ending);
    let old_string = normalize(old_string);
    let new_string = normalize(new_string);
    let replacements = source.matches(&old_string).count();
    if replacements == 0 {
        return failure(
            "Could not find oldString in the file. It must match exactly, including whitespace and indentation."
                .into(),
        );
    }
    if replacements > 1 && !replace_all {
        return failure(
            "Found multiple exact matches for oldString. Provide more surrounding context or set replaceAll to true."
                .into(),
        );
    }
    let replaced = if replace_all {
        source.replace(&old_string, &new_string)
    } else {
        source.replacen(&old_string, &new_string, 1)
    };
    if std::fs::write(&canonical, &replaced).is_err() {
        return failure(format!("Unable to edit {path}"));
    }
    let (additions, deletions, patch) = diff_lines(&resource, &source, &replaced);
    let replacements_applied = if replace_all { replacements } else { 1 };
    let preview = |value: &str, prefix: char| -> Vec<String> {
        let normalized = value.replace("\r\n", "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut shown: Vec<String> = lines
            .iter()
            .take(6)
            .map(|line| {
                let bounded: String = if line.chars().count() > 240 {
                    format!("{}...", line.chars().take(240).collect::<String>())
                } else {
                    (*line).to_string()
                };
                format!("{prefix}{bounded}")
            })
            .collect();
        if lines.len() > shown.len() {
            shown.push(format!("{prefix}..."));
        }
        shown
    };
    let mut model_output = vec![
        format!("Edited file successfully: {resource}"),
        format!("Replacements: {replacements_applied}"),
        "```diff".to_string(),
    ];
    model_output.extend(preview(&old_string, '-'));
    model_output.extend(preview(&new_string, '+'));
    model_output.push("```".to_string());
    Settlement {
        structured: json!({
            "files": [{
                "file": resource,
                "patch": patch,
                "status": "modified",
                "additions": additions,
                "deletions": deletions,
            }],
            "replacements": replacements_applied,
        }),
        content: vec![json!({ "type": "text", "text": model_output.join("\n") })],
        error: None,
    }
}

/// Line diff via LCS: returns (additions, deletions, unified patch in
/// jsdiff createTwoFilesPatch format).
fn diff_lines(file: &str, old: &str, new: &str) -> (usize, usize, String) {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    // Cap the DP table to keep memory bounded for very large files.
    if old_lines.len() * new_lines.len() > 25_000_000 {
        let patch = format!("Index: {file}\n===================================================================\n--- {file}\n+++ {file}\n");
        return (new_lines.len(), old_lines.len(), patch);
    }
    let mut table = vec![0u32; (old_lines.len() + 1) * (new_lines.len() + 1)];
    let width = new_lines.len() + 1;
    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            table[i * width + j] = if old_lines[i] == new_lines[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    #[derive(PartialEq, Clone, Copy)]
    enum Op {
        Keep,
        Delete,
        Add,
    }
    let mut ops: Vec<(Op, usize)> = vec![];
    let (mut i, mut j) = (0usize, 0usize);
    while i < old_lines.len() && j < new_lines.len() {
        if old_lines[i] == new_lines[j] {
            ops.push((Op::Keep, i));
            i += 1;
            j += 1;
            continue;
        }
        if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push((Op::Delete, i));
            i += 1;
            continue;
        }
        ops.push((Op::Add, j));
        j += 1;
    }
    while i < old_lines.len() {
        ops.push((Op::Delete, i));
        i += 1;
    }
    while j < new_lines.len() {
        ops.push((Op::Add, j));
        j += 1;
    }
    let additions = ops.iter().filter(|(op, _)| *op == Op::Add).count();
    let deletions = ops.iter().filter(|(op, _)| *op == Op::Delete).count();

    // Build hunks with 4 context lines (jsdiff default).
    const CONTEXT: usize = 4;
    let mut patch = format!(
        "Index: {file}\n===================================================================\n--- {file}\n+++ {file}\n"
    );
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (op, _))| *op != Op::Keep)
        .map(|(index, _)| index)
        .collect();
    let mut cursor = 0usize;
    while cursor < changed.len() {
        let start = changed[cursor].saturating_sub(CONTEXT);
        let mut end_index = cursor;
        while end_index + 1 < changed.len()
            && changed[end_index + 1] <= changed[end_index] + CONTEXT * 2
        {
            end_index += 1;
        }
        let end = (changed[end_index] + CONTEXT + 1).min(ops.len());
        // Hunk line numbers (1-based).
        let old_start = ops[start..end]
            .iter()
            .find_map(|(op, index)| (*op != Op::Add).then_some(*index))
            .map(|index| index + 1)
            .unwrap_or(1);
        let new_start = ops[start..end]
            .iter()
            .find_map(|(op, index)| (*op != Op::Delete).then_some(*index))
            .map(|index| index + 1)
            .unwrap_or(1);
        let old_count = ops[start..end]
            .iter()
            .filter(|(op, _)| *op != Op::Add)
            .count();
        let new_count = ops[start..end]
            .iter()
            .filter(|(op, _)| *op != Op::Delete)
            .count();
        patch.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for (op, index) in &ops[start..end] {
            let (prefix, line) = match op {
                Op::Keep | Op::Delete => {
                    (if *op == Op::Keep { ' ' } else { '-' }, old_lines[*index])
                }
                Op::Add => ('+', new_lines[*index]),
            };
            patch.push(prefix);
            patch.push_str(line.strip_suffix('\n').unwrap_or(line));
            patch.push('\n');
        }
        cursor = end_index + 1;
    }
    (additions, deletions, patch)
}

// ---------------------------------------------------------------------------
// todowrite
// ---------------------------------------------------------------------------

fn todowrite(env: &ToolEnv, input: &Value) -> Settlement {
    let Some(todos) = input.get("todos").and_then(Value::as_array) else {
        return failure("Unable to update todos".into());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64;
    let update = || -> rusqlite::Result<()> {
        env.conn
            .execute("DELETE FROM todo WHERE session_id = ?", [env.session_id])?;
        for (position, todo) in todos.iter().enumerate() {
            env.conn.execute(
                "INSERT INTO todo (session_id, content, status, priority, position, time_created, time_updated) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    env.session_id,
                    todo.get("content").and_then(Value::as_str).unwrap_or_default(),
                    todo.get("status").and_then(Value::as_str).unwrap_or("pending"),
                    todo.get("priority").and_then(Value::as_str).unwrap_or("medium"),
                    position as i64,
                    now,
                    now,
                ],
            )?;
        }
        Ok(())
    };
    if update().is_err() {
        return failure("Unable to update todos".into());
    }
    Settlement {
        structured: json!({ "todos": todos }),
        content: vec![json!({
            "type": "text",
            "text": serde_json::to_string_pretty(&Value::Array(todos.clone())).expect("serializable"),
        })],
        error: None,
    }
}

// ---------------------------------------------------------------------------
// skill
// ---------------------------------------------------------------------------

fn skill(worktree: &str, input: &Value) -> Settlement {
    let Some(name) = input.get("name").and_then(Value::as_str) else {
        return failure("Invalid tool input: name is required".into());
    };
    let skills = crate::markdown_skills(worktree);
    let Some(found) = skills.iter().find(|skill| skill["name"] == name) else {
        return failure(format!("Unable to load skill {name}"));
    };
    let location = found["location"].as_str().unwrap_or_default();
    let directory = Path::new(location)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut files: Vec<String> = std::fs::read_dir(&directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path().to_string_lossy().into_owned())
                .filter(|path| !path.ends_with("SKILL.md"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.truncate(10);
    let content = found["content"].as_str().unwrap_or_default().trim();
    let mut output = vec![
        format!("<skill_content name=\"{name}\">"),
        format!("# Skill: {name}"),
        String::new(),
        content.to_string(),
        String::new(),
        format!("Base directory for this skill: {directory}"),
        "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.".to_string(),
        "Note: file list is sampled.".to_string(),
        String::new(),
        "<skill_files>".to_string(),
    ];
    output.extend(files.iter().map(|file| format!("<file>{file}</file>")));
    output.push("</skill_files>".to_string());
    output.push("</skill_content>".to_string());
    let rendered = output.join("\n");
    Settlement {
        structured: json!({ "name": name, "directory": directory, "output": rendered }),
        content: vec![json!({ "type": "text", "text": rendered })],
        error: None,
    }
}

// ---------------------------------------------------------------------------
// webfetch
// ---------------------------------------------------------------------------

fn webfetch(input: &Value) -> Settlement {
    let Some(url) = input.get("url").and_then(Value::as_str) else {
        return failure("Invalid tool input: url is required".into());
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return failure(format!("Unable to fetch {url}"));
    }
    let format = input
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    let timeout = input
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(30.0)
        .min(120.0);
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs_f64(timeout))
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        )
        .set("Accept-Language", "en-US,en;q=0.9")
        .call();
    let response = match response {
        Ok(response) => response,
        Err(_) => return failure(format!("Unable to fetch {url}")),
    };
    let content_type = response
        .header("Content-Type")
        .unwrap_or("text/plain")
        .to_string();
    let mut body = String::new();
    let mut reader = std::io::Read::take(response.into_reader(), WEBFETCH_MAX_BYTES as u64 + 1);
    if std::io::Read::read_to_string(&mut reader, &mut body).is_err() {
        return failure(format!("Unable to fetch {url}"));
    }
    if body.len() > WEBFETCH_MAX_BYTES {
        return failure(format!("Unable to fetch {url}"));
    }
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let output = if mime == "text/html" && format != "html" {
        html_to_text(&body)
    } else {
        body
    };
    Settlement {
        structured: json!({
            "url": url,
            "contentType": content_type,
            "format": format,
            "output": output,
        }),
        content: vec![json!({ "type": "text", "text": output })],
        error: None,
    }
}

/// Minimal HTML -> readable text: drops script/style, converts common block
/// elements to line breaks, and decodes basic entities.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut chars = html.char_indices().peekable();
    let lower = html.to_ascii_lowercase();
    let mut skip_until: Option<usize> = None;
    while let Some((index, ch)) = chars.next() {
        if let Some(end) = skip_until {
            if index < end {
                continue;
            }
            skip_until = None;
        }
        if ch != '<' {
            out.push(ch);
            continue;
        }
        let rest = &lower[index..];
        for (open, close) in [("<script", "</script>"), ("<style", "</style>")] {
            if rest.starts_with(open) {
                if let Some(offset) = rest.find(close) {
                    skip_until = Some(index + offset + close.len());
                }
            }
        }
        if skip_until.is_some() {
            continue;
        }
        // Consume the tag; block-level tags become newlines.
        let mut tag = String::new();
        for (_, tag_char) in chars.by_ref() {
            if tag_char == '>' {
                break;
            }
            tag.push(tag_char);
        }
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "p" | "div"
                | "br"
                | "li"
                | "tr"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "section"
                | "article"
        ) {
            out.push('\n');
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .split("\n\n\n")
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(conn: &'a rusqlite::Connection, directory: &'a str) -> ToolEnv<'a> {
        ToolEnv {
            directory,
            worktree: "/workspace",
            agent: "build",
            session_id: "ses_tooltest000000000000000000",
            conn,
        }
    }

    fn memory_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE session (id text PRIMARY KEY);
             INSERT INTO session VALUES ('ses_tooltest000000000000000000');
             CREATE TABLE todo (
               session_id text NOT NULL, content text NOT NULL, status text NOT NULL,
               priority text NOT NULL, position integer NOT NULL,
               time_created integer NOT NULL, time_updated integer NOT NULL,
               CONSTRAINT todo_pk PRIMARY KEY(session_id, position));",
        )
        .expect("schema");
        conn
    }

    #[test]
    fn read_returns_full_small_files() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "read",
            &json!({ "path": "Cargo.toml" }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["encoding"], "utf8");
        assert!(settlement.structured["content"]
            .as_str()
            .unwrap()
            .contains("[workspace]"));
    }

    #[test]
    fn read_rejects_external_paths() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "read",
            &json!({ "path": "/etc/passwd" }),
        );
        assert!(settlement.error.is_some());
    }

    #[test]
    fn read_pages_by_offset() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "read",
            &json!({ "path": "Cargo.toml", "offset": 2, "limit": 1 }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["type"], "text-page");
        assert_eq!(settlement.structured["offset"], 2);
    }

    #[test]
    fn read_lists_directories() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "read",
            &json!({ "path": "." }),
        );
        assert!(settlement.error.is_none());
        assert!(settlement.structured["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "server/" && entry["type"] == "directory"));
    }

    #[test]
    fn glob_finds_rust_sources() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "glob",
            &json!({ "pattern": "**/*.rs", "limit": 5 }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured.as_array().unwrap().len(), 5);
        assert!(settlement.content[0]["text"]
            .as_str()
            .unwrap()
            .contains(".rs"));
    }

    #[test]
    fn grep_finds_matches() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "grep",
            &json!({ "pattern": "opencode-server", "include": "*.toml", "limit": 3 }),
        );
        assert!(settlement.error.is_none());
        assert!(settlement.content[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Found"));
    }

    #[test]
    fn bash_runs_and_reports_exit_code() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "bash",
            &json!({ "command": "echo hello && pwd" }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["exit"], 0);
        assert_eq!(settlement.structured["truncated"], false);
        let output = settlement.content[0]["text"].as_str().unwrap();
        assert!(output.contains("hello"));
        assert!(output.contains("/workspace/rust"));
        assert_eq!(
            settlement.content[1]["text"].as_str().unwrap(),
            "Command exited with code 0."
        );
    }

    #[test]
    fn bash_reports_timeouts() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "bash",
            &json!({ "command": "sleep 5", "timeout": 200 }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["timeout"], true);
        assert_eq!(
            settlement.content[1]["text"].as_str().unwrap(),
            "Command timed out before completion."
        );
    }

    #[test]
    fn bash_denied_for_plan_agent_via_wildcard_still_allows() {
        // plan denies edit but not bash (defaults allow bash).
        let conn = memory_conn();
        let mut plan_env = env(&conn, "/workspace/rust");
        plan_env.agent = "plan";
        let settlement = execute(&plan_env, "bash", &json!({ "command": "true" }));
        assert!(settlement.error.is_none());
        let settlement = execute(
            &plan_env,
            "edit",
            &json!({ "path": "x.rs", "oldString": "a", "newString": "b" }),
        );
        assert_eq!(settlement.error.as_deref(), Some("Unable to edit x.rs"));
    }

    #[test]
    fn write_and_edit_round_trip() {
        let conn = memory_conn();
        let temp = std::env::temp_dir().join(format!("rust-tool-test-{}", std::process::id()));
        std::fs::create_dir_all(temp.join(".git")).expect("temp worktree");
        let directory = temp.to_string_lossy().into_owned();
        let mut tool_env = env(&conn, &directory);
        tool_env.worktree = &directory;

        let settlement = execute(
            &tool_env,
            "write",
            &json!({ "path": "notes.txt", "content": "alpha\nbeta\n" }),
        );
        assert!(settlement.error.is_none(), "{:?}", settlement.error);
        assert_eq!(settlement.structured["operation"], "write");
        assert_eq!(settlement.structured["existed"], false);
        assert!(settlement.content[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Created file successfully"));

        let settlement = execute(
            &tool_env,
            "edit",
            &json!({ "path": "notes.txt", "oldString": "beta", "newString": "gamma" }),
        );
        assert!(settlement.error.is_none(), "{:?}", settlement.error);
        assert_eq!(settlement.structured["replacements"], 1);
        assert_eq!(settlement.structured["files"][0]["additions"], 1);
        assert_eq!(settlement.structured["files"][0]["deletions"], 1);
        let patch = settlement.structured["files"][0]["patch"].as_str().unwrap();
        assert!(patch.contains("-beta"));
        assert!(patch.contains("+gamma"));
        assert_eq!(
            std::fs::read_to_string(temp.join("notes.txt")).unwrap(),
            "alpha\ngamma\n"
        );

        let settlement = execute(
            &tool_env,
            "edit",
            &json!({ "path": "notes.txt", "oldString": "missing", "newString": "x" }),
        );
        assert!(settlement
            .error
            .as_deref()
            .unwrap()
            .starts_with("Could not find oldString"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn todowrite_replaces_rows() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "todowrite",
            &json!({ "todos": [
                { "content": "first", "status": "in_progress", "priority": "high" },
                { "content": "second", "status": "pending", "priority": "medium" }
            ]}),
        );
        assert!(settlement.error.is_none());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM todo", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let status: String = conn
            .query_row("SELECT status FROM todo WHERE position = 0", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn skill_loads_project_skill() {
        let conn = memory_conn();
        let settlement = execute(
            &env(&conn, "/workspace/rust"),
            "skill",
            &json!({ "name": "effect" }),
        );
        assert!(settlement.error.is_none());
        let output = settlement.structured["output"].as_str().unwrap();
        assert!(output.starts_with("<skill_content name=\"effect\">"));
        assert!(output.contains("Base directory for this skill:"));
    }

    #[test]
    fn html_to_text_strips_markup() {
        let text = html_to_text("<html><head><style>x{}</style></head><body><h1>Title</h1><p>Hello &amp; bye</p><script>alert(1)</script></body></html>");
        assert!(text.contains("Title"));
        assert!(text.contains("Hello & bye"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn diff_lines_counts_and_hunks() {
        let (additions, deletions, patch) = diff_lines("f.txt", "a\nb\nc\n", "a\nx\nc\n");
        assert_eq!((additions, deletions), (1, 1));
        assert!(patch.starts_with("Index: f.txt\n==="));
        assert!(patch.contains("@@ -1,3 +1,3 @@"));
        assert!(patch.contains("-b\n+x"));
    }
}
