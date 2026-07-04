#!/usr/bin/env python3
"""Phase 4 differential tests: file list/content and text/file search."""
import json
import urllib.parse
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"

failures = []

def fetch(base, path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)

def check(name, expected, actual):
    if expected == actual:
        print(f"PASS {name}")
        return
    failures.append(name)
    print(f"FAIL {name}")
    exp = json.dumps(expected, indent=1, sort_keys=True).splitlines()
    act = json.dumps(actual, indent=1, sort_keys=True).splitlines()
    shown = 0
    for left, right in zip(exp, act):
        if left != right and shown < 6:
            print(f"  bun:  {left}\n  rust: {right}")
            shown += 1
    if len(exp) != len(act):
        print(f"  line count differs: bun={len(exp)} rust={len(act)}")

# 1. Directory listings (root, nested, mixed content)
for path in [".", "src", "src/tool", "test", "src/session"]:
    encoded = urllib.parse.quote(path)
    check(f"GET /file?path={path}", fetch(BUN, f"/file?path={encoded}"), fetch(RUST, f"/file?path={encoded}"))

# 2. File content: text, trimmed text, binary
for path in ["package.json", "src/tool/goal.ts", "script/schema.ts", "no/such/file.txt"]:
    encoded = urllib.parse.quote(path)
    check(f"GET /file/content?path={path}", fetch(BUN, f"/file/content?path={encoded}"), fetch(RUST, f"/file/content?path={encoded}"))

# 3. Text search. The Bun endpoint is NOT self-consistent when total matches
# exceed the limit of 10 (ripgrep's parallel walk truncates a nondeterministic
# subset), so the oracle is ripgrep itself: every returned match must exist in
# an exhaustive rg run, and the count must be min(10, total).
import subprocess

def rg_oracle(pattern):
    out = subprocess.run(
        ["rg", "--no-config", "--json", "--hidden", "--no-messages", "--glob=!**/.git/**", "--", pattern, "."],
        cwd="/workspace/packages/opencode", capture_output=True, text=True,
    )
    matches = set()
    for line in out.stdout.splitlines():
        record = json.loads(line)
        if record.get("type") != "match":
            continue
        data = record["data"]
        matches.add((data["path"]["text"].removeprefix("./"), data["line_number"], data["lines"]["text"]))
    return matches

for pattern in ["GoalSetTool", "SessionPrompt.Service", "does_not_exist_anywhere_123"]:
    encoded = urllib.parse.quote(pattern)
    oracle = rg_oracle(pattern)
    expected_count = min(10, len(oracle))
    for label, base in (("bun", BUN), ("rust", RUST)):
        items = fetch(base, f"/find?pattern={encoded}")
        keys = {(m["path"]["text"], m["line_number"], m["lines"]["text"]) for m in items}
        valid = keys <= oracle and len(items) == expected_count
        print(("PASS" if valid else "FAIL") + f" GET /find?pattern={pattern} [{label}] {len(items)}/{expected_count} all-in-oracle={keys <= oracle}")
        if not valid:
            failures.append(f"find {pattern} {label}")

# 4. find/file. The Bun server uses the native `fff` fuzzy engine (typo
# tolerance, frecency); the Rust port uses strict subsequence scoring. The
# pinned contract: the top hits for high-signal queries must agree.
for query, expected_top in [("registry", "registry"), ("goal.ts", "goal"), ("prompt.test", "prompt.test")]:
    bun = fetch(BUN, f"/find/file?query={query}&limit=10")
    rust = fetch(RUST, f"/find/file?query={query}&limit=10")
    bun_top = {p for p in bun[:3]}
    rust_top = {p for p in rust[:3]}
    ok = any(expected_top in p for p in rust_top) and len(bun_top & set(rust)) >= 1
    print(("PASS" if ok else "FAIL") + f" GET /find/file?query={query} top-hit agreement (bun_top&rust={len(bun_top & set(rust))})")
    if not ok:
        failures.append(f"find/file {query}")

# 5. Empty surfaces
check("GET /find/symbol", fetch(BUN, "/find/symbol?query=x"), fetch(RUST, "/find/symbol?query=x"))
check("GET /file/status", fetch(BUN, "/file/status"), fetch(RUST, "/file/status"))

print()
print(f"RESULT: {'FAIL' if failures else 'PASS'} ({len(failures)} failures)")
raise SystemExit(1 if failures else 0)
