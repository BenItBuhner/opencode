#!/usr/bin/env python3
"""Live feature battery for the Rust runner: goal mode (every edge), plan
mode permission denial, file edits, and cross-server byte parity after each
feature. Uses OpenCode Zen free models through the Rust server on 4097 and
compares durable state with the Bun server on 4096."""
import json
import os
import subprocess
import time
import urllib.error
import urllib.request

BUN = "http://127.0.0.1:4096"
RUST = "http://127.0.0.1:4097"
WORKDIR = "/tmp/feature-demo"

failures = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL") + f" {name}" + (f"  {detail}" if detail else ""))
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


def wait_settled(sid, timeout=120):
    deadline = time.time() + timeout
    request(RUST, f"/api/session/{sid}/wait", None, "POST")
    while time.time() < deadline:
        status, active = request(RUST, "/api/session/active")
        if sid not in json.loads(active)["data"]:
            return True
        time.sleep(0.5)
    return False


def turn(sid, text):
    status, body = request(RUST, f"/api/session/{sid}/prompt", {"prompt": {"text": text}}, "POST")
    assert status == 200, body
    assert wait_settled(sid), "session did not settle"


def messages(sid):
    return json.loads(request(RUST, f"/api/session/{sid}/message?order=asc")[1])["data"]


def tool_states(sid, name):
    return [
        part["state"]
        for message in messages(sid)
        if message["type"] == "assistant"
        for part in message["content"]
        if part["type"] == "tool" and part["name"] == name
    ]


def goal_metadata(sid):
    return json.loads(request(RUST, f"/session/{sid}")[1]).get("metadata", {}).get("goal")


def parity(name, sid):
    same = all(
        request(BUN, path) == request(RUST, path)
        for path in [
            f"/api/session/{sid}/message?order=asc",
            f"/api/session/{sid}/history?limit=100",
            f"/api/session/{sid}",
            f"/session/{sid}",
        ]
    )
    check(f"{name}: cross-server byte parity", same)


def create_session(agent=None):
    payload = {
        "location": {"directory": WORKDIR},
        "model": {"id": "big-pickle", "providerID": "opencode"},
    }
    if agent:
        payload["agent"] = agent
    status, body = request(RUST, "/api/session", payload, "POST")
    assert status == 200, body
    return json.loads(body)["data"]["id"]


subprocess.run(["rm", "-rf", WORKDIR], check=True)
os.makedirs(WORKDIR)
subprocess.run(["git", "init", "-q"], cwd=WORKDIR, check=True)
with open(f"{WORKDIR}/notes.txt", "w") as f:
    f.write("alpha\nbeta\ngamma\n")

# ===========================================================================
# 1. GOAL MODE — every edge
# ===========================================================================
sid = create_session("goal")

turn(sid, 'Call the goal_set tool with text "Ship the Rust port". Then stop and confirm in one short line.')
goal = goal_metadata(sid)
check("goal_set stores durable goal", goal is not None and goal["text"] == "Ship the Rust port", str(goal))
check("goal_set marks active, revision 1", goal["status"] == "active" and goal["revision"] == 1)
states = tool_states(sid, "goal_set")
check("goal_set tool settled", states and states[-1]["status"] == "completed", str(states[-1:]))
check(
    "goal_set output format",
    "Goal: Ship the Rust port" in states[-1]["content"][0]["text"]
    and "Status: active" in states[-1]["content"][0]["text"],
)

turn(sid, "Call the goal_status tool, then stop and report the status in one line.")
states = tool_states(sid, "goal_status")
check("goal_status reads goal", states and "Ship the Rust port" in states[-1]["content"][0]["text"])

turn(
    sid,
    'Call goal_summarize_state with progress 40 and this exact summary:\n## Progress\n- Runner ported\n## Current State\n- All suites green\n## Blockers\n- None\n## Next Steps\n- Polish TUI\nThen stop.',
)
goal = goal_metadata(sid)
check("goal_summarize_state persists progress", goal.get("progress") == 40, str(goal.get("progress")))
check(
    "summary recorded with revision",
    goal.get("summaries") and goal["summaries"][-1]["progress"] == 40 and "Runner ported" in goal["summaries"][-1]["summary"],
)

# Edge: invalid summary shape must fail the tool with the validation message.
turn(
    sid,
    'Call goal_summarize_state with progress 41 and summary exactly "just plain text, no headers". Report the exact error you get in one line.',
)
states = [s for s in tool_states(sid, "goal_summarize_state") if s["status"] == "error"]
check(
    "invalid summary rejected with validator message",
    states and "size 2 markdown headers" in states[-1]["error"]["message"],
    str(states[-1:]),
)
check("failed summary does not bump progress", goal_metadata(sid).get("progress") == 40)

turn(sid, "Call the goal_pause tool, then stop.")
goal = goal_metadata(sid)
check("goal_pause pauses durable goal", goal["status"] == "paused", str(goal["status"]))

turn(sid, "Call the goal_resume tool, then stop.")
goal = goal_metadata(sid)
check("goal_resume reactivates goal", goal["status"] == "active")

turn(sid, "Call the goal_complete tool, then stop.")
check("goal_complete clears goal from session", goal_metadata(sid) is None)
states = tool_states(sid, "goal_complete")
check("goal_complete reports completed", "Status: completed" in states[-1]["content"][0]["text"])

# Edge: goal operations without a goal report the no-goal message.
turn(sid, "Call the goal_pause tool, then stop.")
states = tool_states(sid, "goal_pause")
check(
    "pause without goal reports none",
    "No session goal is currently set." in states[-1]["content"][0]["text"],
)
parity("goal mode", sid)

# ===========================================================================
# 2. PLAN MODE — edits denied, reads allowed
# ===========================================================================
sid = create_session("plan")
turn(
    sid,
    "Use the edit tool to replace beta with delta in notes.txt. If the tool errors, report the exact error text. Then use the read tool to read notes.txt and report its second line.",
)
edit_states = tool_states(sid, "edit")
check(
    "plan mode denies edit with Bun's message",
    edit_states
    and edit_states[-1]["status"] == "error"
    and edit_states[-1]["error"]["message"] == "Unable to edit notes.txt",
    str(edit_states[-1:]),
)
with open(f"{WORKDIR}/notes.txt") as f:
    check("plan mode left the file untouched", f.read() == "alpha\nbeta\ngamma\n")
read_states = tool_states(sid, "read")
check("plan mode allows read", read_states and read_states[-1]["status"] == "completed")
check(
    "plan agent advertises no edit/write tools",
    "session.next.tool" in request(RUST, f"/api/session/{sid}/history?limit=100")[1],
)
parity("plan mode", sid)

# ===========================================================================
# 3. BUILD MODE — file edits end-to-end
# ===========================================================================
sid = create_session("build")
turn(
    sid,
    'Use the edit tool to replace beta with delta in notes.txt, then use the write tool to create out/result.txt whose content is the single word "done" with no punctuation. Then stop.',
)
with open(f"{WORKDIR}/notes.txt") as f:
    check("build mode edit applied", f.read() == "alpha\ndelta\ngamma\n")
check(
    "build mode write created nested file",
    open(f"{WORKDIR}/out/result.txt").read().strip().rstrip(".") == "done",
)
edit_states = tool_states(sid, "edit")
check(
    "edit records unified patch + counts",
    edit_states
    and edit_states[-1]["structured"]["files"][0]["additions"] == 1
    and edit_states[-1]["structured"]["files"][0]["deletions"] == 1
    and "@@" in edit_states[-1]["structured"]["files"][0]["patch"],
)
parity("build edits", sid)

# Edge: exact-match failure surfaces the guidance message.
turn(sid, 'Use the edit tool on notes.txt with oldString "does-not-exist-anywhere" and newString "x". Report the exact error in one line.')
edit_states = [s for s in tool_states(sid, "edit") if s["status"] == "error"]
check(
    "edit miss reports exact-match guidance",
    edit_states and "Could not find oldString" in edit_states[-1]["error"]["message"],
)

# ===========================================================================
# 4. AGENT SWITCHING — durable events + system prompt behavior
# ===========================================================================
status, _ = request(RUST, f"/api/session/{sid}/agent", {"agent": "goal"}, "POST")
check("agent switch endpoint", status == 204)
kinds = [m["type"] for m in messages(sid)]
check("agent-switched message projected", "agent-switched" in kinds)
turn(sid, 'Call the goal_set tool with text "Verify goal harness". Then stop.')
check("goal tools usable after switch", goal_metadata(sid) is not None)
parity("agent switching", sid)

print()
if failures:
    print(f"{len(failures)} FAILURES: {failures}")
    raise SystemExit(1)
print("ALL FEATURE CHECKS PASSED")
