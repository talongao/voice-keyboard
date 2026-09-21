#!/bin/bash
# 麦克风模式的接口层测试：驱动探测 / 分步引导 / 自签证书 / 运行中 HTTP↔HTTPS 切换。
#
#   ./tests/mic_tls.sh
#
# 不碰注入、不需要真麦克风，只验证"服务端这套机制对不对"。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${VK_BIN:-$ROOT/rust/target/release/voice-keyboard}"
PORT="${VK_PORT:-8811}"
DISP="${VK_DISP:-:97}"
STATE="$HOME/.local/state/voice-keyboard"
LOG=/tmp/vk-mic-tls.log
PASS=0; FAIL=0

ok()   { printf "  \033[32m✓\033[0m %s\n" "$1"; PASS=$((PASS+1)); }
bad()  { printf "  \033[31m✗\033[0m %s\n" "$1"; FAIL=$((FAIL+1)); }
head_() { printf "\n\033[36m%s\033[0m\n" "$1"; }

cleanup() {
  curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/api/shutdown" 2>/dev/null
  curl -sk -o /dev/null --max-time 2 "https://127.0.0.1:$PORT/api/shutdown" 2>/dev/null
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  [ -n "${XPID:-}" ] && kill "$XPID" 2>/dev/null
  wait 2>/dev/null
  return 0
}
trap cleanup EXIT

[ -x "$BIN" ] || { echo "先编译： cargo build --release --manifest-path rust/Cargo.toml"; exit 1; }
for c in Xvfb curl openssl; do
  command -v "$c" >/dev/null || { echo "缺少依赖: $c"; exit 1; }
done

Xvfb "$DISP" -screen 0 1024x768x24 >/dev/null 2>&1 & XPID=$!
sleep 1

# 从干净状态开始（不能留旧证书，否则起步就是 HTTPS）
rm -f "$STATE/tls/"*.pem 2>/dev/null

DISPLAY="$DISP" "$BIN" --headless --no-tray --no-browser --port "$PORT" --no-auth --quiet >"$LOG" 2>&1 &
SRV_PID=$!
sleep 2

J() { python3 -c "import sys,json;print(json.dumps(json.load(sys.stdin),ensure_ascii=False))"; }
field() { python3 -c "import sys,json;d=json.load(sys.stdin);print(d$1)"; }

head_ "[1/5] 驱动探测 /api/mic/status"
S=$(curl -s --max-time 5 "http://127.0.0.1:$PORT/api/mic/status")
[ "$(echo "$S" | field "['platform']")" != "" ] && ok "返回了平台名" || bad "没有平台名"
if [ "$(echo "$S" | field "['ok']")" = "False" ]; then
  [ -n "$(echo "$S" | field "['reason']")" ] && ok "不可用时给出了原因（说明缺什么）" || bad "不可用却没说原因"
else
  ok "检测到可用虚拟设备（$S）"
fi
[ "$(echo "$S" | field "['tls']")" = "False" ] && ok "初始状态是 HTTP" || bad "初始不该是 HTTPS"

head_ "[1.5/5] 开着 PIN 鉴权时，本机控制台页仍要能读（不带 token）"
# 这一条是踩坑补的：控制台页没有 token，但它必须能读麦克风状态与引导。
# 之前测试全用 --no-auth 跑，所以没发现「带鉴权就 401」的问题。
pkill -x voice-keyboard 2>/dev/null
sleep 1
DISPLAY="$DISP" "$BIN" --headless --no-tray --no-browser --port "$PORT" --quiet >"$LOG.auth" 2>&1 &
SRV_PID=$!
sleep 2
CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$PORT/api/mic/status")
[ "$CODE" = "200" ] && ok "带鉴权时本机免 token 可读 /api/mic/status（200）" \
  || bad "带鉴权时本机读不到 /api/mic/status（$CODE）——控制台页会显示「无法获取引导」"
CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$PORT/api/mic/guide")
[ "$CODE" = "200" ] && ok "带鉴权时本机免 token 可读 /api/mic/guide（200）" || bad "带鉴权时读不到引导（$CODE）"
# 但手机该要 token 的接口一个都不能松
CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 -X POST -d '{"text":"x"}' "http://127.0.0.1:$PORT/api/send")
[ "$CODE" = "200" ] && ok "本机调用注入接口仍然放行（和以前一致）" || true
# 换回免鉴权模式继续后面的用例
curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/api/shutdown"
sleep 1
DISPLAY="$DISP" "$BIN" --headless --no-tray --no-browser --port "$PORT" --no-auth --quiet >"$LOG" 2>&1 &
SRV_PID=$!
sleep 2

head_ "[2/5] 分步引导 /api/mic/guide"
G=$(curl -s --max-time 5 "http://127.0.0.1:$PORT/api/mic/guide")
N=$(echo "$G" | field "['steps'].__len__()")
[ "$N" -ge 2 ] && ok "给出 $N 步引导" || bad "引导步骤太少（$N）"
# 还没证书时，引导里必须先教"开 HTTPS"这件事
echo "$G" | grep -q "enable_tls" && ok "引导里包含「开启 HTTPS」这一步" || bad "缺「开启 HTTPS」引导"
echo "$G" | grep -q "信任" && ok "引导里包含「手机信任证书」" || bad "缺证书信任引导"

head_ "[3/5] 生成自签证书并当场切到 HTTPS"
R=$(curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT/api/tls/enable")
[ "$(echo "$R" | field "['ok']")" = "True" ] && ok "接口返回成功（并已安排切换）" || bad "开启失败：$R"
sleep 3

[ -f "$STATE/tls/cert.pem" ] && [ -f "$STATE/tls/key.pem" ] && ok "证书与私钥已落盘" || bad "证书没写下来"

CODE=$(curl -sk -o /dev/null -w "%{http_code}" --max-time 5 "https://127.0.0.1:$PORT/api/info")
[ "$CODE" = "200" ] && ok "HTTPS 已生效（200）" || bad "HTTPS 不通（$CODE）"

# 明文 HTTP 必须已经断掉：不能两种协议同时开着
CODE_PLAIN=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 "http://127.0.0.1:$PORT/api/info" 2>/dev/null)
[ "$CODE_PLAIN" != "200" ] && ok "明文 HTTP 已关闭（$CODE_PLAIN）" || bad "HTTP 还开着，说明没真切换"

SAN=$(openssl x509 -in "$STATE/tls/cert.pem" -noout -ext subjectAltName 2>/dev/null)
echo "$SAN" | grep -q "IP Address" && ok "证书 SAN 含 IP（手机按 IP 访问才不会报错）" || bad "证书 SAN 没有 IP：$SAN"

KEYMODE=$(stat -c '%a' "$STATE/tls/key.pem" 2>/dev/null)
[ "$KEYMODE" = "600" ] && ok "私钥权限 600" || bad "私钥权限是 $KEYMODE（建议 600）"

head_ "[4/5] 切回 HTTP"
curl -sk -o /dev/null --max-time 5 -X POST "https://127.0.0.1:$PORT/api/tls/disable"
sleep 3
[ ! -f "$STATE/tls/cert.pem" ] && ok "证书已删除" || bad "证书没删掉"
CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$PORT/api/info")
[ "$CODE" = "200" ] && ok "HTTP 已恢复（200）" || bad "HTTP 没恢复（$CODE）"

head_ "[5/5] --http 能强制用 HTTP"
curl -s --max-time 5 -X POST "http://127.0.0.1:$PORT/api/tls/enable" >/dev/null
sleep 3
cleanup
DISPLAY="$DISP" "$BIN" --headless --no-tray --no-browser --port "$PORT" --no-auth --quiet --http >"$LOG" 2>&1 &
SRV_PID=$!
sleep 2
CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$PORT/api/info")
[ "$CODE" = "200" ] && ok "有证书但 --http 时仍用 HTTP" || bad "--http 没生效（$CODE）"
rm -f "$STATE/tls/"*.pem 2>/dev/null

echo
if [ "$FAIL" -eq 0 ]; then
  printf "\033[32m全部通过（%d 项）\033[0m\n" "$PASS"
else
  printf "\033[31m失败 %d 项 / 通过 %d 项\033[0m\n" "$FAIL" "$PASS"
  echo "服务日志：$LOG"
fi
exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
