#!/usr/bin/env bash
#
# 手机端浏览器流程的端到端测试
#
# 和 tests/e2e_xvfb.sh 的分工：
#   e2e_xvfb.sh  用 curl 打接口，验「HTTP → 鉴权 → 注入层 → 应用收到了字」
#   browser/     用 Playwright 扮手机点真实页面，验前端 JS
#                （配对 / localStorage / 多地址降级 / 按钮状态 / 提示文案），
#                并且把注入结果对着一个真编辑器（vim）逐字节对账
#
# 这套环境细节踩过的坑（改动前先看）：
#   1. 接收端别用 `cat > file`：tty 行缓冲会把退格吃掉，「撤销」根本测不出来，
#      回车提交过的行也退不回去。用 vim（insert 模式）才是真编辑器语义。
#   2. 没有窗口管理器时，窗口销毁后 X 不会重画，屏幕上留残影 →
#      每次都用全新 display，而且只挪自己认识的顶层窗口（xterm 的子窗口也带名字）。
#   3. vim 要加 -n：残留的 .swp 会让它「只读」打开，存盘直接报 E45。
#
# 用法：
#   ./tests/browser/run.sh                 # 默认跑 Rust 版（主线）
#   ./tests/browser/run.sh --python        # 冻结的原型版，只作留档：Linux 下会偶发丢字
#   VK_BIN=/path/to/voice-keyboard ./tests/browser/run.sh
#   ./tests/browser/run.sh --port 8806 --display :98
#   ./tests/browser/run.sh --keep          # 跑完不清理，方便手动上去看
#
# 依赖：Xvfb xdotool xterm vim node + playwright（详见 README.md）
# 说明：只覆盖 Linux 的注入路径（XTEST）。
#      Windows 的 SendInput、macOS 的 CGEvent 必须在真机上单独验。

set -uo pipefail

PORT=8806
DISP=":98"
KEEP=0
USE_PYTHON=0
# 端口策略那一步会用到（默认 0，失败后置 1）
PORT_RC=0
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

while [ $# -gt 0 ]; do
  case "$1" in
    --port)    PORT="$2"; shift 2 ;;
    --display) DISP="$2"; shift 2 ;;
    --keep)    KEEP=1; shift ;;
    --python)  USE_PYTHON=1; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "未知参数: $1"; exit 2 ;;
  esac
done

BASE="http://127.0.0.1:$PORT"
RECV=/tmp/vk-browser-received.txt
EXPECT=/tmp/vk-browser-expected.txt
SHOTS="${VK_SHOTS:-/tmp/vk-shots}"
SRV_LOG="$(mktemp /tmp/vk-browser-server-XXXXXX.log)"
XVFB_PID=""; SRV_PID=""

c_cyan()  { printf '\n\033[36m%s\033[0m\n' "$*"; }
c_green() { printf '\033[32m  ✓ %s\033[0m\n' "$*"; }
c_red()   { printf '\033[31m  ✗ %s\033[0m\n' "$*"; }
c_dim()   { printf '\033[2m    %s\033[0m\n' "$*"; }

cleanup() {
  # 服务是用 setsid 起的（自成进程组），所以按进程组杀，能把
  # `uv run python server.py` 底下的 python 一起带走。
  # 别用 `pkill -f server.py --port N`：命令行里恰好有同样字样的
  # 其它进程（包括调这个脚本的那一层 shell）会被误杀。
  [ -n "$SRV_PID" ] && kill -- -"$SRV_PID" 2>/dev/null
  pkill -f "nano $RECV" 2>/dev/null
  pkill -f "vim -n -c startinsert $RECV" 2>/dev/null
  [ "$KEEP" -eq 0 ] && [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  # --keep 时别把这些删了，不然出问题没得看
  [ "$KEEP" -eq 0 ] && rm -f "$RECV" "$EXPECT" /tmp/.vk-browser-received.txt.sw* 2>/dev/null
  if [ "$KEEP" -eq 1 ]; then
    echo
    echo "已保留现场：DISPLAY=$DISP  服务日志=$SRV_LOG  截图=$SHOTS"
  else
    rm -f "$SRV_LOG"
  fi
}
trap cleanup EXIT

# ── 依赖 ──────────────────────────────────────────────────
c_cyan "[1/8] 检查依赖"
missing=""
for c in Xvfb xdotool xterm vim node ffmpeg; do
  command -v "$c" >/dev/null || missing="$missing $c"
done
if [ -n "$missing" ]; then
  c_red "缺少命令：$missing"
  echo "    Debian/Ubuntu: sudo apt install xvfb xdotool xterm vim nodejs ffmpeg"
  exit 1
fi

PW="${VK_PW:-}"
if [ -z "$PW" ]; then
  for cand in "$ROOT/node_modules/playwright" "$HOME/node_modules/playwright" \
              "$(npm root -g 2>/dev/null)/playwright"; do
    [ -d "$cand" ] && { PW="$cand"; break; }
  done
fi
if [ -z "$PW" ]; then
  c_red "找不到 playwright（这套测试是可选的，不想装可以跳过）"
  echo "    npm i playwright && npx playwright install chromium"
  echo "    或指定路径：VK_PW=/path/to/node_modules/playwright $0"
  exit 1
fi
c_dim "playwright = $PW"

SHELL_BIN="${CHROME_SHELL:-}"
if [ -z "$SHELL_BIN" ]; then
  SHELL_BIN="$(ls -1 "$HOME"/.cache/hyperframes/chrome/chrome-headless-shell/*/chrome-headless-shell-linux64/chrome-headless-shell 2>/dev/null | head -1)"
fi
[ -n "$SHELL_BIN" ] && c_dim "chrome        = $SHELL_BIN" || c_dim "chrome        = 用 playwright 自带的"

RUNNER="python3"
command -v uv >/dev/null 2>&1 && RUNNER="uv run python"

if [ "$USE_PYTHON" -eq 0 ] && [ -z "${VK_BIN:-}" ]; then
  for cand in "$ROOT/rust/target/release/voice-keyboard" \
              "$ROOT/rust/target/debug/voice-keyboard"; do
    [ -x "$cand" ] && { VK_BIN="$cand"; break; }
  done
  if [ -z "${VK_BIN:-}" ]; then
    c_red "找不到 Rust 版可执行文件（主线就是它）"
    echo "    先编： cargo build --release --manifest-path rust/Cargo.toml"
    echo "    或者跑冻结的原型版： $0 --python"
    exit 1
  fi
fi

# ── 起环境 ────────────────────────────────────────────────
c_cyan "[2/8] 开一块干净的虚拟屏幕 $DISP（窗口销毁不会重画，必须全新开）"
pkill -f "[X]vfb $DISP" 2>/dev/null; sleep 0.5
export DISPLAY="$DISP"
Xvfb "$DISP" -screen 0 1280x940x24 >/dev/null 2>&1 &
XVFB_PID=$!
sleep 1.5

c_cyan "[3/8] 起服务"
cd "$ROOT" || exit 1
if [ -n "${VK_BIN:-}" ]; then
  c_dim "Rust 版：$VK_BIN"
  # --strict-port：端口必须确定，不能让自动换端口把地址换跑了
  # --no-browser/--no-tray：别让浏览器/托盘起来抢焦点（CI 上踩过，见 e2e_xvfb.sh 里的注释）
  setsid "$VK_BIN" --port "$PORT" --mode auto --strict-port --no-browser --no-tray >"$SRV_LOG" 2>&1 &
else
  c_dim "Python 版：server.py --mode xdotool（已冻结，Linux 下会偶发丢字）"
  setsid $RUNNER server.py --port "$PORT" --mode xdotool >"$SRV_LOG" 2>&1 &
fi
SRV_PID=$!
for _ in $(seq 1 50); do curl -sf -o /dev/null "$BASE/api/info" && break; sleep 0.3; done
if ! curl -sf -o /dev/null "$BASE/api/info"; then
  c_red "服务没起来："; cat "$SRV_LOG"; exit 1
fi

ST="$(curl -s "$BASE/api/status")"
KEY="$(echo "$ST" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("pair_key",""))')"
PIN="$(echo "$ST" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("pin") or "")')"
[ -n "$KEY" ] || { c_red "拿不到 pair_key"; exit 1; }
c_dim "PIN=$PIN  密钥=${KEY:0:8}..."

# 程序现在没有原生窗口（PC 端界面是浏览器里的控制台页），不用再挪窗口。
c_cyan "[4/8] 起接收端：vim（insert 模式），光标就在它里面"
rm -f "$RECV" "$EXPECT" /tmp/.vk-browser-received.txt.sw*
TITLE="目标窗口-$$"
xterm -T "$TITLE" -geometry 66x40+0+0 -bg "#101216" -fg "#e8eaf0" \
      -fa "WenQuanYi Micro Hei" -fs 11 -e vim -n -c startinsert "$RECV" >/dev/null 2>&1 &
sleep 2.5
WID="$(xdotool search --name "^$TITLE\$" | head -1)"
[ -n "$WID" ] || { c_red "接收端窗口没起来"; exit 1; }
xdotool windowraise "$WID" 2>/dev/null
xdotool windowfocus "$WID" 2>/dev/null
c_dim "接收端窗口 = $WID，焦点 = $(xdotool getwindowfocus)"

c_cyan "[5/8] 手机端：Playwright 打开页面走完整流程"
VK_BASE="$BASE" VK_KEY="$KEY" VK_PIN="$PIN" VK_SHOTS="$SHOTS" \
VK_EXPECT="$EXPECT" VK_PW="$PW" CHROME_SHELL="$SHELL_BIN" \
  node "$ROOT/tests/browser/phone.js"
PHONE_RC=$?

c_cyan "[6/8] 截图后 Esc :wq 存盘"
ffmpeg -loglevel error -f x11grab -video_size 1280x940 -i "$DISP" -frames:v 1 \
       -y "$SHOTS/00-电脑桌面.png" 2>/dev/null
xdotool windowfocus "$WID" 2>/dev/null; sleep 0.4
xdotool key --clearmodifiers Escape; sleep 0.5
xdotool type --delay 40 ':wq'; sleep 0.4
xdotool key --clearmodifiers Return; sleep 1.5
if [ ! -f "$RECV" ]; then
  # 存盘失败（比如只读）时再救一次
  xdotool key --clearmodifiers Return; sleep 0.4
  xdotool key --clearmodifiers Escape; sleep 0.4
  xdotool type --delay 40 ':wq!'; sleep 0.4
  xdotool key --clearmodifiers Return; sleep 1.5
fi

c_cyan "[7/8] 对账：手机上发的 vs 电脑光标处收到的"
if [ -f "$RECV" ]; then
  echo "───── vim 里实际存下来的内容 ─────"
  cat "$RECV"
  echo "──────────────────────────────────"
else
  c_red "接收文件不存在 —— 存盘那步没成功"
fi
RECV_RC=1
# 注意：这里不能写成 `PHONE_RC -eq 0 && diff ...`。
# && 会在手机端挂掉时短路，diff 根本不跑，却报「收到的内容对不上」——
# 把排查方向带沟里去（我自己就被坑过一次）。
if [ "$PHONE_RC" -ne 0 ]; then
  c_dim "手机端没全过，先不比对收没收到东西"
fi
# 末尾换行归一化：vim 在最后一行敲回车会多留一个空行（它的 Enter 语义），
# 那是编辑器的事，不是应用注入错了。内容行必须逐字对上。
norm() { python3 -c "import sys; sys.stdout.write(open(sys.argv[1], encoding='utf-8').read().rstrip('\n'))" "$1"; }
if [ -f "$RECV" ] && [ -f "$EXPECT" ] && diff -u <(norm "$EXPECT") <(norm "$RECV") >/tmp/vk-browser-diff.txt 2>&1; then
  c_green "电脑上拿到的文字 = 手机上发的一字不差"
  RECV_RC=0
else
  c_red "收到的内容对不上"
  sed 's/^/    /' /tmp/vk-browser-diff.txt 2>/dev/null
  # 「只少了一两个字」和「整句不对」是两回事：前者是 X11 的已知竞态
  # （enigo/xdotool 都得临时改键码，改完到按键之间偶尔漏一个）。
  # 只在 Linux 上黄牌放行；Windows 上同样的现象就是真 bug，不该莫不关心。
  if [ "$(uname -s)" = "Linux" ] && [ -f "$RECV" ] && python3 - "$EXPECT" "$RECV" <<'PY'
import sys
# 先掐掉结尾换行：vim 在末尾敲回车会多留一个空行，
# 不归一化的话它正好把少掉的那个字抵掉，missing 会算成 0。
exp = open(sys.argv[1], encoding="utf-8").read().rstrip("\n")
got = open(sys.argv[2], encoding="utf-8").read().rstrip("\n")
i = 0
for ch in exp:
    if i < len(got) and got[i] == ch:
        i += 1
missing = len(exp) - len(got)
# got 是 exp 的子序列、且只少了一两个字：就是那个已知竞态
sys.exit(0 if i == len(got) and 0 < missing <= 2 else 1)
PY
  then
    c_dim "⚠ 少了几个字——Linux/X11 的已知竞态（见 README），不记为失败"
    c_dim "  Windows 上如果也少字，那是真 bug（那边不走临时改键码这条路）"
    RECV_RC=0
  fi
fi

c_cyan "[8/8] 端口策略：被占时自己换端口，但别把「已经开了一个」当成换端口"
if [ -n "${VK_BIN:-}" ]; then
  # a) 端口被别的程序占着 → 应该自己往后找一个能用的（而不是报错死掉）
  NEXT=$((PORT + 1))
  setsid python3 -m http.server "$NEXT" --bind 127.0.0.1 >/dev/null 2>&1 &
  OTHER_PID=$!
  sleep 1.5
  setsid "$VK_BIN" --port "$NEXT" --headless >/tmp/vk-portmove.log 2>&1 &
  MOVE_PID=$!
  sleep 4
  if grep -qa "改用 $((PORT + 2))" /tmp/vk-portmove.log; then
    c_green "$NEXT 被别的程序占着 → 自己换到 $((PORT + 2))"
    PORT_RC=0
  else
    c_red "没有自动换端口"
    tail -3 /tmp/vk-portmove.log | sed 's/^/    /'
    PORT_RC=1
  fi
  kill "$MOVE_PID" 2>/dev/null; kill "$OTHER_PID" 2>/dev/null

  # b) 占着的是本程序自己 → 认出来「已经有一个在跑了」，顺手把它的窗口叫出来
  #    （窗口可能收进托盘了，用户找不到入口就再双击一次 = 召回窗口）
  OUT="$(DISPLAY= timeout 15 "$VK_BIN" --port "$PORT" 2>&1)"; RC=$?
  if [ "$RC" -eq 0 ] && echo "$OUT" | grep -qa "已经有一个在跑"; then
    c_green "重复启动会被认出来，并把老窗口叫回来（不会再开一个去抢）"
    echo "$OUT" | grep -qaE "控制台页面打开|窗口叫出来" \
      && c_green "确实请求了打开界面（/api/show）" \
      || { c_red "没请求显示窗口"; PORT_RC=1; }
  else
    c_red "重复启动没被认出来（rc=$RC）"
    echo "$OUT" | tail -3 | sed 's/^/    /'
    PORT_RC=1
  fi

  # c) --strict-port：写死了端口的人（防火墙/脚本）不该被默默换掉
  OUT="$(DISPLAY= timeout 15 "$VK_BIN" --port "$PORT" --headless --strict-port 2>&1)"
  RC=$?
  if echo "$OUT" | grep -qaE "启动失败|已经有一个在跑"; then
    c_green "--strict-port 下不换端口，如实报错（rc=$RC）"
  else
    c_red "--strict-port 没拦住（rc=$RC）"
    echo "$OUT" | tail -3 | sed 's/^/    /'
    PORT_RC=1
  fi
else
  OUT="$(DISPLAY= $RUNNER server.py --port "$PORT" 2>&1)"; RC=$?
  if [ "$RC" -ne 0 ]; then
    c_green "第二个实例如实报错并退出（rc=$RC）"
  else
    c_red "第二个实例竟然没报错（rc=$RC）"
    PORT_RC=1
  fi
fi

echo
if [ "$PHONE_RC" -eq 0 ] && [ "$RECV_RC" -eq 0 ] && [ "$PORT_RC" -eq 0 ]; then
  printf '\033[32m全部通过\033[0m\n'
  exit 0
fi
printf '\033[31m有失败项（手机端=%s 接收端=%s 端口策略=%s）\033[0m\n' \
       "$PHONE_RC" "$RECV_RC" "$PORT_RC"
exit 1
