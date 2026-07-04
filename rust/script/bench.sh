#!/bin/bash
# Before/after benchmark: Bun server (4096) vs Rust server (4097)
set -e
export NO_COLOR=true
SID=ses_0d12cd044ffe5gSTD3MXLgBlEm

summarize() {
python3 -c "
import json, sys
d = json.load(sys.stdin)
s = d['summary']
lp = d['latencyPercentiles']
print(f\"  rps={s['requestsPerSec']:>9.0f}  mean={s['average']*1000:6.2f}ms  p50={lp['p50']*1000:6.2f}ms  p99={lp['p99']*1000:6.2f}ms\")
"
}

for name in "list /session" "get /session/$SID"; do
  path="${name#* }"
  label="${name%% *}"
  echo "== $label =="
  printf "bun  "
  oha -n 5000 -c 32 --no-tui --output-format json "http://127.0.0.1:4096$path" 2>/dev/null | summarize
  printf "rust "
  oha -n 5000 -c 32 --no-tui --output-format json "http://127.0.0.1:4097$path" 2>/dev/null | summarize
done

echo "== memory (RSS, post-load) =="
BUN_PID=$(pgrep -f 'src/index.ts serve --port 4096' | head -1)
RUST_PID=$(pgrep -f 'opencode-server --directory' | head -1)
grep VmRSS /proc/$BUN_PID/status | awk '{printf "bun  rss=%.1f MB\n", $2/1024}'
grep VmRSS /proc/$RUST_PID/status | awk '{printf "rust rss=%.1f MB\n", $2/1024}'
