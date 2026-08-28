#!/bin/bash
# D-C 取证：空闲（提示符后阻塞）宿主 CPU 时间——红线：必须为 0。
# 用法：idle_cpu_probe.sh [秒数]
set -u
DUR="${1:-6}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IDLE="$(mktemp /tmp/ninja_idle_sh_XXXX.sh)"
printf '#!/bin/bash\nprintf "idle prompt %% "\nread _x\n' > "$IDLE"
chmod +x "$IDLE"
SHELL="$IDLE" "$ROOT/target/debug/ninja" >/dev/null 2>&1 & PID=$!
sleep "$DUR"
echo "idle wall=${DUR}s cputime=$(ps -o cputime= -p $PID | tr -d ' ')"
kill -TERM $PID 2>/dev/null
wait $PID 2>/dev/null
rm -f "$IDLE"
