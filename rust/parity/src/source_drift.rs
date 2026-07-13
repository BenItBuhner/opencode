use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn normalize_path(path: &str) -> String {
    let params = Regex::new(r":\w+")
        .expect("params regex")
        .replace_all(path, "{}");
    let splats = Regex::new(r"\{\*?\w+\}")
        .expect("splats regex")
        .replace_all(&params, "{}");
    if let Some(prefix) = splats.strip_suffix("/*") {
        return format!("{prefix}/{{}}");
    }
    splats.into_owned()
}

fn protocol_endpoints() -> BTreeSet<(String, String)> {
    let endpoint =
        Regex::new(r#"HttpApiEndpoint\.(get|post|patch|put|del|delete)\(\s*"[^"]+",\s*([^,\n]+)"#)
            .expect("endpoint regex");
    let constant = Regex::new(r#"const\s+(\w+)\s*=\s*"([^"]+)""#).expect("constant regex");
    let quoted = Regex::new(r#""([^"]+)""#).expect("quoted regex");
    let template = Regex::new(r#"`([^`]+)`"#).expect("template regex");
    let mut result = BTreeSet::new();
    for entry in fs::read_dir(root().join("packages/protocol/src/groups")).expect("protocol groups")
    {
        let path = entry.expect("group entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("ts") {
            continue;
        }
        let source = fs::read_to_string(path).expect("group source");
        let constants = constant
            .captures_iter(&source)
            .map(|capture| (capture[1].to_string(), capture[2].to_string()))
            .collect::<BTreeMap<_, _>>();
        for capture in endpoint.captures_iter(&source) {
            let expression = capture[2].trim();
            let path = quoted
                .captures(expression)
                .map(|value| value[1].to_string())
                .or_else(|| constants.get(expression).cloned())
                .or_else(|| {
                    template.captures(expression).and_then(|value| {
                        let mut path = value[1].to_string();
                        for (name, replacement) in &constants {
                            path = path.replace(&format!("${{{name}}}"), replacement);
                        }
                        (!path.contains("${")).then_some(path)
                    })
                });
            if let Some(path) = path {
                result.insert((
                    if &capture[1] == "del" {
                        "delete".into()
                    } else {
                        capture[1].to_string()
                    },
                    normalize_path(&path),
                ));
            }
        }
    }
    result
}

fn rust_routes() -> BTreeSet<(String, String)> {
    let source = read("rust/server/src/lib.rs");
    let route =
        Regex::new(r#"(?s)\.route\(\s*"([^"]+)",\s*((?:[^()]|\([^()]*\))*)\)"#).expect("route");
    let method = Regex::new(r"\b(get|post|patch|put|delete)\(").expect("method");
    route
        .captures_iter(&source)
        .flat_map(|capture| {
            let path = normalize_path(&capture[1]);
            method
                .captures_iter(&capture[2])
                .map(move |method| (method[1].to_string(), path.clone()))
        })
        .collect()
}

#[test]
fn protocol_endpoints_are_ported_or_tracked() {
    let backlog = [
        ("post", "/api/session/{}/compact"),
        ("post", "/api/session/{}/revert/stage"),
        ("post", "/api/session/{}/revert/clear"),
        ("post", "/api/session/{}/revert/commit"),
        ("get", "/api/integration"),
        ("get", "/api/integration/{}"),
        ("post", "/api/integration/{}/connect/key"),
        ("post", "/api/integration/{}/connect/oauth"),
        ("get", "/api/integration/attempt/{}"),
        ("post", "/api/integration/attempt/{}/complete"),
        ("delete", "/api/integration/attempt/{}"),
        ("patch", "/api/credential/{}"),
        ("delete", "/api/credential/{}"),
        ("post", "/experimental/project/{}/copy"),
        ("delete", "/experimental/project/{}/copy"),
        ("post", "/experimental/project/{}/copy/refresh"),
    ]
    .into_iter()
    .map(|(method, path)| (method.to_string(), path.to_string()))
    .collect::<BTreeSet<_>>();
    let upstream = protocol_endpoints();
    let rust = rust_routes();
    let unported = upstream.difference(&rust).cloned().collect::<BTreeSet<_>>();
    assert_eq!(unported, backlog, "v2 endpoint backlog drift");
}

#[test]
fn durable_event_manifest_matches() {
    let upstream_source = read("packages/schema/src/session-event.ts");
    let upstream_regex =
        Regex::new(r#"type:\s*"(session\.next\.[^"]+)",\s*\.\.\.(options|stepSettlementOptions)"#)
            .expect("event regex");
    let upstream = upstream_regex
        .captures_iter(&upstream_source)
        .map(|capture| {
            (
                capture[1].to_string(),
                if &capture[2] == "stepSettlementOptions" {
                    2
                } else {
                    1
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let rust_regex =
        Regex::new(r#"\("(session\.next\.[^"]+)",\s*(\d+)\)"#).expect("rust event regex");
    let rust = rust_regex
        .captures_iter(&read("rust/server/src/v2.rs"))
        .map(|capture| {
            (
                capture[1].to_string(),
                capture[2].parse::<i32>().expect("version"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(rust, upstream);
}

#[test]
fn tools_permissions_theme_and_logo_match() {
    let name = Regex::new(r#"export const name = "([^"]+)""#).expect("tool name");
    let mut upstream_tools = BTreeSet::new();
    for entry in fs::read_dir(root().join("packages/core/src/tool")).expect("tool directory") {
        let path = entry.expect("tool entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("ts") {
            continue;
        }
        if let Some(capture) = name.captures(&fs::read_to_string(path).expect("tool source")) {
            upstream_tools.insert(capture[1].to_string());
        }
    }
    let rust_tool = Regex::new(r#""name":\s*"([^"]+)""#).expect("rust tool");
    let rust_source =
        read("rust/server/src/runner/tools.rs") + &read("rust/server/src/runner/goal.rs");
    let rust_tools = rust_tool
        .captures_iter(&rust_source)
        .map(|capture| capture[1].to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        upstream_tools
            .difference(&rust_tools)
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["websearch".to_string()])
    );

    let action = Regex::new(r#"action:\s*"([^"]+)""#).expect("action");
    let upstream_actions = action
        .captures_iter(&read("packages/core/src/plugin/agent.ts"))
        .map(|capture| capture[1].to_string())
        .collect::<BTreeSet<_>>();
    let rust_permission = read("rust/server/src/runner/permission.rs");
    for action in upstream_actions {
        assert!(
            rust_permission.contains(&format!("\"{action}\"")),
            "missing permission action {action}"
        );
    }

    let theme: Value =
        serde_json::from_str(&read("packages/tui/src/theme/assets/opencode.json")).expect("theme");
    let defs = theme["defs"].as_object().expect("theme defs");
    let rust_theme = read("rust/tui/src/theme.rs");
    let color = Regex::new(r"pub const (\w+): Color = Color::Rgb\(0x(\w\w), 0x(\w\w), 0x(\w\w)\)")
        .expect("color regex");
    let rust_colors = color
        .captures_iter(&rust_theme)
        .map(|capture| {
            (
                capture[1].to_string(),
                format!(
                    "#{}{}{}",
                    &capture[2].to_lowercase(),
                    &capture[3].to_lowercase(),
                    &capture[4].to_lowercase()
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (token, constant) in [
        ("primary", "PRIMARY"),
        ("secondary", "SECONDARY"),
        ("accent", "ACCENT"),
        ("error", "ERROR"),
        ("warning", "WARNING"),
        ("success", "SUCCESS"),
        ("info", "INFO"),
        ("text", "TEXT"),
        ("textMuted", "TEXT_MUTED"),
        ("background", "BACKGROUND"),
        ("backgroundPanel", "BACKGROUND_PANEL"),
        ("backgroundElement", "BACKGROUND_ELEMENT"),
        ("border", "BORDER"),
    ] {
        let mut value = theme["theme"][token]
            .as_str()
            .or_else(|| theme["theme"][token]["dark"].as_str())
            .expect("theme token");
        while let Some(next) = defs.get(value).and_then(Value::as_str) {
            value = next;
        }
        assert_eq!(rust_colors.get(constant).map(String::as_str), Some(value));
    }

    let rows = Regex::new(r#""([^"]*█[^"]*)""#).expect("logo rows");
    let upstream_logo = read("packages/tui/src/logo.ts");
    let rust_logo = read("rust/tui/src/logo.rs");
    for capture in rows.captures_iter(
        upstream_logo
            .split("export const go")
            .next()
            .unwrap_or(&upstream_logo),
    ) {
        assert!(rust_logo.contains(&capture[1]), "missing logo row");
    }
}
