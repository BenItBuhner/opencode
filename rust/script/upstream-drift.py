#!/usr/bin/env python3
"""Upstream drift detector: the upgrade path from upstream TypeScript to the
Rust port.

Workflow after every upstream merge (`git merge upstream/dev`):

    1. python3 rust/script/upstream-drift.py
       -> each FAIL names the upstream surface that changed and the Rust file
          that owns the port.
    2. Port the flagged deltas, then re-run this script plus the parity
       suites (parity*.py) against live Bun + Rust servers.
    3. parity-runner.py exercises live provider turns end-to-end.

The script needs no servers; it compares checked-in sources only, so it can
run in CI as a merge gate.

Coverage:
  A. v2 protocol endpoints (packages/protocol/src/groups) vs the Rust router.
  B. Durable session event types/versions (schema/session-event.ts) vs the
     Rust SessionDurable manifest.
  C. Registered tool names (core/src/tool) vs the Rust tool registry.
  D. Built-in agent permission actions (core/src/plugin/agent.ts) vs the Rust
     permission rulesets.
  E. Agent system prompts (core/src/plugin/agent.ts) vs the Rust constants.
  F. The default theme palette (tui/theme/assets/opencode.json) vs theme.rs.
  G. The TUI logo glyphs (tui/src/logo.ts) vs logo.rs.
"""

import json
import re
from pathlib import Path

ROOT = Path("/workspace")
failures = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL") + f" {name}" + (f"  {detail}" if detail else ""))
    if not ok:
        failures.append(name)


def read(path):
    return (ROOT / path).read_text()


# ---------------------------------------------------------------------------
# A. Protocol endpoints vs Rust router
# ---------------------------------------------------------------------------
endpoint_re = re.compile(r'HttpApiEndpoint\.(get|post|patch|del|delete)\(\s*"[^"]+",\s*"([^"]+)"')
ts_endpoints = set()
for group in (ROOT / "packages/protocol/src/groups").glob("*.ts"):
    for method, path in endpoint_re.findall(group.read_text()):
        # Express-style :params -> axum-style {params}; normalize names since
        # the Rust router picks its own placeholder identifiers.
        normalized = re.sub(r":\w+", "{}", path)
        normalized = re.sub(r"/\*$", "/{}", normalized)
        ts_endpoints.add((("delete" if method == "del" else method), normalized))

rust_main = read("rust/server/src/main.rs")
rust_routes = set()
for path, handlers in re.findall(r'\.route\(\s*"([^"]+)",\s*((?:[^()]|\([^()]*\))*)\)', rust_main):
    normalized = re.sub(r"\{\w+\}", "{}", path)
    for method in re.findall(r"\b(get|post|patch|delete)\(", handlers):
        rust_routes.add((method, normalized))

# The intentionally-unported v2 surface (each entry needs an explicit decision
# when porting; remove entries as they land in the Rust server).
V2_ENDPOINT_BACKLOG = {
    ("get", "/api/event"),
    ("get", "/api/session/{}/event"),
    ("get", "/api/session/{}/context"),
    ("get", "/api/session/{}/history"),  # ported; kept for structure example
    ("post", "/api/session/{}/compact"),
    ("post", "/api/session/{}/revert/stage"),
    ("post", "/api/session/{}/revert/clear"),
    ("post", "/api/session/{}/revert/commit"),
    ("get", "/api/permission/request"),
    ("get", "/api/permission/saved"),
    ("delete", "/api/permission/saved/{}"),
    ("post", "/api/session/{}/permission"),
    ("get", "/api/session/{}/permission"),
    ("get", "/api/session/{}/permission/{}"),
    ("post", "/api/session/{}/permission/{}/reply"),
    ("get", "/api/question/request"),
    ("get", "/api/session/{}/question"),
    ("post", "/api/session/{}/question/{}/reply"),
    ("post", "/api/session/{}/question/{}/reject"),
    ("get", "/api/pty"),
    ("post", "/api/pty"),
    ("get", "/api/pty/{}"),
    ("delete", "/api/pty/{}"),
    ("post", "/api/pty/{}/connect-token"),
    ("get", "/api/pty/{}/connect"),
    ("get", "/api/fs/read/{}"),
    ("get", "/api/fs/list"),
    ("get", "/api/fs/find"),
    ("get", "/api/model"),
    ("get", "/api/provider"),
    ("get", "/api/provider/{}"),
    ("get", "/api/reference"),
    ("get", "/api/location"),
    ("get", "/api/integration"),
    ("get", "/api/integration/{}"),
    ("post", "/api/integration/{}/connect/key"),
    ("post", "/api/integration/{}/connect/oauth"),
    ("get", "/api/integration/attempt/{}"),
    ("post", "/api/integration/attempt/{}/complete"),
    ("delete", "/api/integration/attempt/{}"),
    ("patch", "/api/credential/{}"),
    ("delete", "/api/credential/{}"),
    ("get", "/api/command"),
    ("get", "/api/agent"),
    ("get", "/api/skill"),
    ("get", "/api/health"),  # ported; kept as the worked example in review
    ("get", "/api/session/{}/message"),
    ("get", "/api/session/{}/message/{}"),
}

unported = {
    endpoint
    for endpoint in ts_endpoints
    if endpoint not in rust_routes
}
new_upstream = sorted(unported - V2_ENDPOINT_BACKLOG)
check(
    "A. no NEW unported v2 endpoints (additions since the last port pass)",
    not new_upstream,
    f"new={new_upstream}" if new_upstream else f"tracked backlog={len(unported)}",
)

# ---------------------------------------------------------------------------
# B. Durable event manifest
# ---------------------------------------------------------------------------
events_ts = read("packages/schema/src/session-event.ts")
ts_events = {}
for match in re.finditer(
    r'type:\s*"(session\.next\.[^"]+)",\s*\.\.\.(options|stepSettlementOptions)', events_ts
):
    version = 2 if match.group(2) == "stepSettlementOptions" else 1
    ts_events[match.group(1)] = version
rust_v2 = read("rust/server/src/v2.rs")
rust_events = {
    kind: int(version)
    for kind, version in re.findall(r'\("(session\.next\.[^"]+)",\s*(\d+)\)', rust_v2)
}
check(
    "B. durable event manifest matches schema/session-event.ts",
    ts_events == rust_events,
    f"ts-only={sorted(set(ts_events) - set(rust_events))} rust-only={sorted(set(rust_events) - set(ts_events))} "
    f"version-drift={[k for k in ts_events if k in rust_events and ts_events[k] != rust_events[k]]}",
)

# ---------------------------------------------------------------------------
# C. Tool registry names
# ---------------------------------------------------------------------------
ts_tools = set()
for tool in (ROOT / "packages/core/src/tool").glob("*.ts"):
    found = re.search(r'export const name = "([^"]+)"', tool.read_text())
    if found:
        ts_tools.add(found.group(1))
rust_tools_src = read("rust/server/src/runner/tools.rs") + read("rust/server/src/runner/goal.rs")
rust_tools = set(re.findall(r'"name":\s*"([^"]+)"', rust_tools_src))
TOOL_BACKLOG = {"question", "websearch", "apply_patch", "applypatch"}
missing_tools = ts_tools - rust_tools - TOOL_BACKLOG
check(
    "C. core tool registry ported (minus tracked backlog: question/websearch/apply_patch)",
    not missing_tools,
    f"missing={sorted(missing_tools)}" if missing_tools else f"ported={len(ts_tools & rust_tools)}",
)

# ---------------------------------------------------------------------------
# D. Agent permission actions
# ---------------------------------------------------------------------------
agent_ts = read("packages/core/src/plugin/agent.ts")
ts_actions = set(re.findall(r'action:\s*"([^"]+)"', agent_ts))
rust_permission = read("rust/server/src/runner/permission.rs")
# Actions appear as rule("action", ...) calls and as `for action in [...]`
# array literals feeding rule(action, ...).
rust_actions = set(re.findall(r'rule\("([^"]+)"', rust_permission))
for array in re.findall(r"for action in \[([^\]]+)\]", rust_permission):
    rust_actions.update(re.findall(r'"([^"]+)"', array))
missing_actions = ts_actions - rust_actions
check(
    "D. built-in permission actions covered",
    not missing_actions,
    f"missing={sorted(missing_actions)}" if missing_actions else f"actions={len(ts_actions)}",
)

# ---------------------------------------------------------------------------
# E. Agent system prompts
# ---------------------------------------------------------------------------
rust_runner = read("rust/server/src/runner/mod.rs")


def ts_string_constant(source, name):
    # Matches `const NAME =\n  "..."` and `const NAME = \`...\``.
    match = re.search(rf"const {name} =\s*(\"(?:[^\"\\]|\\.)*\"|`[^`]*`)", source, re.S)
    if not match:
        return None
    raw = match.group(1)
    if raw.startswith('"'):
        return json.loads(raw)
    return raw[1:-1]


for name, rust_needle in [
    ("BUILD_SYSTEM", "You are an AI coding agent."),
    ("PROMPT_GOAL", "You are the Goal agent."),
    ("PROMPT_EXPLORE", "You are a file search specialist."),
]:
    ts_prompt = ts_string_constant(agent_ts, name)
    rust_match = re.search(rf'"({re.escape(rust_needle)}(?:[^"\\]|\\.)*)"', rust_runner)
    rust_prompt = json.loads(f'"{rust_match.group(1)}"') if rust_match else None
    check(
        f"E. agent prompt {name} in sync",
        ts_prompt is not None and rust_prompt is not None and ts_prompt.strip() == rust_prompt.strip(),
        "" if ts_prompt and rust_prompt and ts_prompt.strip() == rust_prompt.strip() else "content drift",
    )

# ---------------------------------------------------------------------------
# F. Theme palette
# ---------------------------------------------------------------------------
theme_json = json.loads(read("packages/tui/src/theme/assets/opencode.json"))
defs = theme_json.get("defs", {})


def resolve(token):
    value = theme_json["theme"].get(token)
    if isinstance(value, dict):
        value = value.get("dark")
    while isinstance(value, str) and value in defs:
        value = defs[value]
    return value.lower() if isinstance(value, str) else value


theme_rs = read("rust/tui/src/theme.rs")


def rust_color(name):
    found = re.search(
        rf"pub const {name}: Color = Color::Rgb\(0x(\w\w), 0x(\w\w), 0x(\w\w)\)", theme_rs
    )
    return f"#{found.group(1)}{found.group(2)}{found.group(3)}".lower() if found else None


for token, const in [
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
]:
    check(
        f"F. theme token {token}",
        resolve(token) == rust_color(const),
        f"ts={resolve(token)} rust={rust_color(const)}",
    )

# ---------------------------------------------------------------------------
# G. Logo glyphs
# ---------------------------------------------------------------------------
logo_ts = read("packages/tui/src/logo.ts")
logo_rs = read("rust/tui/src/logo.rs")
ts_rows = re.findall(r'"([^"]*█[^"]*)"', logo_ts.split("export const go")[0])
rust_rows = re.findall(r'"([^"]*█[^"]*)"', logo_rs)
check(
    "G. logo glyph rows identical",
    ts_rows and all(row in rust_rows for row in ts_rows),
    f"ts_rows={len(ts_rows)} matched={sum(1 for row in ts_rows if row in rust_rows)}",
)

print()
if failures:
    print(f"{len(failures)} DRIFT FAILURES: {failures}")
    raise SystemExit(1)
print("NO UPSTREAM DRIFT DETECTED")
