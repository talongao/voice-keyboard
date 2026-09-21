// 手机端「麦克风模式」的浏览器测试。
//
//   VK_PW=/path/to/node_modules/playwright VK_PORT=8805 node tests/browser/mic_mode.js
//
// 两种服务端模式都能跑，脚本会自适应：
//   --mode dryrun  → 探测报"可用"，走【录音链路】：真采集 + 真 POST PCM + 校验服务端收到多少
//   正常模式       → 探测报"缺驱动/缺HTTPS"，走【引导链路】：弹窗 → 分步引导
//
// 用 Chromium 的假麦克风（--use-fake-device-for-media-stream），不需要真麦克风。
const PW = process.env.VK_PW || "/tmp/shot/node_modules/playwright";
const { chromium } = require(PW);

const PORT = process.env.VK_PORT || "8805";
const BASE = `http://127.0.0.1:${PORT}`;
const CHROME = process.env.VK_CHROME ||
  "/root/.cache/hyperframes/chrome/chrome-headless-shell/linux-152.0.7928.2/chrome-headless-shell-linux64/chrome-headless-shell";

let pass = 0, fail = 0;
const ok = (m) => { console.log(`  \x1b[32m✓\x1b[0m ${m}`); pass++; };
const bad = (m, extra) => {
  console.log(`  \x1b[31m✗\x1b[0m ${m}`);
  if (extra !== undefined) console.log("      " + JSON.stringify(extra));
  fail++;
};

async function status() {
  const r = await fetch(`${BASE}/api/status`);
  return r.json();
}

(async () => {
  const st = await status();
  const key = st.pair_key || "";
  if (!key) { console.error("拿不到 pair_key，服务起了吗？"); process.exit(1); }

  const browser = await chromium.launch({
    executablePath: CHROME,
    args: [
      "--no-sandbox",
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
      // 有真波形文件就喂它（没波形时 Chromium 的假麦克风是静音的，电平表自然不会动）
      ...(require("fs").existsSync("/tmp/fake-mic.wav")
        ? ["--use-file-for-fake-audio-capture=/tmp/fake-mic.wav"] : []),
      "--autoplay-policy=no-user-gesture-required",
    ],
  });
  // 手机页就用手机视口（PC 端控制台页另开一页，用桌面视口）
  const page = await browser.newPage({
    viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true,
  });
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));

  await page.goto(`${BASE}/#k=${key}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(1200);

  // ① 标签栏存在且能切到麦克风
  const hasTabs = (await page.locator("#tab-mic").count()) === 1;
  hasTabs ? ok("手机页有「麦克风」标签") : bad("找不到麦克风标签");
  if (!hasTabs) { await browser.close(); process.exit(1); }

  await page.click("#tab-mic");
  await page.waitForTimeout(1500);
  const state = (await page.textContent("#mic-state")).trim();
  const askVisible = await page.locator("#mic-ask").isVisible();
  console.log(`  · 麦克风 Tab 状态：「${state}」（弹窗${askVisible ? "已出现" : "未出现"}）`);

  if (!askVisible) {
    // ── 可用：走录音链路 ──
    ok("探测判定可用（没有弹引导）");
    await page.click("#mic-btn");
    await page.waitForTimeout(2500);
    const recState = (await page.textContent("#mic-state")).trim();
    /正在采集/.test(recState) ? ok(`开始录音后状态正确：${recState}`) : bad(`状态不对：${recState}`);

    // 电平表应当有反应（假麦克风也有信号）
    const width = await page.evaluate(() => document.querySelector("#meter-bar").style.width);
    parseFloat(width) > 0 ? ok(`电平表有反应（${width}）`) : bad("电平表没动");

    await page.waitForTimeout(1200);
    await page.click("#mic-btn"); // 停止
    await page.waitForTimeout(1500);
    const sub = (await page.textContent("#mic-sub")).trim();
    /传输/.test(sub) ? ok(`停止后给出统计：${sub}`) : bad(`停止后没有统计：${sub}`);

    // 统计里的块数必须 > 0：这证明 worklet 真的在采、POST 真的送到了服务端
    const m = sub.match(/(\d+)\s*块/);
    m && Number(m[1]) > 0
      ? ok(`服务端真的收到了 ${m[1]} 块 PCM（整条音频链路通了）`)
      : bad(`服务端没收到音频块：${sub}`);
  } else {
    // ── 不可用：手机上应当"请求电脑端弹引导"，而不是在手机上看装驱动步骤 ──
    ok("探测判定不可用，出现了提示");
    const ask = (await page.textContent("#mic-ask-txt")).trim();
    /在电脑上/.test(ask) ? ok(`提示指向电脑端：${ask.slice(0, 40)}…`) : bad(`提示没说清在电脑上做：${ask}`);
    /装驱动、生成证书都在电脑上做/.test(ask) || /电脑上完成设置/.test(ask)
      ? ok("说明了这些活只能在电脑上做") : bad("没说明为什么要在电脑上做");

    // 点「在电脑上打开引导」→ 应当请求服务端打开电脑端引导页
    const [req] = await Promise.all([
      page.waitForRequest((r) => r.url().includes("/api/mic/guide-open"), { timeout: 5000 }).catch(() => null),
      page.click("#mic-guide-yes"),
    ]);
    req ? ok("点了之后确实请求了电脑端打开引导") : bad("没有发出 guide-open 请求");
    await page.waitForTimeout(1000);
    const after = (await page.textContent("#mic-steps")).trim();
    /电脑/.test(after) ? ok(`手机上给了后续指引：${after.slice(0, 30)}…`) : bad(`后续指引不对：${after}`);

    // 电脑端控制台页带着 ?mic=1 打开时，必须能渲染出引导步骤
    // 电脑端控制台页：**桌面视口**，别拿手机窄屏测 PC 页面
    const pc = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    const pcErrs = [];
    pc.on("pageerror", (e) => pcErrs.push(String(e)));
    await pc.goto(`${BASE}/console?mic=1`, { waitUntil: "networkidle" });
    await pc.waitForTimeout(2000);
    const steps = await pc.locator("#mic-steps .mstep").count();
    steps >= 2 ? ok(`电脑端控制台渲染出 ${steps} 步引导`) : bad(`电脑端引导没渲染：${steps} 步`);
    const why = (await pc.textContent("#mic-why")).trim();
    why.length > 4 ? ok(`电脑端显示了状态与原因：${why.slice(0, 44)}…`) : bad(`电脑端没显示原因：${why}`);
    const recheck = await pc.locator("#mic-recheck").count();
    recheck === 1 ? ok("电脑端有「重新检测」") : bad("电脑端没有重新检测按钮");
    pcErrs.length === 0 ? ok("电脑端无 JS 报错") : bad("电脑端有 JS 报错", pcErrs);
    await pc.close();
  }

  errs.length === 0 ? ok("没有 JS 报错") : bad("页面有 JS 报错", errs);

  await page.screenshot({ path: "/tmp/vk-mic-mode.png" });
  await browser.close();
  console.log();
  if (fail === 0) console.log(`\x1b[32m全部通过（${pass} 项）\x1b[0m`);
  else console.log(`\x1b[31m失败 ${fail} 项 / 通过 ${pass} 项\x1b[0m`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => { console.error("跑挂了:", e); process.exit(1); });
