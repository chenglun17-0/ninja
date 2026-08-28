#!/bin/bash
# D-C 取证：压力输出（yes 式全速）下宿主 CPU 时间；修前/修后用同一脚本同一时长对比。
# 用法：cpu_pressure_probe.sh <标签> [秒数]
set -u
TAG="${1:?label}"
DUR="${2:-6}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NINJA="$ROOT/target/debug/ninja"
[ -x "$NINJA" ] || { echo "no ninja binary (cargo build first)" >&2; exit 1; }

SPEW="$(mktemp /tmp/ninja_spew_XXXXXX)"
cat > "$SPEW" <<'EOF'
#!/bin/bash
exec /usr/bin/yes "pressure line 0123456789 abcdefghijklmnopqrstuvwxyz"
EOF
chmod +x "$SPEW"

SHELL="$SPEW" "$NINJA" >/dev/null 2>&1 & PID=$!
sleep "$DUR"
CPU_PS=$(ps -o cputime= -p $PID | tr -d ' ')
CPU_MS=$(ps -o cputime= -p $PID 2>/dev/null | awk -F'[ :.]' '{print ($1*60+$2)*1000}')
kill -TERM $PID 2>/dev/null
wait $PID 2>/dev/null
rm -f "$SPEW"
echo "$TAG wall=${DUR}s cputime=$CPU_PS"
