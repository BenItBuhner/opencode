#!/bin/bash
# Before/after benchmark: Bun server (4096) vs Rust server (4097).
# Requires: oha (cargo install oha), both servers running against the same DB.
set -e
export NO_COLOR=true
SID="${SID:-ses_0d12cd044ffe5gSTD3MXLgBlEm}"
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

bench "session list" "/session" 5000
bench "session get" "/session/$SID" 5000
bench "messages full" "/session/$FIXTURE/message"
bench "messages page (limit=25)" "/session/$FIXTURE/message?limit=25"
bench "project list" "/project" 5000

echo "== memory (RSS, post-load) =="
BUN_PID=$(pgrep -f 'index.ts serve --port 4096' | rg -v 'tmux|bash' | head -1)
RUST_PID=$(pgrep -f 'target/release/opencode-server' | head -1)
[ -n "$BUN_PID" ] && grep VmRSS /proc/$BUN_PID/status | awk '{printf "bun  rss=%.1f MB\n", $2/1024}'
[ -n "$RUST_PID" ] && grep VmRSS /proc/$RUST_PID/status | awk '{printf "rust rss=%.1f MB\n", $2/1024}'
