#!/bin/bash
# D-C 取证：\r 重写型压力（进度条/spinner 类——单行 Partial 帧，
# 渲染路径占大头）下宿主 CPU 时间；修前/修后同一脚本同一时长对比。
set -u
TAG="${1:?label}"
DUR="${2:-6}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SPEW="$(mktemp /tmp/ninja_spin_XXXXXX)"
cat > "$SPEW" <<'EOF'
#!/bin/bash
i=0
while true; do
  printf '\rspin %08d working...' "$i"
  i=$((i+1))
done
EOF
chmod +x "$SPEW"

SHELL="$SPEW" "$ROOT/target/debug/ninja" >/dev/null 2>&1 & PID=$!
sleep "$DUR"
echo "$TAG wall=${DUR}s cputime=$(ps -o cputime= -p $PID | tr -d ' ')"
kill -TERM $PID 2>/dev/null
wait $PID 2>/dev/null
rm -f "$SPEW"
