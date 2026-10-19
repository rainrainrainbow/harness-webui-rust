#!/bin/bash
PID=$(for p in $(pgrep -f 'harness-webui-rust-bin'); do c=$(cat /proc/$p/comm 2>/dev/null); [ "$c" = "harness-webui-r" ] && echo $p; done | head -1)
T=$(python3 -c "import json;print(json.load(open('/root/.dsh-webui-rust/state.json'))['token'])" 2>/dev/null)
C="dsh=$T"
echo "pid=$PID token=${T:0:8}"
echo "== 1) auth gate =="
echo -n "no-cookie: "; curl -s -m 8 -o /dev/null -w '%{http_code}\n' "http://127.0.0.1:8989/"
echo -n "cookie:    "; curl -s -m 8 -o /dev/null -w '%{http_code}\n' -H "Cookie: $C" "http://127.0.0.1:8989/"
echo -n "app.js:    "; curl -s -m 8 -o /dev/null -w '%{http_code}\n' -H "Cookie: $C" "http://127.0.0.1:8989/app.js"
echo -n "style.css: "; curl -s -m 8 -o /dev/null -w '%{http_code}\n' -H "Cookie: $C" "http://127.0.0.1:8989/style.css"

echo "== 2) create session =="
SID=$(curl -s -m 8 -H "Cookie: $C" -H 'Content-Type: application/json' -d '{"title":"verify"}' "http://127.0.0.1:8989/api/sessions" | python3 -c "import sys,json;print(json.load(sys.stdin)['id'])")
echo "sid=$SID"

echo "== 3) start chat =="
curl -s -m 8 -H "Cookie: $C" -H 'Content-Type: application/json' -d "{\"s\":$SID,\"prompt\":\"修复 parser 的 bug\",\"model\":\"deepseek-v3.2\",\"mode\":\"Standard\",\"permission\":\"Standard\"}" "http://127.0.0.1:8989/api/chat"; echo

echo "== 4) SSE stream open in background =="
timeout 6 curl -s -N -H "Cookie: $C" "http://127.0.0.1:8989/api/stream?s=$SID" > /tmp/sse.txt &
SSEPID=$!
sleep 2

echo "== 5) CONCURRENT approve while SSE open (used to hang) =="
time curl -s -m 8 -H "Cookie: $C" -H 'Content-Type: application/json' -d "{\"s\":$SID,\"ok\":true}" "http://127.0.0.1:8989/api/approve"; echo

echo "== 6) CONCURRENT sessions list while streaming =="
time curl -s -m 8 -o /dev/null -w 'http=%{http_code}\n' -H "Cookie: $C" "http://127.0.0.1:8989/api/sessions"

wait $SSEPID 2>/dev/null
echo "== SSE bytes: $(wc -c < /tmp/sse.txt) =="
echo "--- event names ---"; grep -o '^event: [a-z_]*' /tmp/sse.txt | sort | uniq -c
echo "--- tail ---"; tail -3 /tmp/sse.txt
echo "== RSS =="; awk '/VmRSS/{printf "%.1f MiB\n", $2/1024}' /proc/$PID/status
