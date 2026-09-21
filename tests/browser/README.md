# 手机端浏览器流程的端到端测试

用 Playwright 扮演一台 iPhone，把**手机端那一层 JS 真的点一遍**：

```
Playwright（真实页面：扫码 → 配对 → 打字 → 点按钮）
   ↓ HTTP
真服务（默认被 Test 的 Rust 可执行文件；--python 走冻结的原型版）
   ↓ 真注入（enigo+XTEST / xdotool）
Xvfb 上的 vim（insert 模式）← 光标在这里
   ↓ Esc :wq 存盘
和「手机上依次发的内容」逐字节比对
```

## 和 `tests/e2e_xvfb.sh` 的分工

| | e2e_xvfb.sh | 这个 |
|---|---|---|
| 驱动方式 | curl 打接口 | 真浏览器点页面 |
| 能测到 | HTTP → 鉴权 → 注入层 → 应用收到字 | 前端 JS：配对流程、localStorage、多地址降级、按钮禁用状态、提示文案 |
| 测不到对方 | 前端 JS 一行都没跑 | —（注入那一层由 vim 兜底验证） |

两个都跑才算完整。历史上就是**curl 全绿、手机端却是坏的**：
前端 `api()` 一律用 POST，而 `/api/endpoints` 只接受 GET，curl 直接测 GET 从来没暴露过。

## 跑

默认跑 **Rust 版（主线）**；没编可执行文件时会提示你去编：

```bash
cargo build --release --manifest-path rust/Cargo.toml
./tests/browser/run.sh

./tests/browser/run.sh --keep               # 跑完留着现场，可以自己上去看
./tests/browser/run.sh --port 8807 --display :97
```

`--python` 跑已冻结的原型版（只作留档，Linux 下会偶发丢字，见下）；
`VK_BIN=/path/to/voice-keyboard` 可以指定用哪一个可执行文件。

退出码：0 全过；1 有失败项；2 参数/依赖问题。

## 依赖

```bash
sudo apt install xvfb xdotool xterm vim nodejs ffmpeg
npm i playwright && npx playwright install chromium
```

装在别处的话指定一下：

```bash
VK_PW=/path/to/node_modules/playwright ./tests/browser/run.sh
CHROME_SHELL=/path/to/chrome-headless-shell ./tests/browser/run.sh
```

没装 playwright 会直接报错退出——这套是可选的，CI 里不想装就跳过。

## 覆盖的场景

1. 扫码直连（`#k=`）→ 地址栏密钥清掉 → token 落 localStorage
2. 打字发送 → 提示字数 → 输入框清空 → 「重来」亮起
3. 撤销 → 原文退回输入框 → 电脑那边也真退掉了 → 按钮变灰
4. 回车策略归电脑端：`/api/settings` 关掉自动回车 → 手机端发也不回车
5. 手机端「回车」「退格」键：关着自动回车时的唯一出路；乱传按键名被拒
6. 刷新页面：免配对 + 摇杆开关记住
7. 看不到二维码时手输 PIN：错码被拦 + 清空重输 + 对码成功
8. 电脑换 IP：base 是死地址时自动降级到备用地址，并写回 localStorage
9. 滚轮摇杆：拖住往上下/左右发 `/api/scroll`、方向对（上=dy 负数）、
   摇杆跟手、松手回中、开关能收起来
10. 全程 `/api` 请求无意外失败、页面无 JS 报错
11. 端口策略：被别的程序占着→自己换端口；占了的是自己→认出来说「已经有一个在跑了」；
    `--strict-port` 下不换端口、如实报错

截图落在 `/tmp/vk-shots`（可用 `VK_SHOTS` 改）。

## 环境坑（改这个测试前先看）

0. **起服务一定要加 `--no-browser`**。程序默认会打开浏览器控制台页；在装了浏览器的
   机器上（比如 GitHub Actions 的 runner），浏览器一起来就**抢走键盘焦点**，
   后面注入的字全跑到浏览器里去了 —— 现象是「第一次发送成功、之后全丢」，
   而不是报错。本地没浏览器，所以从没复现过；是 CI 的真日志
   （接收端文件只剩第一行）抓出来的。

1. **接收端别用 `cat > file`**。tty 行缓冲会把退格吃掉：回车提交过的行退不回去，
   `undo` 就永远测不出来（当年 e2e_xvfb.sh 只能靠「发的时候不带回车」绕过去）。
   用 `vim -c startinsert` 才是真编辑器语义：退格真的删字。
2. **vim 要加 `-n`**。残留的 `.swp`（比如上一轮 nano 留下的）会让 vim 以只读打开，
   `:wq` 直接报 `E45: 'readonly' option is set`——看着像应用没注入，其实是存不了盘。
3. **没有窗口管理器时，窗口销毁后 X 不会重画**，屏幕会留残影、看起来像"注入没生效"。
   所以每轮都开一块全新的 Xvfb display。
4. **xterm 的内部子窗口也带名字**。挪窗口只能挪顶层那个（按精确标题找），
   挪错了会把文字区搬走，屏幕上一片空白。
5. **chrome-headless-shell 不走系统 fontconfig**，中文会变方块、emoji 会变豆腐。
   `phone.js` 里起了个小字体服务器把 Noto CJK / Noto Color Emoji 喂进去——
   真机（iOS/Android）有系统字体，不需要这段。

## 已知不覆盖

- **Windows / macOS 的注入路径**（`SendInput` / `CGEvent`）：必须在真机上验，
  这里只能覆盖 Linux（xdotool / XTEST）。
- **真手机的输入法语音输入**：Playwright 只能模拟打字，点不了系统键盘的麦克风。

## 已知 flake：Linux/X11 上偶尔丢字（两条落都中招）

> **Windows / macOS 不受影响**（那边不 remap 键码，见下），
> 这条是 X11 的死疾，不是我们哪一版写错了。

不走 curl、真去比对收到的字时，`--python` 偶尔会看到失败长这样：

```
第一句：来自手的中文 🎉（已修改）      ← 少了一个「机」
```

内容本身对，就是少一两个字——**这是注入层的锅，不是前端**。
X11 上没有“直接输入任意 Unicode 字符”的接口，`xdotool` 只能把一个空闲键码
临时改成目标字符再按一下；机器一忙，这个「改映射 → 按键」的窗口里就可能漏掉一个字。

实测数据（每轮发 40～80 个中文字，库里就是这套脚本）：

| 服务 | 注入方式 | 结果 |
|---|---|---|
| Python `server.py --mode xdotool`（已冻结） | `xdotool type --delay 6` | 4 轮里 3 轮各掉 1～2 个字 |
| Rust `voice-keyboard`（主线） | enigo + XTEST | 8 轮里 1 轮掉了 2 个字，其余 0 |

把间隔从 6ms 提到 12ms 没改善（各 480 字都 0 掉），说明是竞态，不是节拍：
窗口在「XChangeKeyboardMapping → 按键」之间，应用偶尔读到的是还没生效的映射。
Rust 版概率低得多，但不是零。

所以：

- 看到「只少了几个字」时，`run.sh` 会自己把这段提示打出来（靠判断
  「收到的文本是不是期望文本的子序列」）。
- **Windows / macOS 没这个问题**：那边走 `SendInput` + `KEYEVENTF_UNICODE` /
  `CGEventKeyboardSetUnicodeString`，字符直接交给系统，不改键盘映射。
