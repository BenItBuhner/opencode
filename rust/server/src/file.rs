//! Wire-compatible port of the file/find HTTP surface from
//! packages/opencode/src/server/routes/instance/httpapi/handlers/file.ts.
//!
//! findText shells out to ripgrep with the exact argument set used by
//! packages/core/src/ripgrep.ts and reproduces its match projection,
//! including the 2000-char line clamp and 10-submatch cap.

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const MAX_SUBMATCHES: usize = 10;
const TEXT_CLAMP: usize = 2_000;

pub fn find_text(directory: &str, pattern: &str, limit: usize) -> std::io::Result<Vec<Value>> {
    let output = Command::new("rg")
        .args([
            "--no-config",
            "--json",
            "--hidden",
            "--no-messages",
            "--glob=!**/.git/**",
            "--",
            pattern,
            ".",
        ])
        .current_dir(directory)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut items = vec![];
    for line in stdout.lines() {
        if items.len() >= limit {
            break;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let Some(data) = record.get("data") else {
            continue;
        };
        let path_text = data
            .pointer("/path/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches("./")
            .replace('\\', "/");
        let text = data
            .pointer("/lines/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let clamped = if text.len() > TEXT_CLAMP {
            let mut cut = TEXT_CLAMP;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}...", &text[..cut])
        } else {
            text.to_string()
        };
        let submatches: Vec<Value> = data
            .get("submatches")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .take(MAX_SUBMATCHES)
                    .map(|sub| {
                        json!({
                            "match": { "text": sub.pointer("/match/text").cloned().unwrap_or(Value::Null) },
                            "start": sub.get("start").cloned().unwrap_or(Value::Null),
                            "end": sub.get("end").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        items.push(json!({
            "path": { "text": path_text },
            "lines": { "text": clamped },
            "line_number": data.get("line_number").cloned().unwrap_or(Value::Null),
            "absolute_offset": data.get("absolute_offset").cloned().unwrap_or(Value::Null),
            "submatches": submatches,
        }));
    }
    Ok(items)
}

/// Ripgrep-backed file inventory matching FileSystemSearch state building:
/// every file plus every ancestor directory (with trailing separator).
pub fn inventory(directory: &str) -> std::io::Result<(Vec<String>, Vec<String>)> {
    let output = Command::new("rg")
        .args(["--no-config", "--files", "--glob=!**/.git/**", "."])
        .current_dir(directory)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let relative = line.trim_start_matches("./").replace('\\', "/");
        let parts: Vec<&str> = relative.split('/').collect();
        for index in 0..parts.len().saturating_sub(1) {
            seen.insert(format!("{}/", parts[..=index].join("/")));
        }
        files.push(relative);
    }
    Ok((files, seen.into_iter().collect()))
}

/// Case-insensitive subsequence filter used for find/file. The Bun server
/// ranks with fuzzysort; result sets match for deterministic queries but
/// exact ranking parity is documented as a known divergence.
pub fn find_file(
    directory: &str,
    query: &str,
    kind: Option<&str>,
    limit: usize,
) -> std::io::Result<Vec<String>> {
    let (files, directories) = inventory(directory)?;
    let candidates: Vec<String> = match kind {
        Some("file") => files,
        Some("directory") => directories,
        _ => files.into_iter().chain(directories).collect(),
    };
    let needle = query.to_lowercase();
    let mut scored: Vec<(i64, String)> = candidates
        .into_iter()
        .filter_map(|candidate| {
            let score = subsequence_score(&candidate.to_lowercase(), &needle)?;
            Some((score, candidate))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(_, path)| path)
        .collect())
}

/// Simple fzf-style scorer: consecutive matches and basename hits score
/// higher; returns None when the query is not a subsequence.
fn subsequence_score(haystack: &str, needle: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let haystack_bytes = haystack.as_bytes();
    let mut score = 0i64;
    let mut cursor = 0usize;
    let mut previous: Option<usize> = None;
    let basename_start = haystack.rfind('/').map(|index| index + 1).unwrap_or(0);
    for ch in needle.chars() {
        let found = haystack_bytes[cursor..]
            .iter()
            .position(|byte| (*byte as char).eq_ignore_ascii_case(&ch))?;
        let position = cursor + found;
        score += 1;
        if previous == Some(position.wrapping_sub(1)) {
            score += 5;
        }
        if position >= basename_start {
            score += 3;
        }
        previous = Some(position);
        cursor = position + 1;
    }
    score -= (haystack.len() / 10) as i64;
    Some(score)
}

/// Port of FileHttpApi.list: readdir + gitignore/.ignore flags, directories
/// first, locale ordering, trailing separator on directory paths.
pub fn list(
    directory: &str,
    project_directory: &str,
    relative: &str,
) -> std::io::Result<Vec<Value>> {
    let target = resolve_within(directory, relative)?;
    let ignore_rules = load_ignore(project_directory);
    let mut entries: Vec<(String, bool)> = std::fs::read_dir(&target)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            if !kind.is_file() && !kind.is_dir() {
                return None;
            }
            Some((
                entry.file_name().to_string_lossy().into_owned(),
                kind.is_dir(),
            ))
        })
        .collect();
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => locale_compare(&path_key(relative, &a.0), &path_key(relative, &b.0)),
    });
    Ok(entries
        .into_iter()
        .map(|(name, is_dir)| {
            let rel = path_key(relative, &name);
            let absolute = Path::new(directory).join(&rel);
            let project_rel = pathdiff(&absolute, Path::new(project_directory));
            let ignore_probe = format!("{}{}", project_rel, if is_dir { "/" } else { "" });
            json!({
                "name": name,
                "path": format!("{rel}{}", if is_dir { "/" } else { "" }),
                "absolute": absolute.to_string_lossy(),
                "type": if is_dir { "directory" } else { "file" },
                "ignored": ignore_rules.iter().any(|rule| rule.matched(&ignore_probe, is_dir)),
            })
        })
        .collect())
}

/// Approximation of String#localeCompare for file names: case-insensitive
/// primary weight with a lowercase-first tiebreak, matching ICU root collation
/// for ASCII names.
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let folded = a.to_lowercase().cmp(&b.to_lowercase());
    if folded != std::cmp::Ordering::Equal {
        return folded;
    }
    b.cmp(a)
}

fn path_key(relative: &str, name: &str) -> String {
    if relative.is_empty() || relative == "." {
        return name.to_string();
    }
    format!("{}/{}", relative.trim_end_matches('/'), name)
}

fn pathdiff(target: &Path, base: &Path) -> String {
    match target.strip_prefix(base) {
        Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
        Err(_) => target.to_string_lossy().replace('\\', "/"),
    }
}

struct IgnoreRule {
    matcher: ignore::gitignore::Gitignore,
}

impl IgnoreRule {
    fn matched(&self, path: &str, is_dir: bool) -> bool {
        self.matcher
            .matched(path.trim_end_matches('/'), is_dir)
            .is_ignore()
    }
}

fn load_ignore(project_directory: &str) -> Vec<IgnoreRule> {
    [".gitignore", ".ignore"]
        .iter()
        .filter_map(|name| {
            let file = Path::new(project_directory).join(name);
            if !file.exists() {
                return None;
            }
            let mut builder = ignore::gitignore::GitignoreBuilder::new(project_directory);
            builder.add(&file);
            builder.build().ok().map(|matcher| IgnoreRule { matcher })
        })
        .collect()
}

pub enum Content {
    Text(String),
    Binary { base64: String, mime: String },
    Missing,
}

/// Port of FileHttpApi.content: containment check, empty-text for missing
/// files, UTF-8 text (trimmed) or base64 binary with a mime type.
pub fn content(directory: &str, relative: &str) -> std::io::Result<Content> {
    let file = resolve_within(directory, relative)?;
    if !file.exists() {
        return Ok(Content::Missing);
    }
    let bytes = std::fs::read(&file)?;
    if !bytes.contains(&0) {
        if let Ok(text) = String::from_utf8(bytes.clone()) {
            return Ok(Content::Text(text.trim().to_string()));
        }
    }
    Ok(Content::Binary {
        base64: base64_encode(&bytes),
        mime: mime_type(&file),
    })
}

fn resolve_within(directory: &str, relative: &str) -> std::io::Result<PathBuf> {
    let mut resolved = PathBuf::from(directory);
    for component in Path::new(relative).components() {
        match component {
            Component::ParentDir => {
                if !resolved.pop() || !resolved.starts_with(directory) {
                    return Err(std::io::Error::other("Path escapes the location"));
                }
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(part) => resolved.push(part),
        }
    }
    if !resolved.starts_with(directory) {
        return Err(std::io::Error::other("Path escapes the location"));
    }
    Ok(resolved)
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let buffer = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        let chars = [
            ALPHABET[((value >> 18) & 63) as usize],
            ALPHABET[((value >> 12) & 63) as usize],
            ALPHABET[((value >> 6) & 63) as usize],
            ALPHABET[(value & 63) as usize],
        ];
        match chunk.len() {
            1 => {
                out.push(chars[0] as char);
                out.push(chars[1] as char);
                out.push_str("==");
            }
            2 => {
                out.push(chars[0] as char);
                out.push(chars[1] as char);
                out.push(chars[2] as char);
                out.push('=');
            }
            _ => chars.iter().for_each(|ch| out.push(*ch as char)),
        }
    }
    out
}

/// Mime subset mirroring FSUtil.mimeType for the extensions the app serves.
fn mime_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_rejects_escapes() {
        assert!(resolve_within("/tmp/base", "../etc/passwd").is_err());
        assert!(resolve_within("/tmp/base", "ok/../../etc").is_err());
        assert!(resolve_within("/tmp/base", "src/./main.rs").is_ok());
    }

    #[test]
    fn base64_matches_node_buffer() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"hi"), "aGk=");
        assert_eq!(base64_encode(&[0, 255, 16, 32]), "AP8QIA==");
    }
}
