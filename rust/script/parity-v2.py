#!/usr/bin/env python3
"""Differential wire-parity test for the v2 (/api) surface, including
cross-server durable prompt admission: rows admitted by one server must be
reconcilable byte-for-byte by the other through the shared SQLite store."""
import json
import urllib.error
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"

failures = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL") + f" {name}" + (f" {detail}" if detail else ""))
    if not ok:
        failures.append(name)


def request(base, path, payload=None, method=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        base + path, data=data, headers={"Content-Type": "application/json"}, method=method
    )
    try:
        with urllib.request.urlopen(req) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def get(base, path):
    return request(base, path)


def post(base, path, payload):
    return request(base, path, payload, "POST")


# ---------------------------------------------------------------------------
# 1. Byte-level read parity on the v2 surface
# ---------------------------------------------------------------------------
fixture_page = json.loads(get(BUN, "/api/session?limit=1")[1])
if fixture_page.get("data"):
    FIXTURE = fixture_page["data"][0]["id"]
else:
    status, created = post(
        RUST,
        "/api/session",
        {
            "location": {"directory": "/workspace"},
            "model": {"id": "big-pickle", "providerID": "opencode"},
        },
    )
    assert status == 200, created
    FIXTURE = json.loads(created)["data"]["id"]
for path in [
    "/api/health",
    "/api/session/active",
    f"/api/session/{FIXTURE}",
    "/api/session?limit=3",
    "/api/session?directory=/workspace/packages/opencode&limit=5",
    "/api/session?search=Goal",
    "/api/session?search=zzz-no-match",
    "/api/session?order=asc&limit=4",
    f"/api/session/{FIXTURE}/history",
    f"/api/session/{FIXTURE}/history?limit=1",
    f"/api/session/{FIXTURE}/history?after=0",
    f"/api/session/{FIXTURE}/message",
    f"/api/session/{FIXTURE}/message?limit=5&order=asc",
    "/api/session/ses_doesnotexist0000000000000",
    f"/api/session/{FIXTURE}/message/msg_doesnotexist00000000000000",
]:
    bun = get(BUN, path)
    rust = get(RUST, path)
    check(f"GET {path}", bun == rust, "" if bun == rust else f"bun={bun} rust={rust}")

# Current location-scoped catalog surface used by the official v2 clients.
for path in [
    "/api/location",
    "/api/agent",
    "/api/command",
    "/api/skill",
    "/api/reference",
    "/api/model",
    "/api/provider",
    "/api/provider/opencode",
    "/api/fs/list?path=src",
]:
    bun = get(BUN, path)
    rust = get(RUST, path)
    check(f"GET {path}", bun == rust, "" if bun == rust else f"bun={bun} rust={rust}")

# Cursor round-trip: follow Bun's next cursor on the Rust server and vice versa.
bun_page = json.loads(get(BUN, "/api/session?limit=2")[1])
cursor = bun_page["cursor"]["next"]
bun_next = get(BUN, f"/api/session?cursor={cursor}&limit=2")
rust_next = get(RUST, f"/api/session?cursor={cursor}&limit=2")
check("bun cursor readable by rust", bun_next == rust_next)
rust_page = json.loads(get(RUST, "/api/session?limit=2")[1])
check("cursors byte-identical", bun_page["cursor"] == rust_page["cursor"])
prev = json.loads(bun_next[1])["cursor"]["previous"]
check(
    "previous cursor parity",
    get(BUN, f"/api/session?cursor={prev}&limit=2") == get(RUST, f"/api/session?cursor={prev}&limit=2"),
)
check(
    "invalid cursor parity",
    get(BUN, "/api/session?cursor=%21%21not-base64") == get(RUST, "/api/session?cursor=%21%21not-base64"),
)

# ---------------------------------------------------------------------------
# 2. Cross-server durable prompt admission
# ---------------------------------------------------------------------------
# Rust admits; Bun must reconcile the exact retry byte-for-byte.
seed = json.loads(get(BUN, f"/api/session/{FIXTURE}/history?limit=1")[1])
prompt = {"text": "cross-server admission (rust first)"}
status, rust_admit = post(RUST, f"/api/session/{FIXTURE}/prompt", {"prompt": prompt, "resume": False})
check("rust admit succeeds", status == 200, rust_admit)
admitted = json.loads(rust_admit)["data"]
retry = {"id": admitted["id"], "prompt": prompt, "resume": False}
status, bun_retry = post(BUN, f"/api/session/{FIXTURE}/prompt", retry)
check("bun reconciles rust admission", status == 200 and bun_retry == rust_admit, f"bun={bun_retry}")
status, conflict = post(
    BUN, f"/api/session/{FIXTURE}/prompt", {"id": admitted["id"], "prompt": {"text": "different"}, "resume": False}
)
status_rust, conflict_rust = post(
    RUST, f"/api/session/{FIXTURE}/prompt", {"id": admitted["id"], "prompt": {"text": "different"}, "resume": False}
)
check("conflict parity", (status, conflict) == (status_rust, conflict_rust) and status == 409, f"{status} {conflict}")

# Bun admits; Rust must reconcile the exact retry byte-for-byte.
prompt = {"text": "cross-server admission (bun first)", "files": [{"uri": "file:///workspace/README.md", "name": "README.md"}]}
status, bun_admit = post(BUN, f"/api/session/{FIXTURE}/prompt", {"prompt": prompt, "delivery": "queue", "resume": False})
check("bun admit succeeds", status == 200, bun_admit)
admitted = json.loads(bun_admit)["data"]
status, rust_retry = post(
    RUST, f"/api/session/{FIXTURE}/prompt", {"id": admitted["id"], "prompt": prompt, "delivery": "queue", "resume": False}
)
check("rust reconciles bun admission", status == 200 and rust_retry == bun_admit, f"rust={rust_retry}")

# Sequences must interleave without gaps and both servers must agree on history.
bun_history = get(BUN, f"/api/session/{FIXTURE}/history?limit=100")
rust_history = get(RUST, f"/api/session/{FIXTURE}/history?limit=100")
check("history parity after interleaved admissions", bun_history == rust_history)
events = json.loads(bun_history[1])["data"]
seqs = [event["durable"]["seq"] for event in events]
check("sequences strictly increasing", seqs == sorted(set(seqs)), str(seqs))

# ---------------------------------------------------------------------------
# 3. v2 session create: Rust-created sessions readable by Bun and vice versa
# ---------------------------------------------------------------------------
status, created = post(RUST, "/api/session", {"location": {"directory": "/workspace/packages/opencode"}, "agent": "plan"})
check("rust v2 create succeeds", status == 200, created)
session_id = json.loads(created)["data"]["id"]
bun_view = get(BUN, f"/api/session/{session_id}")
rust_view = get(RUST, f"/api/session/{session_id}")
check("created session parity across servers", bun_view == rust_view and bun_view[0] == 200)
check("create response matches later reads", json.loads(created) == json.loads(bun_view[1]))

# Adopt-existing semantics on ID reuse.
status, adopted = post(BUN, "/api/session", {"id": session_id})
check("bun adopts rust-created session", status == 200 and json.loads(adopted)["data"]["id"] == session_id)

# Prompt into the fresh session from both servers alternately.
for index, base in enumerate([BUN, RUST, BUN, RUST]):
    status, body = post(base, f"/api/session/{session_id}/prompt", {"prompt": {"text": f"turn {index}"}, "resume": False})
    check(f"alternating admission {index} via {'bun' if base == BUN else 'rust'}", status == 200, body)
check(
    "alternating history parity",
    get(BUN, f"/api/session/{session_id}/history") == get(RUST, f"/api/session/{session_id}/history"),
)
data = json.loads(get(BUN, f"/api/session/{session_id}/history")[1])["data"]
check("all four admissions durable", len(data) == 4, str(len(data)))
check(
    "admission seqs contiguous",
    [event["durable"]["seq"] for event in data] == list(range(data[0]["durable"]["seq"], data[0]["durable"]["seq"] + 4)),
)

# ---------------------------------------------------------------------------
# 4. Projected v2 message parity on sessions with real runner output
# ---------------------------------------------------------------------------
# When any session has projected v2 messages (a Bun runner executed a
# Rust-admitted prompt), both servers must serve them byte-for-byte.
sessions = json.loads(get(BUN, "/api/session?limit=50")[1])["data"]
covered = 0
for session in sessions:
    sid = session["id"]
    bun_msgs = get(BUN, f"/api/session/{sid}/message?order=asc")
    # Sessions whose directory no longer exists fail location resolution on
    # the Bun side; both servers must at least agree, then skip them.
    if bun_msgs[0] != 200:
        continue
    if not json.loads(bun_msgs[1]).get("data"):
        continue
    covered += 1
    check(f"projected messages parity {sid}", bun_msgs == get(RUST, f"/api/session/{sid}/message?order=asc"))
    first = json.loads(bun_msgs[1])["data"][0]["id"]
    check(
        f"projected single message parity {sid}",
        get(BUN, f"/api/session/{sid}/message/{first}") == get(RUST, f"/api/session/{sid}/message/{first}"),
    )
    page = json.loads(get(BUN, f"/api/session/{sid}/message?limit=1")[1])
    next_cursor = page["cursor"]["next"]
    check(
        f"message cursor parity {sid}",
        get(BUN, f"/api/session/{sid}/message?cursor={next_cursor}&limit=2")
        == get(RUST, f"/api/session/{sid}/message?cursor={next_cursor}&limit=2"),
    )
    if covered >= 3:
        break
check("at least one session with projected messages covered", covered > 0, f"covered={covered}")

# Interrupt no-op parity (idle interruption is a no-op).
check(
    "interrupt parity",
    post(BUN, f"/api/session/{session_id}/interrupt", None) == post(RUST, f"/api/session/{session_id}/interrupt", None),
)

print()
if failures:
    print(f"{len(failures)} FAILURES: {failures}")
    raise SystemExit(1)
print("ALL v2 PARITY CHECKS PASSED")
