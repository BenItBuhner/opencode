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

# 6. Message read path: full list for every session
sessions = fetch(BUN, "/session")
for item in sessions:
    ms = item["id"]
    check(f"GET /session/{ms}/message", fetch(BUN, f"/session/{ms}/message"), fetch(RUST, f"/session/{ms}/message"))

# 7. Paginated walk: items and cursors must match page by page
FIXTURE = "ses_paritytest0000000000000001"

def walk(base):
    pages = []
    cursor = None
    while True:
        path = f"/session/{FIXTURE}/message?limit=25" + (f"&before={cursor}" if cursor else "")
        request = urllib.request.Request(base + path)
        with urllib.request.urlopen(request) as response:
            pages.append(json.load(response))
            cursor = response.headers.get("X-Next-Cursor")
        if not cursor:
            return pages

bun_pages = walk(BUN)
rust_pages = walk(RUST)
check("paginated walk page count", len(bun_pages), len(rust_pages))
for index, (bun_page, rust_page) in enumerate(zip(bun_pages, rust_pages)):
    check(f"paginated walk page {index}", bun_page, rust_page)

# 8. Single message endpoint
full = fetch(BUN, f"/session/{FIXTURE}/message")
for entry in full[:3] + full[-3:]:
    mid = entry["info"]["id"]
    check(
        f"GET /session/{FIXTURE}/message/{mid}",
        fetch(BUN, f"/session/{FIXTURE}/message/{mid}"),
        fetch(RUST, f"/session/{FIXTURE}/message/{mid}"),
    )

# 9. Todos and children
check(f"GET /session/{FIXTURE}/todo", fetch(BUN, f"/session/{FIXTURE}/todo"), fetch(RUST, f"/session/{FIXTURE}/todo"))
child = post(RUST, "/session", {"title": "Child of fixture", "parentID": FIXTURE})
check(
    f"GET /session/{FIXTURE}/children",
    fetch(BUN, f"/session/{FIXTURE}/children"),
    fetch(RUST, f"/session/{FIXTURE}/children"),
)
assert any(item["id"] == child["id"] for item in fetch(BUN, f"/session/{FIXTURE}/children"))

# 10. Project surface. The Bun instance serves a boot-time cached project
# snapshot for /project/current, so time.updated is allowed to drift; every
# other field must match the live row the Rust server serves.
check("GET /project", fetch(BUN, "/project"), fetch(RUST, "/project"))
bun_current = fetch(BUN, "/project/current")
rust_current = fetch(RUST, "/project/current")
bun_current["time"].pop("updated", None)
rust_current["time"].pop("updated", None)
check("GET /project/current (modulo cached time.updated)", bun_current, rust_current)

print()
print(f"RESULT: {'FAIL' if failures else 'PASS'} ({len(failures)} failures)")
raise SystemExit(1 if failures else 0)
