//! Minimal read-only tool registry for the Rust runner: read, glob, and grep,
//! ported from packages/core/src/tool/{read,glob,grep}.ts and
//! read-filesystem.ts. Definitions mirror the Bun registry's names,
//! descriptions, and output shapes so durable tool events look the same
//! regardless of which server executed the turn.
//!
//! Mutating tools (bash, edit, write, ...) require the permission runtime and
//! remain with the Bun runner for now.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub struct Settlement {
    pub structured: Value,
    pub content: Vec<Value>,
    pub error: Option<String>,
}

const MAX_READ_LINES: usize = 2_000;
const MAX_READ_BYTES: usize = 50 * 1024;
const MAX_LINE_LENGTH: usize = 2_000;

/// OpenAI-facing tool definitions in Bun registry order (read, glob, grep).
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
    ]
}

pub fn execute(directory: &str, name: &str, input: &Value) -> Settlement {
    match name {
        "read" => read(directory, input),
        "glob" => glob(directory, input),
        "grep" => grep(directory, input),
        other => failure(format!("Unknown tool: {other}")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_returns_full_small_files() {
        let settlement = execute("/workspace/rust", "read", &json!({ "path": "Cargo.toml" }));
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["encoding"], "utf8");
        assert!(settlement.structured["content"]
            .as_str()
            .unwrap()
            .contains("[workspace]"));
    }

    #[test]
    fn read_rejects_external_paths() {
        let settlement = execute("/workspace/rust", "read", &json!({ "path": "/etc/passwd" }));
        assert!(settlement.error.is_some());
    }

    #[test]
    fn read_pages_by_offset() {
        let settlement = execute(
            "/workspace/rust",
            "read",
            &json!({ "path": "Cargo.toml", "offset": 2, "limit": 1 }),
        );
        assert!(settlement.error.is_none());
        assert_eq!(settlement.structured["type"], "text-page");
        assert_eq!(settlement.structured["offset"], 2);
    }

    #[test]
    fn read_lists_directories() {
        let settlement = execute("/workspace/rust", "read", &json!({ "path": "." }));
        assert!(settlement.error.is_none());
        assert!(settlement.structured["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "server/" && entry["type"] == "directory"));
    }

    #[test]
    fn glob_finds_rust_sources() {
        let settlement = execute(
            "/workspace/rust",
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
        let settlement = execute(
            "/workspace/rust",
            "grep",
            &json!({ "pattern": "opencode-server", "include": "*.toml", "limit": 3 }),
        );
        assert!(settlement.error.is_none());
        assert!(settlement.content[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Found"));
    }
}
