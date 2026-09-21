#!/bin/bash
# Linux 真实音频链路测试：证明"手机发来的 PCM 真的进了虚拟麦克风"。
#
#   ./tests/audio_e2e.sh
#
# 做法：程序自己建虚拟麦克风 → 灌入一段已知的 440Hz 正弦（模拟手机端每 100ms 一块）
#      → 同时用 parec 从该设备的 monitor 录回来 → 分析频率与幅度。
# 没有 PulseAudio/PipeWire 的环境（比如 CI）会明确跳过，不算失败。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${VK_BIN:-$ROOT/rust/target/release/voice-keyboard}"
PORT="${VK_PORT:-8821}"
SINK="voice-keyboard-mic"
REC=/tmp/vk-audio-rec.raw
CHUNK=/tmp/vk-audio-chunk.raw
LOG=/tmp/vk-audio-e2e.log
PASS=0; FAIL=0; SKIP=0

ok()   { printf "  \033[32m✓\033[0m %s\n" "$1"; PASS=$((PASS+1)); }
bad()  { printf "  \033[31m✗\033[0m %s\n" "$1"; FAIL=$((FAIL+1)); }
skip() { printf "  \033[33m—\033[0m %s（跳过）\n" "$1"; SKIP=$((SKIP+1)); }
head_() { printf "\n\033[36m%s\033[0m\n" "$1"; }

cleanup() {
  [ -n "${PARE_PID:-}" ] && kill "$PARE_PID" 2>/dev/null
  curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/api/shutdown" 2>/dev/null
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  wait 2>/dev/null
  return 0
}
trap cleanup EXIT

command -v pactl >/dev/null 2>&1 || { echo "没有 pactl，跳过（这个测试只在有 PulseAudio/PipeWire 的 Linux 上跑）"; exit 0; }
if ! pactl info >/dev/null 2>&1; then
  echo "PulseAudio/PipeWire 没在跑，跳过"; exit 0
fi
[ -x "$BIN" ] || { echo "先编译： cargo build --release --manifest-path rust/Cargo.toml"; exit 1; }

J() { python3 -c "import sys,json;print(json.dumps(json.load(sys.stdin),ensure_ascii=False))"; }

# 起服务：dryrun 关掉，走真实音频输出
"$BIN" --headless --no-tray --no-browser --port "$PORT" --no-auth --quiet >"$LOG" 2>&1 &
SRV_PID=$!
sleep 2

head_ "[1/5] 探测与创建虚拟麦克风"
ST=$(curl -s --max-time 5 "http://127.0.0.1:$PORT/api/mic/status")
echo "$ST" | grep -q '"ok":true' && ok "探测到音频服务可用" || bad "探测说不可用：$ST"

# 一键创建（幂等）：Linux 上这就是"零安装"那一步
SETUP=$(curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT/api/mic/setup")
echo "$SETUP" | grep -q '"ok":true' && ok "一键创建虚拟麦克风成功" || bad "一键创建失败：$SETUP"
pactl list short sinks 2>/dev/null | grep -q "$SINK" && ok "sink $SINK 已出现在系统里" || bad "没找到 sink：$SINK"

head_ "[2/5] 开麦（真实 pacat 输出）"
S=$(curl -s --max-time 10 -X POST "http://127.0.0.1:$PORT/api/mic/start")
echo "$S" | grep -q '"ok":true' && ok "开麦成功：$(echo "$S" | J)" || bad "开麦失败：$S"

head_ "[3/5] 边灌边录"
# 从虚拟麦克风的 monitor 录回来：录到的就是"电脑认为麦克风采到的声音"
timeout 12 parec --device="${SINK}.monitor" --rate=48000 --channels=1 --format=s16le >"$REC" 2>/dev/null &
PARE_PID=$!
sleep 0.6

# 生成 30 块 × 100ms 的 440Hz 正弦，按手机的节奏（每 100ms 一块）发过去
python3 - "$CHUNK" <<'PY'
import math, struct, sys
data = bytearray()
for i in range(4800):          # 100ms @48k
    data += struct.pack('<h', int(0.4 * 32767 * math.sin(2 * math.pi * 440 * i / 48000)))
open(sys.argv[1], 'wb').write(bytes(data))
PY
SENT=0
for i in $(seq 1 30); do
  CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 \
    -X POST --data-binary "@$CHUNK" -H "Content-Type: application/octet-stream" \
    "http://127.0.0.1:$PORT/api/mic/audio")
  [ "$CODE" = "200" ] && SENT=$((SENT+1))
  sleep 0.1
done
[ "$SENT" -eq 30 ] && ok "30 块 PCM 全部被接受" || bad "只有 $SENT/30 块被接受"

head_ "[4/5] 关麦与回收"
STAT=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT/api/mic/stop")
echo "$STAT" | grep -q '"ok":true' && ok "关麦返回统计：$(echo "$STAT" | J)" || bad "关麦失败：$STAT"
sleep 1
# pacat 必须被收干净：留下僵尸就说明 kill/wait 没做对
ZOMBIES=$(ps -o stat= --ppid "$SRV_PID" 2>/dev/null | grep -c '^Z')
[ "$ZOMBIES" -eq 0 ] && ok "关麦后没有僵尸子进程" || bad "有 $ZOMBIES 个僵尸进程"

sleep 0.5
kill "$PARE_PID" 2>/dev/null; wait "$PARE_PID" 2>/dev/null
pactl list short source-outputs >/dev/null 2>&1

head_ "[5/5] 分析录到的音频"
RES=$(python3 - "$REC" <<'PY'
import struct, sys, math
raw = open(sys.argv[1], 'rb').read()
n = len(raw) // 2
if n < 4800:
    print("EMPTY %d" % n); raise SystemExit
s = struct.unpack('<%dh' % n, raw[:n*2])
# 掐掉首尾各 100ms（pacat/parec 的缓冲边缘）
a, b = 4800, n - 4800
seg = s[a:b]
rms = math.sqrt(sum(v*v for v in seg) / len(seg)) / 32768
# 用零交叉估主频
cross = sum(1 for i in range(1, len(seg)) if (seg[i-1] < 0) != (seg[i] < 0))
freq = cross / 2 / (len(seg) / 48000)
print("%.4f %.1f %d" % (rms, freq, n))
PY
)
read -r RMS FREQ NSAMPLES <<<"$RES"
if [ "${RMS:-EMPTY}" = "EMPTY" ]; then
  bad "没录到任何音频（样本数 $NSAMPLES）"
else
  echo "  · 录到 $NSAMPLES 个采样，RMS=$RMS，估算主频 ${FREQ}Hz"
  python3 -c "import sys; sys.exit(0 if float('$RMS') > 0.05 else 1)" \
    && ok "录到的不是静音（RMS $RMS）" || bad "录到的基本是静音（RMS $RMS）"
  python3 -c "import sys; sys.exit(0 if 400 <= float('$FREQ') <= 480 else 1)" \
    && ok "主频约 ${FREQ}Hz，和我们灌进去的 440Hz 吻合" || bad "主频 ${FREQ}Hz 和 440Hz 差太多"
fi

echo
if [ "$FAIL" -eq 0 ]; then
  printf "\033[32m全部通过（%d 项）\033[0m\n" "$PASS"
else
  printf "\033[31m失败 %d 项 / 通过 %d 项\033[0m\n" "$FAIL" "$PASS"
  echo "服务日志：$LOG"
fi
exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
