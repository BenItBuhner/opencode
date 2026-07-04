#!/usr/bin/env python3
"""Phase 3 differential tests: SSE events, durable event rows, delete, status."""
import json
import sqlite3
import threading
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"
DB = "/home/ubuntu/.local/share/opencode/opencode-local.db"

failures = []

def check(name, expected, actual):
    if expected == actual:
        print(f"PASS {name}")
        return
    failures.append(name)
    print(f"FAIL {name}\n  expected: {json.dumps(expected)[:220]}\n  actual:   {json.dumps(actual)[:220]}")

def request(base, method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(base + path, data=data, headers={"Content-Type": "application/json"}, method=method)
    with urllib.request.urlopen(req) as response:
        return json.load(response)

def sse_capture(base, count, trigger):
    """Open SSE, run trigger, return the first `count` event payloads."""
    events = []
    ready = threading.Event()

    def reader():
        req = urllib.request.Request(base + "/event")
        with urllib.request.urlopen(req, timeout=10) as response:
            ready.set()
            for raw in response:
                line = raw.decode().strip()
                if line.startswith("data:"):
                    events.append(json.loads(line[5:].strip()))
                    if len(events) >= count:
                        return

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    ready.wait(5)
    import time
    time.sleep(0.5)
    trigger()
    thread.join(8)
    return events

def normalize(event):
    event = dict(event)
    event.pop("id", None)
    if isinstance(event.get("properties"), dict):
        props = dict(event["properties"])
        info = props.get("info")
        if isinstance(info, dict):
            info = dict(info)
            info.pop("id", None)
            info.pop("slug", None)
            info.pop("time", None)
            info.pop("title", None)
            props["info"] = info
        props.pop("sessionID", None)
        event["properties"] = props
    return event

# 1. SSE shape parity for session.updated triggered on each server
bun_session = request(BUN, "POST", "/session", {"title": "SSE parity bun"})
rust_session = request(RUST, "POST", "/session", {"title": "SSE parity rust"})

bun_events = sse_capture(BUN, 2, lambda: request(BUN, "PATCH", f"/session/{bun_session['id']}", {"title": "SSE parity bun"}))
rust_events = sse_capture(RUST, 2, lambda: request(RUST, "PATCH", f"/session/{rust_session['id']}", {"title": "SSE parity rust"}))

check("SSE first frame is server.connected", normalize(bun_events[0]), normalize(rust_events[0]))
check("SSE session.updated shape", normalize(bun_events[1]), normalize(rust_events[1]))
assert bun_events[1]["type"] == rust_events[1]["type"] == "session.updated"
assert rust_events[1]["id"].startswith("evt_")

# 2. Durable event rows written by each server must have identical structure
db = sqlite3.connect(DB)
def durable_row(session_id):
    row = db.execute(
        "SELECT type, data FROM event WHERE aggregate_id = ? ORDER BY seq DESC LIMIT 1", (session_id,)
    ).fetchone()
    kind, data = row
    parsed = json.loads(data)
    return {"type": kind, "keys": sorted(parsed.keys()), "info_keys_subset": sorted(k for k in parsed.get("info", {}) if k in ("id", "slug", "projectID", "directory", "title", "version", "cost", "tokens", "time"))}

check("durable event row structure", durable_row(bun_session["id"]), durable_row(rust_session["id"]))

# 3. Durable seq increments per aggregate on the Rust side
request(RUST, "PATCH", f"/session/{rust_session['id']}", {"title": "SSE parity rust 2"})
seqs = [row[0] for row in db.execute("SELECT seq FROM event WHERE aggregate_id = ? ORDER BY seq", (rust_session["id"],))]
check("rust durable seq progression", list(range(len(seqs))), seqs)

# 4. Cross-server delete: Rust deletes a tree, Bun must report 404
parent = request(RUST, "POST", "/session", {"title": "Delete tree parent"})
child = request(RUST, "POST", "/session", {"title": "Delete tree child", "parentID": parent["id"]})
request(RUST, "DELETE", f"/session/{parent['id']}")
for sid in (parent["id"], child["id"]):
    try:
        request(BUN, "GET", f"/session/{sid}")
        failures.append(f"delete leak {sid}")
        print(f"FAIL delete leak {sid}")
    except urllib.error.HTTPError as error:
        check(f"bun 404 after rust delete ({sid[:20]})", 404, error.code)
    rows = db.execute("SELECT count(*) FROM event WHERE aggregate_id = ?", (sid,)).fetchone()[0]
    check(f"durable history removed ({sid[:20]})", 0, rows)

# 5. Status endpoint parity (idle state)
check("GET /session/status", request(BUN, "GET", "/session/status"), request(RUST, "GET", "/session/status"))

# cleanup
for sid in (bun_session["id"], rust_session["id"]):
    request(RUST, "DELETE", f"/session/{sid}")

print()
print(f"RESULT: {'FAIL' if failures else 'PASS'} ({len(failures)} failures)")
raise SystemExit(1 if failures else 0)
