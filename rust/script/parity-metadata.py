#!/usr/bin/env python3
"""Phase 5 differential tests: config/provider/instance metadata and Rust client."""
import json
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"

failures = []

def fetch(base, path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)

def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL") + f" {name}" + (f" {detail}" if detail else ""))
    if not ok:
        failures.append(name)

def exact(path):
    bun = fetch(BUN, path)
    rust = fetch(RUST, path)
    check(f"GET {path}", bun == rust)
    if bun != rust:
        print("  bun: ", json.dumps(bun, sort_keys=True)[:400])
        print("  rust:", json.dumps(rust, sort_keys=True)[:400])

for path in [
    "/global/health",
    "/global/config",
    "/permission",
    "/question",
    "/path",
    "/vcs",
    "/vcs/status",
    "/lsp",
    "/formatter",
]:
    exact(path)

bun_cfg = fetch(BUN, "/config")
rust_cfg = fetch(RUST, "/config")
check("config preserves schema", rust_cfg.get("$schema") == bun_cfg.get("$schema") == "https://opencode.ai/config.json")
check("config has username", isinstance(rust_cfg.get("username"), str) and bool(rust_cfg.get("username")))

bun_config_providers = fetch(BUN, "/config/providers")
rust_config_providers = fetch(RUST, "/config/providers")
bun_opencode = next((item for item in bun_config_providers["providers"] if item["id"] == "opencode"), None)
rust_opencode = next((item for item in rust_config_providers["providers"] if item["id"] == "opencode"), None)
check("config/providers exposes opencode", bool(bun_opencode and rust_opencode))
if bun_opencode and rust_opencode:
    check("config/providers opencode default", bun_config_providers["default"].get("opencode") == rust_config_providers["default"].get("opencode"))
    check(
        "config/providers opencode model set",
        set(bun_opencode["models"]) == set(rust_opencode["models"]),
        f"bun={len(bun_opencode['models'])} rust={len(rust_opencode['models'])}",
    )

bun_provider = fetch(BUN, "/provider")
rust_provider = fetch(RUST, "/provider")
check("provider catalog non-empty", len(rust_provider["all"]) >= 1)
check(
    "provider catalog contains bun providers",
    {item["id"] for item in bun_provider["all"]} <= {item["id"] for item in rust_provider["all"]},
    f"bun={len(bun_provider['all'])} rust={len(rust_provider['all'])}",
)
check("provider connected includes opencode", "opencode" in rust_provider["connected"])

auth = fetch(RUST, "/provider/auth")
check("provider/auth exposes opencode method", "opencode" in auth and len(auth["opencode"]) >= 1)

bun_commands = {item["name"] for item in fetch(BUN, "/command")}
rust_commands = {item["name"] for item in fetch(RUST, "/command")}
check("command metadata includes builtin commands", {"init", "review", "goal"} <= rust_commands)
check("command metadata includes local commands", {"commit", "rmslop", "issues", "learn"} <= rust_commands)
check("command metadata covers bun names", bun_commands <= rust_commands, f"missing={sorted(bun_commands - rust_commands)[:5]}")

bun_agents = {item["name"] for item in fetch(BUN, "/agent")}
rust_agents = {item["name"] for item in fetch(RUST, "/agent")}
check("agent metadata includes builtins", {"build", "plan", "goal", "general", "explore"} <= rust_agents)
check("agent metadata includes local agents", {"duplicate-pr", "triage"} <= rust_agents)
check("agent metadata covers bun names", bun_agents <= rust_agents, f"missing={sorted(bun_agents - rust_agents)[:5]}")

rust_skills = {item["name"] for item in fetch(RUST, "/skill")}
check("skill metadata includes effect skill", "effect" in rust_skills)

print()
print(f"RESULT: {'FAIL' if failures else 'PASS'} ({len(failures)} failures)")
raise SystemExit(1 if failures else 0)
