# 语音键盘 —— 仓库规则（给 agent 和人都看）

改代码前先读完。

> ⚠️ **这个仓库是公开的。** 本地环境信息、私有基础设施一律不许进来：
> 本机代理地址与端口、私有对象存储 / 分发通道的地址与凭据、真实内网 IP 与主机名、
> 配对 token 与密钥、个人访问令牌的 scope 清单、本机绝对路径（`/tmp/...`、`~/...`）。
> 文档和截图需要举例时，用演示值（如 `192.168.1.100`、`DEMO-...`）。
> 提交前自查：`grep -rniE 'talon|oss|licell|proxy|192\\.168\\.0|/tmp/' --include='*' -I .`
> 历史里进过敏感内容的，光删文件不够——要重写历史（见「发版与仓库维护」）。这个仓库的协作方式**只用 GitHub**（以前挂过 Gitee，已经弃用）。

## 仓库沿革

仓库在 2026-09 因**历史中出现过本机环境信息**而整体重建过一次：旧的提交、PR、Release
全部删除，改为从 `v0.1.15` 重新开始（历史只保留初始的三条提交）。所以你会看到
git 历史很短——这是有意为之，不是丢数据。

## 仓库与分支

- 权威仓库：**`git@github.com:talongao/voice-keyboard.git`**（public，remote 名 `origin`）
- 默认分支：`master`
- 所有开发都在 **`develop`** 上；**`master` 只能通过 PR 更新**

| 分支 | 规矩 |
|---|---|
| `develop` | 开发分支，对公众可见（仓库是公开的）。**不要直接往 master 推东西** |
| `master` | 只能通过 PR 进来，且 CI 四个 job 必须绿。不许直推、不许 force push |

## 提交 / 推送前的硬性检查（已装 hooks）

仓库自带钩子，`core.hooksPath` 指向 `.githooks`：

```bash
git config core.hooksPath .githooks     # 新克隆的机器先跑这一句
```

- **`.githooks/pre-commit`**：在 `master`/`main` 上提交 → 直接拒绝
- **`.githooks/pre-push`**：推送 `master` → 直接拒绝（提示走 PR）
  - 唯一的例外是**新仓库引导**：远端还没有 master 时没有任何 PR 可走，
    此时用 `git push --no-verify origin master` 推一次（仅此一次，之后一律走 PR）

> 这是本地防线；GitHub 那侧还有 ruleset（无 bypass、要 PR、要四个检查绿），
> 所以就算绕过钩子直推，远端也会拒（实测过：`push declined due to repository rule violations`）。

## 标准流程

```bash
# 1) 开发（永远在 develop 上）
git switch develop
git commit -am "..."
git push origin develop

# 2) 开 PR（head 就是 develop）
gh pr create --base master --head develop --title "..." --body "..."

# 3) 等 CI 绿（PR 上会跑 windows / macos / linux / test）
gh pr checks --watch

# 4) 合并：必须 rebase（master 要求线性历史，merge commit 会被拒）
gh pr merge --rebase

# 5) 合并后同步本地
#    ⚠️ rebase 合并会**重写提交 sha**，master 与 develop 的历史会分叉，
#      所以不能用 ff 合并，直接对齐 origin/master（内容是一致的）
git fetch origin
git reset --hard origin/master
git push --force-with-lease origin develop
```

发版（合并之后做）：

```bash
git tag -a v0.1.15 -m "v0.1.15：..."      # 只打在 master 的提交上
git push origin v0.1.15
# → GitHub Actions 自动发 Release（4 个资产）
```

## CI（GitHub Actions）

`.github/workflows/build.yml`，五个 job：

| job | 干什么 |
|---|---|
| windows | MSVC 编单文件 exe |
| macos | M 系列 + Intel 都编 → lipo 合成通用二进制 → 裸二进制 tar.gz **+ `.app` 包 zip** |
| linux | 单文件二进制 |
| test | `tests/e2e_xvfb.sh`（Xvfb + 真注入 + 比对文件） |
| release | 只有打 tag 时跑：把三平台产物发成 GitHub Release |

触发：push master / 指向 master 的 PR / tag / 手动。
`push develop` **不触发**（省得每次开发都跑一遍）。公开仓库 Actions 不计分钟。

## GitHub 侧配置（当前状态，别乱动）

- 仓库 `talongao/voice-keyboard`（public），默认分支 `master`
- ruleset `master`（id 23749654）：**enforcement = Active**，**bypass list = 空**
- 规则：`deletion` / `non_fast_forward` / `required_linear_history` /
  `pull_request`（0 approval，只允许 rebase/squash 合并）/
  `required_status_checks`（必需：`windows` `macos` `linux` `test`）
- ⚠️ **千万别开 `update`（Restrict updates）那条规则**：它的语义是"只有 bypass 用户能更新"，
  配上空的 bypass list 等于**谁都合并不进来**（踩过：PR 全绿但 mergeStateStatus=BLOCKED）

## 构建与发版备忘

- Windows 交叉编译（Linux 上）：`rustup target add x86_64-pc-windows-gnu`，
  然后 `cargo build --release --manifest-path rust/Cargo.toml --target x86_64-pc-windows-gnu`
- **macOS 不能从 Linux 交叉编译**（需要 Apple SDK + macOS 链接器），所以 macOS 产物
  一律由 CI 的 macos runner 出：两个架构分别编、`lipo -create` 合成通用二进制、
  再组 `.app` 包（菜单栏图标必须在 `.app` 里）
- 发布产物以 **GitHub Release** 为准；打 tag 由 CI 自动构建并发布

## 测试怎么跑（本地）

```bash
cargo build --release --manifest-path rust/Cargo.toml
./tests/e2e_xvfb.sh                    # 接口层：HTTP → 鉴权 → 注入 → 应用真收到字
VK_PW=/path/to/node_modules/playwright ./tests/browser/run.sh   # 手机端：真浏览器点页面
# （VK_PW 不设也行，脚本会在 ./node_modules、~/node_modules、全局 npm root 里找）
```

## 试过但放弃的：麦克风模式（v0.1.16 有，v0.1.17 移除）

做过一版"手机当电脑麦克风"（音频直传）。完整实现与踩坑记录都在 git 历史里
（tag `v0.1.16`、PR #7），这里只记**为什么砍掉**，免得将来再走一遍：

- **手机必须信任自签证书**：浏览器只在安全上下文（https / localhost）下给麦克风，
  而局域网 IP 没有域名、CA 不可能签发 → 只能自签，iOS 还要装描述文件 + 手动开信任开关。
- **电脑要装虚拟声卡，还可能改掉默认设备**：Windows 装 VB-CABLE 后系统常把默认输出
  切到它（用户扬声器就没声音）；macOS 装 BlackHole 还要重启；只有 Linux 零安装。
- **网页必须保持前台**：移动端浏览器息屏或切后台就暂停录音（原生 App 才能后台跑）。
- 而收益只是"不用手机输入法的语音输入" —— 成本明显高于收益，砍掉，回到"借输入法"这条线。

## 踩过的坑（别再踩）

1. **Linux/X11 偶发丢字**：X11 没"直接输入任意 Unicode 字符"的接口，注入得临时改键码，
   忙的时候偶尔漏一两个字（实测 Rust 版 8 轮里 1 轮掉 2 个字）。
   两个测试对"只少 1~2 个字的子序列"黄牌放行（仅 Linux），其它差异照旧红牌。
   Windows/macOS 不走这条路，不受影响。
2. **测试脚本起服务必须加 `--no-browser`**：程序默认会开浏览器控制台页，
   在装了浏览器的机器（GitHub runner 有 Firefox）上**浏览器会抢走键盘焦点**，
   之后注入的字全跑浏览器里 —— 现象是"第一次成功、之后全丢"。
3. **macOS 的菜单栏图标要先授权辅助功能**（系统设置 → 隐私与安全性 → 辅助功能）：
   不授权的话图标**不出现且不报错**（注入也会被静默挡掉）。
4. **macOS 托盘只能在 `.app` 包里**：裸二进制从终端跑碰 AppKit 会 abort
   （`CGSConnectionByID` 断言，拦不住）。程序自己判断：不在 `.app` 里就跳过托盘。
5. **Windows 的托盘/菜单靠主线程消息循环派发**：主线程去 `sleep` 轮询的话，
   连右键菜单都弹不出来（托盘"点了没反应"的根因）。
