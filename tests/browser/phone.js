// 用 Playwright 扮演真实手机，把手机端那一层 JS 逻辑真跑一遍。
//
// 为什么需要它：e2e_xvfb.sh 是拿 curl 打 HTTP 接口，测不到前端 JS
// （配对流程、localStorage、多地址降级、按钮状态、提示文案）。
// 这里走的是真人操作路径：打开页面 → 扫码配对 → 打字 → 点发送 →
// 发现错字撤销 → 改完重发 → 关回车 → 停顿自动发送 → 刷新 → 手输 PIN。
//
// 环境变量（run.sh 会传好）：
//   VK_BASE       服务地址，如 http://127.0.0.1:8806
//   VK_KEY        pair_key（二维码里带的那种长密钥）
//   VK_PIN        6 位配对码
//   VK_SHOTS      截图目录
//   VK_EXPECT     把「应该出现在电脑光标处」的文本写到这里，给 shell 比对
//   VK_PW         playwright 模块路径（默认直接 require("playwright")）
//   CHROME_SHELL  chrome-headless-shell 路径（留空则用 playwright 自带的）
//   VK_FONT_PORT  测试字体服务器端口（默认 8799）
const fs = require("fs");
const path = require("path");
const http = require("http");

const PW = process.env.VK_PW || "playwright";
const { chromium } = require(PW);

const SHELL = process.env.CHROME_SHELL || "";
const BASE = process.env.VK_BASE || "http://127.0.0.1:8806";
const KEY = process.env.VK_KEY || "";
const PIN = process.env.VK_PIN || "";
const SHOTS = process.env.VK_SHOTS || "/tmp/vk-shots";
const EXPECT = process.env.VK_EXPECT || "";
const FONT_PORT = Number(process.env.VK_FONT_PORT || 8799);

// iPhone 14 视口 + 触摸 + 中文环境
const IPHONE = {
  viewport: { width: 390, height: 844 },
  deviceScaleFactor: 2,
  isMobile: true,
  hasTouch: true,
  locale: "zh-CN",
  userAgent:
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 " +
    "(KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
};

// chrome-headless-shell 不走系统 fontconfig，中文会渲染成方块、emoji 会变豆腐。
// 真机（iOS/Android）有系统字体，不需要这段——纯粹是测试环境的补丁。
const CJK = [
  "/usr/share/fonts/truetype/cjk/NotoSansCJKsc-Regular.otf",
  "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
].find((f) => fs.existsSync(f));
const EMOJI = ["/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"].find((f) =>
  fs.existsSync(f)
);

const FONT_CSS = [
  CJK && `@font-face{font-family:"PingFang SC";src:url("http://127.0.0.1:${FONT_PORT}/cjk")}`,
  CJK && `@font-face{font-family:"Microsoft YaHei";src:url("http://127.0.0.1:${FONT_PORT}/cjk")}`,
  EMOJI && `@font-face{font-family:"Noto Color Emoji";src:url("http://127.0.0.1:${FONT_PORT}/emoji")}`,
  // 页面自己的 font-family 串里没有 emoji 字体，而 headless shell 又找不到
  // 系统 emoji 字体——只能把整条串重写一遍，把 emoji 字体接到最后。
  `*{font-family:-apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei",${EMOJI ? '"Noto Color Emoji",' : ""}sans-serif !important}`,
]
  .filter(Boolean)
  .join("\n");

let PASS = 0;
let FAIL = 0;
const ok = (m) => {
  PASS++;
  console.log(`  \x1b[32m✓ ${m}\x1b[0m`);
};
const bad = (m, extra) => {
  FAIL++;
  console.log(`  \x1b[31m✗ ${m}\x1b[0m`);
  if (extra !== undefined) console.log(`      ${JSON.stringify(extra)}`);
};
const eq = (got, want, m) => (got === want ? ok(m) : bad(m, { got, want }));

function startFontServer() {
  const srv = http.createServer((req, res) => {
    const file = req.url === "/emoji" ? EMOJI : CJK;
    if (!file) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, {
      "Content-Type": req.url === "/emoji" ? "font/ttf" : "font/otf",
      "Access-Control-Allow-Origin": "*",
    });
    res.end(fs.readFileSync(file));
  });
  return new Promise((r) => srv.listen(FONT_PORT, () => r(srv)));
}

function watch(page, nets, errs) {
  page.on("response", async (r) => {
    if (!r.url().includes("/api/")) return;
    let body = "";
    try {
      body = (await r.text()).slice(0, 100);
    } catch (e) {}
    nets.push({
      status: r.status(),
      method: r.request().method(),
      path: r.url().split("/api/")[1],
      body,
    });
  });
  page.on("pageerror", (e) => errs.push("pageerror: " + e.message));
  page.on("console", (m) => {
    if (m.type() === "error") errs.push("console: " + m.text());
  });
  page.on("requestfailed", (r) => errs.push("reqfailed: " + r.url()));
}

const injectFonts = (page) => page.addStyleTag({ content: FONT_CSS });
const shot = (page, name) => page.screenshot({ path: path.join(SHOTS, name) });
const toastNow = (page) =>
  page.evaluate(() => document.querySelector("#toast").textContent);

// 提示条只显示 1.9 秒，而且是同一个 DOM 节点反复用。
// 必须等「旧的消失了」+「新的匹配上了」，否则读到的是上一条提示。
const waitToastGone = (page, timeout = 4000) =>
  page
    .waitForFunction(
      () => !document.querySelector("#toast").className.includes("show"),
      { timeout }
    )
    .catch(() => {});

async function waitToastMatch(page, re, timeout = 20000) {
  await page.waitForFunction(
    (src) => {
      const t = document.querySelector("#toast");
      return t.className.includes("show") && new RegExp(src).test(t.textContent);
    },
    re.source,
    { timeout }
  );
  return toastNow(page);
}

// 真实用户操作：打完字 → 按发送 → 等提示。
// 注入期间发送按钮是灰的，不等它恢复，这一次点击会被丢掉。
async function sendText(page, text, expect = /^已插入 \d+ 字$/) {
  await page.fill("#ta", text);
  await page.waitForFunction(() => !document.querySelector("#send").disabled, {
    timeout: 15000,
  });
  await waitToastGone(page);
  await page.click("#send");
  return waitToastMatch(page, expect);
}

const chipOn = (page, sel) =>
  page.evaluate((s) => document.querySelector(s).classList.contains("on"), sel);

// 配对成功后页面还会去拉一次 /api/endpoints，拉完才把顶栏改成「已连接」。
// 直接读会撞上「连接中…」，所以这里等一下再断言。
async function waitConnected(page, timeout = 10000) {
  await page
    .waitForFunction(
      () => document.querySelector("#status").textContent === "已连接",
      { timeout }
    )
    .catch(() => {});
  return page.textContent("#status");
}

// 多地址降级后顶栏地址才会被改写
async function waitAddr(page, want, timeout = 10000) {
  await page
    .waitForFunction(
      (w) => document.querySelector("#addr").textContent === w,
      want,
      { timeout }
    )
    .catch(() => {});
  return page.textContent("#addr");
}

const TEXT_A = "第一句：来自手机的中文 🎉";
const TEXT_EDITED = TEXT_A + "（已修改）";
const TEXT_B = "不回车第二句";
const TEXT_C = "自动发送测试";
const TEXT_D = "降级地址也通了";

(async () => {
  fs.mkdirSync(SHOTS, { recursive: true });
  const fontSrv = await startFontServer();
  const browser = await chromium.launch({
    ...(SHELL ? { executablePath: SHELL } : {}),
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
  });
  const errs = [];

  // ═══ 场景 1：扫码直连（#k=）→ 打字 → 发送 ═══
  console.log("\n\x1b[36m[场景 1] 扫码配对 → 打字 → 发送\x1b[0m");
  const ctx1 = await browser.newContext(IPHONE);
  const p1 = await ctx1.newPage();
  const nets1 = [];
  watch(p1, nets1, errs);

  await p1.goto(`${BASE}/#k=${KEY}`, { waitUntil: "domcontentloaded" });
  await injectFonts(p1);
  await p1.waitForSelector("#gate.hide", { state: "attached", timeout: 10000 });
  ok("扫二维码进来，不用输 PIN 就配对上了");

  eq(await p1.evaluate(() => location.hash), "", "地址栏里的密钥已清掉（不留历史）");
  const token = await p1.evaluate(() => localStorage.getItem("vk_token") || "");
  token ? ok("token 已存 localStorage，下次打开免配对") : bad("token 没存下来");
  eq(await waitConnected(p1), "已连接", "顶栏状态 = 已连接");
  eq(
    await waitAddr(p1, BASE.replace(/^https?:\/\//, "")),
    BASE.replace(/^https?:\/\//, ""),
    "顶栏显示的是电脑地址"
  );
  await shot(p1, "01-已配对.png");

  await p1.fill("#ta", TEXT_A);
  await shot(p1, "02-输入中.png");
  await waitToastGone(p1);
  await p1.click("#send");
  const t1 = await waitToastMatch(p1, /^已插入 \d+ 字$/);
  eq(t1, `已插入 ${[...TEXT_A].length} 字`, "发送后提示字数正确");
  eq(await p1.inputValue("#ta"), "", "发送后输入框清空，可以接着打下一句");
  (await p1.isEnabled("#undo")) ? ok("「重来」按钮亮起") : bad("「重来」按钮没亮");
  await shot(p1, "03-已发送.png");

  // ═══ 场景 2：发现错字 → 撤销 → 改完重发 ═══
  console.log("\n\x1b[36m[场景 2] 撤销 → 改错字 → 重发\x1b[0m");
  await waitToastGone(p1);
  await p1.click("#undo");
  const t2 = await waitToastMatch(p1, /^已退回/);
  eq(t2, "已退回，改完再发", "撤销提示正确");
  eq(await p1.inputValue("#ta"), TEXT_A, "原文退回输入框，电脑那边也退掉了");
  !(await p1.isEnabled("#undo"))
    ? ok("撤销后按钮变灰，不能连撤两次")
    : bad("撤销后按钮还亮着");
  await shot(p1, "04-撤销回输入框.png");

  await sendText(p1, TEXT_EDITED);
  eq(await p1.inputValue("#ta"), "", "改完重发成功");

  // ═══ 场景 3：回车由电脑端说了算（手机端没有这个开关了）═══
  console.log("\n\x1b[36m[场景 3] 回车策略归电脑端：PC 关掉 → 手机发也不回车\x1b[0m");
  const setEnter = (on) =>
    fetch(`${BASE}/api/settings`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: "Bearer " + token },
      body: JSON.stringify({ allow_enter: on }),
    });

  await setEnter(false);
  const t3 = await sendText(p1, TEXT_B);
  eq(t3, `已插入 ${[...TEXT_B].length} 字`, "电脑端关掉自动回车后，手机端照样能发（只是不回车）");
  ok("（这一步是电脑端的开关生效，不是手机端的）");

  // 手机端那颗「回车」键：关着自动回车时唯一的出路
  console.log("\n\x1b[36m[场景 4] 手机端的「回车」「退格」键\x1b[0m");
  const nets4 = nets1.length;
  await p1.click("#k-enter");
  await p1.waitForTimeout(400);
  const enterReqs = nets1.slice(nets4).filter((n) => n.path === "key");
  enterReqs.length === 1 && enterReqs[0].status === 200
    ? ok("点「回车」→ 电脑端收到一次回车")
    : bad("回车键没发出去", nets1.slice(nets4));
  eq(JSON.parse(enterReqs[0]?.body || "{}").key, "enter", "发的是 enter");
  await shot(p1, "15-回车键.png");

  const nets5 = nets1.length;
  await p1.click("#k-backspace");
  await p1.waitForTimeout(300);
  JSON.parse(nets1.slice(nets5).filter((n) => n.path === "key")[0]?.body || "{}").key ===
  "backspace"
    ? ok("「退格」键也能用")
    : bad("退格键没发出去", nets1.slice(nets5));
  // 刚才那个退格把上面敲的回车删掉了，补回来——
  // 顺便也验了「退格只退一格」（多退的话最后对账会少字）
  await p1.click("#k-enter");
  await p1.waitForTimeout(300);
  ok("再敲一次回车补回来");

  await setEnter(true); // 恢复成默认（后面场景 7 还要用）
  const t4 = await sendText(p1, TEXT_C);
  eq(t4, `已插入 ${[...TEXT_C].length} 字`, "电脑端开关打开后又回车了");

  // ═══ 场景 5：刷新后配对与设置都还在 ═══
  console.log("\n\x1b[36m[场景 5] 刷新页面，配对与设置要留住\x1b[0m");
  await p1.reload({ waitUntil: "domcontentloaded" });
  await injectFonts(p1);
  await p1.waitForSelector("#gate.hide", { state: "attached", timeout: 8000 });
  eq(await waitConnected(p1), "已连接", "刷新后免配对，直接就是已连接");
  eq(await chipOn(p1, "#c-stick"), true, "摇杆开关记住了");
  await shot(p1, "07-刷新后.png");

  const endpoint200 = nets1.some(
    (n) => n.method === "GET" && n.path === "endpoints" && n.status === 200
  );
  endpoint200
    ? ok("GET /api/endpoints 返回 200（api() 动词那个 bug 没复发）")
    : bad("GET /api/endpoints 不是 200", nets1.filter((n) => n.path === "endpoints"));

  // ═══ 场景 6：看不到二维码时手输 PIN ═══
  console.log("\n\x1b[36m[场景 6] 没用二维码，手输 PIN\x1b[0m");
  const ctx2 = await browser.newContext(IPHONE);
  const p2 = await ctx2.newPage();
  const nets2 = [];
  watch(p2, nets2, errs);
  await p2.goto(BASE, { waitUntil: "domcontentloaded" });
  await injectFonts(p2);
  await p2.waitForSelector("#gate:not(.hide)", { timeout: 8000 });
  ok("没配对过，先弹出配对码输入框");
  await shot(p2, "08-待配对.png");

  await p2.fill("#pin", "000000");
  await p2.waitForFunction(
    () => document.querySelector("#gate-err").textContent.length > 0,
    { timeout: 8000 }
  );
  const gateErr = await p2.textContent("#gate-err");
  gateErr.includes("PIN") || gateErr.includes("配对")
    ? ok(`配对码错了会拦下来：「${gateErr}」`)
    : bad("错误 PIN 的提示不对", gateErr);
  eq(await p2.inputValue("#pin"), "", "错误后自动清空，方便重输");
  await shot(p2, "09-配对码错误.png");

  await p2.fill("#pin", PIN);
  await p2.waitForSelector("#gate.hide", { state: "attached", timeout: 8000 });
  ok("输入正确 PIN 后配对成功");
  eq(await waitConnected(p2), "已连接", "PIN 配对后状态正常");

  // ═══ 场景 7：电脑 IP 变了，多地址自动降级 ═══
  console.log("\n\x1b[36m[场景 7] 电脑 IP 变了，多地址自动降级\x1b[0m");
  const ctx3 = await browser.newContext(IPHONE);
  await ctx3.addInitScript(
    ([tok, base]) => {
      localStorage.setItem("vk_token", tok);
      localStorage.setItem("vk_base", "http://127.0.0.1:9"); // 死地址，必然连不上
      localStorage.setItem("vk_endpoints", JSON.stringify([base]));
    },
    [token, BASE]
  );
  const p3 = await ctx3.newPage();
  const nets3 = [];
  watch(p3, nets3, errs);
  await p3.goto(BASE, { waitUntil: "domcontentloaded" });
  await injectFonts(p3);
  await p3.waitForSelector("#gate.hide", { state: "attached", timeout: 8000 });
  eq(
    await waitAddr(p3, BASE.replace(/^https?:\/\//, "")),
    BASE.replace(/^https?:\/\//, ""),
    "死地址失败后，自动切到能用的那个地址"
  );
  eq(
    await p3.evaluate(() => localStorage.getItem("vk_base")),
    BASE,
    "新地址写回了 localStorage"
  );

  const t7 = await sendText(p3, TEXT_D);
  eq(t7, `已插入 ${[...TEXT_D].length} 字`, "降级到备用地址后照样发得出去");
  await shot(p3, "10-多地址降级.png");

  // ═══ 场景 7b：PC 端按键（关掉「发送后回车」之后就靠它了）═══
  console.log("\n\x1b[36m[场景 7b] 单独敲回车（关着自动回车时的唯一出路）\x1b[0m");
  const keysBefore = nets3.length;
  await p3.click("#k-enter");
  await p3.waitForTimeout(500);
  const keyReqs = nets3.slice(keysBefore).filter((n) => n.path === "key");
  keyReqs.length && keyReqs[0].status === 200
    ? ok("点「回车」→ 电脑那边收到一次回车")
    : bad("回车按钮没发出去", nets3.slice(keysBefore));
  eq(JSON.parse(keyReqs[0]?.body || "{}").key, "enter", "发的是 enter");
  eq(await p3.textContent("#status"), "已连接", "敲完键状态没乱");

  const backBefore = nets3.length;
  await p3.click("#k-backspace");
  await p3.waitForTimeout(400);
  const backReqs = nets3.slice(backBefore).filter((n) => n.path === "key");
  JSON.parse(backReqs[0]?.body || "{}").key === "backspace"
    ? ok("「退格」也能用")
    : bad("退格没发出去", nets3.slice(backBefore));
  await shot(p3, "15-回车按钮.png");

  // ═══ 场景 8：滚轮摇杆 ═══
  console.log("\n\x1b[36m[场景 8] 滚轮摇杆（拖动 → 电脑跟着滚）\x1b[0m");
  const p4 = p1; // 用回第一个页面，它已经配对好了
  const stick = await p4.$("#stick");
  stick ? ok("摇杆默认就显示（手机上打开就能用）") : bad("没找到摇杆");
  await shot(p4, "11-滚轮摇杆.png");

  const before = nets1.filter((n) => n.path === "scroll").length;
  const box = await stick.boundingBox();
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;

  // 往上拖住不放，发几次滚动（往上的方向在协议里是 dy 负数）
  await p4.mouse.move(cx, cy);
  await p4.mouse.down();
  await p4.mouse.move(cx, cy - 34, { steps: 6 });
  await p4.waitForTimeout(320);
  const knobUp = await p4.evaluate(
    () => document.querySelector("#knob-stick").style.transform
  );
  const knobOff = (s) => {
    const m = /translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)/.exec(s || "");
    return m ? Math.hypot(Number(m[1]), Number(m[2])) : 0;
  };
  await p4.mouse.up();
  await p4.waitForTimeout(120);
  const knobBack = await p4.evaluate(
    () => document.querySelector("#knob-stick").style.transform
  );
  await shot(p4, "12-摇杆往上拖.png");

  const ups = nets1.filter((n) => n.path === "scroll");
  ups.length > before
    ? ok(`拖动期间持续发滚动请求（${ups.length - before} 次）`)
    : bad("拖动没发滚动请求");
  const firstDy = ups.length ? JSON.parse(ups[ups.length - 1].body).dy : 0;
  firstDy < 0 ? ok(`往上拖 → dy=${firstDy}（负数=往上滚，方向对）`) : bad("滚动方向反了", ups.slice(-3));
  knobOff(knobUp) > 5 ? ok(`摇杆跟着手指走（偏移 ${Math.round(knobOff(knobUp))}px）`) : bad("摇杆没跟手", knobUp);
  knobOff(knobBack) < 0.5 ? ok("松手回中") : bad("松手没回中", knobBack);

  // 往右拖 → dx 正数（横向滚动）
  const beforeX = nets1.filter((n) => n.path === "scroll").length;
  await p4.mouse.move(cx, cy);
  await p4.mouse.down();
  await p4.mouse.move(cx + 34, cy, { steps: 6 });
  await p4.waitForTimeout(200);
  await p4.mouse.up();
  const xs = nets1.filter((n) => n.path === "scroll").slice(beforeX);
  const dx = xs.length ? JSON.parse(xs[xs.length - 1].body).dx : 0;
  dx > 0 ? ok(`往右拖 → dx=${dx}（正数=往右滚）`) : bad("横向滚动方向不对", xs.slice(-3));

  // 摇杆开关
  await p4.click("#c-stick");
  eq(await chipOn(p4, "#c-stick"), false, "点一下，摇杆收起来了");
  eq(
    await p4.evaluate(() => document.querySelector("#stick-row").classList.contains("hide")),
    true,
    "收起来后那一行真的隐藏（把空间还给输入框）"
  );
  await shot(p4, "13-摇杆收起.png");
  await p4.click("#c-stick");
  eq(await chipOn(p4, "#c-stick"), true, "再点一下又回来了");

  // ═══ 收尾：网络与控制台 ═══
  console.log("\n\x1b[36m[场景 9] 全程网络与控制台\x1b[0m");
  const all = nets1.concat(nets2, nets3);
  // 故意试探的失败不算：401 未授权、以及场景 6 那条错误 PIN 的 403
  const intended = (n) =>
    n.status === 401 || (n.path === "pair" && n.body.includes("PIN 不对"));
  const bads = all.filter((n) => n.status >= 400 && !intended(n));
  bads.length === 0
    ? ok(`共 ${all.length} 个 /api 请求，没有意外失败`)
    : bad("有失败请求", bads);

  // 127.0.0.1:9 连不上、以及那条 403，都是上面故意造的，浏览器会各记一条控制台错误
  const realErrs = errs.filter(
    (e) => !/127\.0\.0\.1:9|ERR_UNSAFE_PORT|status of 403/.test(e)
  );
  realErrs.length === 0 ? ok("页面没有其他 JS 报错") : bad("页面报错了", realErrs);

  // 手机上依次发了这些，电脑光标处最终应该就是这个
  // 手机上依次发的：改好的第一句（回车）→ 不回车那句（PC 端关着自动回车）
  // → 手机点「回车」补上换行 → 后面又发了一句带回车的
  fs.writeFileSync(EXPECT, TEXT_EDITED + "\n" + TEXT_B + "\n" + TEXT_C + "\n" + TEXT_D + "\n");

  await browser.close();
  fontSrv.close();
  console.log(
    `\n${FAIL === 0 ? "\x1b[32m" : "\x1b[31m"}手机端：${PASS} 项通过 / ${FAIL} 项失败\x1b[0m`
  );
  process.exit(FAIL === 0 ? 0 : 1);
})().catch((e) => {
  console.error("\x1b[31m模拟中断：\x1b[0m", e.message);
  process.exit(2);
});
