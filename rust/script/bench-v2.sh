#!/bin/bash
# Before/after benchmark for the v2 (/api) surface: Bun (4096) vs Rust (4097).
# Requires: oha, both servers running against the same DB.
set -e
export NO_COLOR=true
FIXTURE="${FIXTURE:-ses_paritytest0000000000000001}"

summarize() {
python3 -c "
import json, sys
d = json.load(sys.stdin)
s = d['summary']
lp = d['latencyPercentiles']
print(f\"  rps={s['requestsPerSec']:>9.0f}  mean={s['average']*1000:6.2f}ms  p50={lp['p50']*1000:6.2f}ms  p99={lp['p99']*1000:6.2f}ms\")
"
}

bench() {
  echo "== $1 =="
  printf "bun  "
  oha -n "${3:-3000}" -c 32 --no-tui --output-format json "http://127.0.0.1:4096$2" 2>/dev/null | summarize
  printf "rust "
  oha -n "${3:-3000}" -c 32 --no-tui --output-format json "http://127.0.0.1:4097$2" 2>/dev/null | summarize
}

bench "v2 health" "/api/health" 5000
bench "v2 session list" "/api/session?limit=25" 5000
bench "v2 session get" "/api/session/$FIXTURE" 5000
bench "v2 history" "/api/session/$FIXTURE/history" 5000
bench "v2 messages" "/api/session/$FIXTURE/message" 5000

echo "== prompt admission (durable write path, sequential) =="
python3 - <<'EOF'
import json, time, urllib.request

FIXTURE = "ses_paritytest0000000000000001"

def admit(base, text):
    req = urllib.request.Request(
        f"{base}/api/session/{FIXTURE}/prompt",
        data=json.dumps({"prompt": {"text": text}, "resume": False}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as response:
        response.read()

for name, base in [("bun", "http://127.0.0.1:4096"), ("rust", "http://127.0.0.1:4097")]:
    for i in range(5):
        admit(base, f"warmup {name} {i} {time.time_ns()}")
    start = time.perf_counter()
    count = 100
    for i in range(count):
        admit(base, f"bench {name} {i} {time.time_ns()}")
    elapsed = time.perf_counter() - start
    print(f"{name:4} {count} admissions in {elapsed:.2f}s  ({count/elapsed:.0f}/s, {elapsed/count*1000:.2f}ms each)")
EOF

echo "== memory (RSS, post-load) =="
BUN_PID=$(pgrep -f '^bun run --conditions=browser ./src/index.ts serve --port 4096' | head -1)
RUST_PID=$(pgrep -f 'target/release/opencode-server' | head -1)
[ -n "$BUN_PID" ] && grep VmRSS /proc/$BUN_PID/status | awk '{printf "bun  rss=%.1f MB\n", $2/1024}'
[ -n "$RUST_PID" ] && grep VmRSS /proc/$RUST_PID/status | awk '{printf "rust rss=%.1f MB\n", $2/1024}'
