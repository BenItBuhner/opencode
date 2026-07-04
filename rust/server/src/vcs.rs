use serde_json::{json, Value};
use std::process::Command;

pub fn info(directory: &str) -> Value {
    json!({
        "branch": run(directory, &["branch", "--show-current"]).ok().filter(|item| !item.is_empty()),
        "default_branch": default_branch(directory),
    })
}

pub fn status(directory: &str) -> Vec<Value> {
    let stats = numstat(directory, "HEAD");
    status_items(directory)
        .into_iter()
        .map(|item| {
            let stat = stats
                .iter()
                .find(|stat| stat.file == item.file)
                .cloned()
                .unwrap_or_else(|| {
                    if item.status == "added" {
                        untracked_stat(directory, &item.file)
                    } else {
                        Stat {
                            file: item.file.clone(),
                            additions: 0,
                            deletions: 0,
                        }
                    }
                });
            json!({
                "file": item.file,
                "additions": stat.additions,
                "deletions": stat.deletions,
                "status": item.status,
            })
        })
        .collect()
}

pub fn diff(directory: &str, mode: &str, context: Option<i64>) -> Vec<Value> {
    if mode == "branch" {
        let Some(default) = default_branch_ref(directory) else {
            return vec![];
        };
        if run(directory, &["branch", "--show-current"])
            .ok()
            .as_deref()
            == default_branch(directory).as_deref()
        {
            return vec![];
        }
        let Ok(base) = run(directory, &["merge-base", &default, "HEAD"]) else {
            return vec![];
        };
        return diff_against(directory, &base, context);
    }
    if has_head(directory) {
        return diff_against(directory, "HEAD", context);
    }
    status(directory)
        .into_iter()
        .map(|mut item| {
            item["patch"] = Value::String(empty_patch(item["file"].as_str().unwrap_or_default()));
            item
        })
        .collect()
}

pub fn diff_raw(directory: &str) -> String {
    let tracked = if has_head(directory) {
        run(
            directory,
            &["diff", "--no-ext-diff", "--binary", "HEAD", "--", "."],
        )
        .unwrap_or_default()
    } else {
        String::new()
    };
    let untracked = status_items(directory)
        .into_iter()
        .filter(|item| item.code == "??")
        .filter_map(|item| {
            run(
                directory,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--binary",
                    "--no-index",
                    "/dev/null",
                    &item.file,
                ],
            )
            .ok()
        })
        .collect::<Vec<_>>();
    std::iter::once(tracked)
        .chain(untracked)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn diff_against(directory: &str, ref_name: &str, context: Option<i64>) -> Vec<Value> {
    let stats = numstat(directory, ref_name);
    let patches = patches(directory, ref_name, context);
    let mut items = status_items(directory)
        .into_iter()
        .filter(|item| item.code == "??")
        .collect::<Vec<_>>();
    items.extend(diff_items(directory, ref_name));
    items.sort_by(|a, b| a.file.cmp(&b.file));
    items.dedup_by(|a, b| a.file == b.file);
    items
        .into_iter()
        .map(|item| {
            let stat = stats
                .iter()
                .find(|stat| stat.file == item.file)
                .cloned()
                .unwrap_or_else(|| untracked_stat(directory, &item.file));
            json!({
                "file": item.file,
                "patch": patches.iter().find(|patch| patch.file == item.file).map(|patch| patch.text.clone()).unwrap_or_else(|| empty_patch(&item.file)),
                "additions": stat.additions,
                "deletions": stat.deletions,
                "status": item.status,
            })
        })
        .collect()
}

fn status_items(directory: &str) -> Vec<Item> {
    run(directory, &["status", "--porcelain=v1", "--", "."])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let code = line.get(0..2)?.to_string();
            let file = line.get(3..)?.split(" -> ").last()?.to_string();
            Some(Item {
                status: status_from_code(&code),
                code,
                file,
            })
        })
        .collect()
}

fn diff_items(directory: &str, ref_name: &str) -> Vec<Item> {
    run(directory, &["diff", "--name-status", ref_name, "--", "."])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let code = parts.next()?.to_string();
            let file = parts.last()?.to_string();
            Some(Item {
                status: status_from_code(&code),
                code,
                file,
            })
        })
        .collect()
}

fn numstat(directory: &str, ref_name: &str) -> Vec<Stat> {
    run(directory, &["diff", "--numstat", ref_name, "--", "."])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            Some(Stat {
                additions: parts.next()?.parse().unwrap_or(0),
                deletions: parts.next()?.parse().unwrap_or(0),
                file: parts.last()?.to_string(),
            })
        })
        .collect()
}

fn patches(directory: &str, ref_name: &str, context: Option<i64>) -> Vec<Patch> {
    let unified = format!("--unified={}", context.unwrap_or(2_147_483_647));
    let text = run(
        directory,
        &[
            "diff",
            "--no-ext-diff",
            "--binary",
            &unified,
            ref_name,
            "--",
            ".",
        ],
    )
    .unwrap_or_default();
    text.split("\ndiff --git ")
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| {
            let text = if chunk.starts_with("diff --git ") {
                chunk.to_string()
            } else {
                format!("diff --git {chunk}")
            };
            Patch {
                file: file_from_patch(&text).unwrap_or_default(),
                text,
            }
        })
        .collect()
}

fn file_from_patch(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            line.strip_prefix("+++ b/")
                .or_else(|| line.strip_prefix("--- a/"))
        })
        .map(ToString::to_string)
}

fn untracked_stat(directory: &str, file: &str) -> Stat {
    let additions = std::fs::read_to_string(std::path::Path::new(directory).join(file))
        .map(|text| text.lines().count() as i64)
        .unwrap_or(0);
    Stat {
        file: file.into(),
        additions,
        deletions: 0,
    }
}

fn default_branch(directory: &str) -> Option<String> {
    default_branch_ref(directory).map(|item| item.rsplit('/').next().unwrap_or(&item).to_string())
}

fn default_branch_ref(directory: &str) -> Option<String> {
    run(directory, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .ok()
        .filter(|item| !item.is_empty())
        .or_else(|| {
            Some("origin/dev".to_string())
                .filter(|item| run(directory, &["rev-parse", "--verify", item]).is_ok())
        })
        .or_else(|| {
            Some("origin/main".to_string())
                .filter(|item| run(directory, &["rev-parse", "--verify", item]).is_ok())
        })
}

fn has_head(directory: &str) -> bool {
    run(directory, &["rev-parse", "--verify", "HEAD"]).is_ok()
}

fn status_from_code(code: &str) -> String {
    if code.contains('D') {
        return "deleted".into();
    }
    if code.contains('?') || code.contains('A') {
        return "added".into();
    }
    "modified".into()
}

fn empty_patch(file: &str) -> String {
    format!("Index: {file}\n===================================================================\n--- {file}\n+++ {file}\n")
}

fn run(directory: &str, args: &[&str]) -> std::io::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(String::from_utf8_lossy(
            &output.stderr,
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[derive(Clone)]
struct Item {
    file: String,
    code: String,
    status: String,
}

#[derive(Clone)]
struct Stat {
    file: String,
    additions: i64,
    deletions: i64,
}

struct Patch {
    file: String,
    text: String,
}
