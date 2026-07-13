use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Paths {
    pub home: String,
    pub config: String,
    pub state: String,
    pub cache: String,
}

pub fn paths() -> Paths {
    let home = std::env::var("OPENCODE_TEST_HOME")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    let config = std::env::var("OPENCODE_CONFIG_DIR").unwrap_or_else(|_| {
        std::env::var("XDG_CONFIG_HOME")
            .map(|base| format!("{base}/opencode"))
            .unwrap_or_else(|_| format!("{home}/.config/opencode"))
    });
    let state = std::env::var("XDG_STATE_HOME")
        .map(|base| format!("{base}/opencode"))
        .unwrap_or_else(|_| format!("{home}/.local/state/opencode"));
    let cache = std::env::var("XDG_CACHE_HOME")
        .map(|base| format!("{base}/opencode"))
        .unwrap_or_else(|_| format!("{home}/.cache/opencode"));
    Paths {
        home,
        config,
        state,
        cache,
    }
}

pub fn global() -> Value {
    let paths = paths();
    ["config.json", "opencode.json", "opencode.jsonc"]
        .into_iter()
        .map(|name| read_jsonc(Path::new(&paths.config).join(name)))
        .fold(object(), merge)
}

pub fn instance(directory: &str, worktree: &str) -> Value {
    let mut result = global();
    for file in project_files(directory, worktree) {
        result = merge(result, read_jsonc(file));
    }
    for dir in config_dirs(directory, worktree) {
        for file in ["opencode.json", "opencode.jsonc"] {
            result = merge(result, read_jsonc(Path::new(&dir).join(file)));
        }
    }
    if let Ok(content) = std::env::var("OPENCODE_CONFIG_CONTENT") {
        result = merge(result, parse_jsonc(&content).unwrap_or_else(object));
    }
    if result.get("agent").is_none() {
        result["agent"] = object();
    }
    if result.get("mode").is_none() {
        result["mode"] = object();
    }
    if result.get("plugin").is_none() {
        result["plugin"] = Value::Array(vec![]);
    }
    if result.get("username").is_none() {
        result["username"] = Value::String(
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "user".into()),
        );
    }
    result
}

pub fn update_global(patch: Value) -> std::io::Result<Value> {
    let file = global_config_file();
    let current = read_jsonc(&file);
    let next = merge(current, patch);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, serde_json::to_string_pretty(&next).expect("json"))?;
    Ok(next)
}

pub fn update_instance(directory: &str, patch: Value) -> std::io::Result<Value> {
    let file = Path::new(directory).join("config.json");
    let next = merge(read_jsonc(&file), patch);
    std::fs::write(&file, serde_json::to_string_pretty(&next).expect("json"))?;
    Ok(next)
}

fn global_config_file() -> PathBuf {
    let config = paths().config;
    ["opencode.jsonc", "opencode.json", "config.json"]
        .into_iter()
        .map(|name| Path::new(&config).join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| Path::new(&config).join("opencode.jsonc"))
}

fn project_files(directory: &str, worktree: &str) -> Vec<PathBuf> {
    ancestors(directory, worktree)
        .into_iter()
        .rev()
        .flat_map(|dir| {
            ["opencode.jsonc", "opencode.json"]
                .into_iter()
                .map(move |file| Path::new(&dir).join(file))
        })
        .filter(|path| path.exists())
        .collect()
}

fn config_dirs(directory: &str, worktree: &str) -> Vec<String> {
    let mut dirs = vec![paths().config];
    dirs.extend(
        ancestors(directory, worktree)
            .into_iter()
            .map(|dir| Path::new(&dir).join(".opencode"))
            .filter(|path| path.exists())
            .map(|path| path.to_string_lossy().into_owned()),
    );
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        if !dirs.iter().any(|item| item == &dir) {
            dirs.push(dir);
        }
    }
    dirs
}

fn ancestors(directory: &str, worktree: &str) -> Vec<String> {
    let mut dirs = vec![];
    let mut current = Path::new(directory);
    let stop = Path::new(worktree);
    loop {
        dirs.push(current.to_string_lossy().into_owned());
        if current == stop {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    dirs
}

fn read_jsonc(path: impl AsRef<Path>) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_jsonc(&text))
        .unwrap_or_else(object)
}

fn parse_jsonc(input: &str) -> Option<Value> {
    serde_json::from_str(&strip_trailing_commas(&strip_comments(input))).ok()
}

fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if string {
            escaped = ch == '\\' && !escaped;
            string = ch != '"' || escaped;
            out.push(ch);
            if ch != '\\' {
                escaped = false;
            }
            continue;
        }
        if ch == '"' {
            string = true;
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next;
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next < chars.len() && (chars[next] == '}' || chars[next] == ']') {
                index += 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

pub fn merge(mut target: Value, source: Value) -> Value {
    match (&mut target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                let current = target.remove(&key).unwrap_or(Value::Null);
                target.insert(key, merge(current, value));
            }
            Value::Object(target.clone())
        }
        (_, Value::Null) => target,
        (_, source) => source,
    }
}

fn object() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use super::{merge, strip_comments, strip_trailing_commas};
    use serde_json::json;

    #[test]
    fn jsonc_stripping_keeps_strings() {
        let text = r#"{"url":"https://example.com",/*x*/"items":[1,2,],}"#;
        assert_eq!(
            strip_trailing_commas(&strip_comments(text)),
            r#"{"url":"https://example.com","items":[1,2]}"#
        );
    }

    #[test]
    fn merge_deep_objects_replaces_arrays() {
        assert_eq!(
            merge(json!({"a":{"b":1},"x":[1]}), json!({"a":{"c":2},"x":[2]})),
            json!({"a":{"b":1,"c":2},"x":[2]})
        );
    }
}
