#!/usr/bin/env python3
"""Differential wire-parity test: Bun server (4096) vs Rust server (4097)."""
import json
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"

def fetch(base, path):
    with urllib.request.urlopen(base + path) as response:
        return json.load(response)

def post(base, path, payload):
    request = urllib.request.Request(
        base + path, data=json.dumps(payload).encode(), headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(request) as response:
        return json.load(response)

def patch(base, path, payload):
    request = urllib.request.Request(
        base + path, data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"}, method="PATCH",
    )
    with urllib.request.urlopen(request) as response:
        return json.load(response)

failures = []

def check(name, expected, actual):
    if expected == actual:
        print(f"PASS {name}")
        return
    failures.append(name)
    print(f"FAIL {name}")
    exp = json.dumps(expected, indent=1, sort_keys=True).splitlines()
    act = json.dumps(actual, indent=1, sort_keys=True).splitlines()
    for left, right in zip(exp, act):
        if left != right:
            print(f"  bun:  {left}\n  rust: {right}")
    if len(exp) != len(act):
        print(f"  line count differs: bun={len(exp)} rust={len(act)}")

# 1. List parity for every list variant the app uses
for path in [
    "/session",
    "/session?directory=/workspace/packages/opencode",
    "/session?directory=/workspace",
    "/session?roots=true",
    "/session?limit=3",
    "/session?search=Goal",
]:
    check(f"GET {path}", fetch(BUN, path), fetch(RUST, path))

# 2. Get parity for every session in the project
sessions = fetch(BUN, "/session")
for item in sessions:
    sid = item["id"]
    check(f"GET /session/{sid}", fetch(BUN, f"/session/{sid}"), fetch(RUST, f"/session/{sid}"))

# 3. Rust-created session must be readable (identically) through the Bun server
created = post(RUST, "/session", {"title": "Created by Rust server"})
sid = created["id"]
bun_view = fetch(BUN, f"/session/{sid}")
check("cross-server create (rust write, bun read)", bun_view, created)

# 4. Rust PATCH metadata (goal!) must round-trip through the Bun server
goal = {
    "goal": {
        "text": "Rust strangler port",
        "status": "active",
        "created": 1783197000000,
        "updated": 1783197000000,
        "progress": 5,
        "revision": 1,
    }
}
updated = patch(RUST, f"/session/{sid}", {"metadata": goal})
bun_view = fetch(BUN, f"/session/{sid}")
check("cross-server patch metadata (rust write, bun read)", bun_view, updated)
assert bun_view["metadata"]["goal"]["text"] == "Rust strangler port"

# 5. Bun PATCH must be readable identically through Rust
patch(BUN, f"/session/{sid}", {"title": "Renamed by Bun server"})
check(
    "cross-server patch title (bun write, rust read)",
    fetch(BUN, f"/session/{sid}"),
    fetch(RUST, f"/session/{sid}"),
)

print()
total = 0
print(f"RESULT: {'FAIL' if failures else 'PASS'} ({len(failures)} failures)")
raise SystemExit(1 if failures else 0)
