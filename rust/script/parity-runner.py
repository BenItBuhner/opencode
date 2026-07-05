#!/usr/bin/env python3
"""End-to-end checks for the Rust serialized runner (live OpenCode Zen calls).

Creates a fresh session, lets the Rust server execute real provider turns
(text-only and tool loop), and asserts that the durable trace is structurally
correct and byte-identical when served by either server."""
import json
import time
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
        with urllib.request.urlopen(req, timeout=180) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


# 1. Create a session bound to a Zen free model and run one Rust turn.
status, created = request(
    RUST,
    "/api/session",
    {"location": {"directory": "/workspace/rust"}, "model": {"id": "big-pickle", "providerID": "opencode"}},
    "POST",
)
check("create session", status == 200, created)
sid = json.loads(created)["data"]["id"]

status, admitted = request(
    RUST,
    f"/api/session/{sid}/prompt",
    {"prompt": {"text": "What is 12+5? Reply with only the number."}},
    "POST",
)
check("prompt admitted with wake", status == 200, admitted)

status, active = request(RUST, "/api/session/active")
check("session active during drain", json.loads(active)["data"].get(sid, {}).get("type") == "running", active)

status, _ = request(RUST, f"/api/session/{sid}/wait", None, "POST")
check("wait settles", status == 204)

status, messages = request(RUST, f"/api/session/{sid}/message?order=asc")
data = json.loads(messages)["data"]
check("user message projected", any(m["type"] == "user" for m in data))
assistant = [m for m in data if m["type"] == "assistant"]
check("assistant message projected", len(assistant) == 1, messages)
texts = [c["text"] for m in assistant for c in m["content"] if c["type"] == "text"]
check("assistant answered", any("17" in t for t in texts), str(texts))
check("step settled", assistant[-1].get("finish") == "stop", str(assistant[-1].get("finish")))
check("tokens accounted", assistant[-1]["tokens"]["input"] > 0, str(assistant[-1].get("tokens")))

# 2. Durable trace is well-formed and byte-identical across servers.
status, history = request(RUST, f"/api/session/{sid}/history?limit=100")
events = json.loads(history)["data"]
kinds = [e["type"] for e in events]
check("trace starts with admission", kinds[0] == "session.next.prompt.admitted", str(kinds))
check("prompted precedes step", kinds.index("session.next.prompted") < kinds.index("session.next.step.started"))
check("step ended settles trace", kinds[-1] == "session.next.step.ended", str(kinds))
seqs = [e["durable"]["seq"] for e in events]
check("sequences contiguous", seqs == list(range(seqs[0], seqs[0] + len(seqs))), str(seqs))
for path in [f"/api/session/{sid}/message?order=asc", f"/api/session/{sid}/history?limit=100", f"/api/session/{sid}"]:
    check(f"byte parity {path.split('/')[-1]}", request(BUN, path) == request(RUST, path))

# 3. Tool loop: force a read call and require a continuation turn.
status, _ = request(
    RUST,
    f"/api/session/{sid}/prompt",
    {"prompt": {"text": "Use the read tool to read Cargo.toml and report the workspace members. Be brief."}},
    "POST",
)
check("tool prompt admitted", status == 200)
request(RUST, f"/api/session/{sid}/wait", None, "POST")
deadline = time.time() + 120
while time.time() < deadline:
    status, active = request(RUST, "/api/session/active")
    if sid not in json.loads(active)["data"]:
        break
    time.sleep(0.5)

status, history = request(RUST, f"/api/session/{sid}/history?limit=100")
kinds = [e["type"] for e in json.loads(history)["data"]]
called = kinds.count("session.next.tool.called")
succeeded = kinds.count("session.next.tool.success")
failed = kinds.count("session.next.tool.failed")
check("tool call recorded durably", called >= 1, str(kinds))
check("all tool calls settled", called == succeeded + failed, f"called={called} ok={succeeded} failed={failed}")
check("continuation turn ran", kinds.count("session.next.step.started") >= 3, str(kinds.count("session.next.step.started")))
check("trace settles after tools", kinds[-1] == "session.next.step.ended")
status, messages = request(RUST, f"/api/session/{sid}/message?order=asc")
final_texts = [
    c["text"]
    for m in json.loads(messages)["data"]
    if m["type"] == "assistant"
    for c in m["content"]
    if c["type"] == "text"
]
check("tool answer mentions members", any("server" in t and "client" in t for t in final_texts), str(final_texts[-1:]))
check(
    "byte parity after tool loop",
    request(BUN, f"/api/session/{sid}/message?order=asc") == request(RUST, f"/api/session/{sid}/message?order=asc"),
)

# 4. Cross-runner continuation: the Bun runner extends the Rust-run session.
status, _ = request(BUN, f"/api/session/{sid}/prompt", {"prompt": {"text": "Now reply with exactly: HANDOFF-OK"}}, "POST")
check("bun continuation admitted", status == 200)
deadline = time.time() + 120
while time.time() < deadline:
    status, messages = request(BUN, f"/api/session/{sid}/message?order=asc")
    data = json.loads(messages)["data"]
    texts = [c["text"] for m in data if m["type"] == "assistant" for c in m["content"] if c["type"] == "text"]
    if any("HANDOFF-OK" in t for t in texts):
        break
    time.sleep(1)
check("bun runner continued rust session", any("HANDOFF-OK" in t for t in texts), str(texts[-1:]))
check(
    "byte parity after cross-runner handoff",
    request(BUN, f"/api/session/{sid}/message?order=asc") == request(RUST, f"/api/session/{sid}/message?order=asc"),
)

print()
if failures:
    print(f"{len(failures)} FAILURES: {failures}")
    raise SystemExit(1)
print("ALL RUNNER CHECKS PASSED")
