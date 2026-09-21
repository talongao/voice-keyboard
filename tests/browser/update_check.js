// 控制台页「检查更新」的测试（确定性：GitHub API 全部打桩，不依赖网络）。
//
//   VK_PW=/path/to/node_modules/playwright node tests/browser/update_check.js
//
// 需要先起一个实例，例如：
//   ./rust/target/release/voice-keyboard --headless --no-tray --no-browser --port 8803 --no-auth
const PW = process.env.VK_PW || "/tmp/shot/node_modules/playwright";
const { chromium } = require(PW);

const PORT = process.env.VK_PORT || "8803";
const URL = `http://127.0.0.1:${PORT}/console`;
const CHROME = process.env.VK_CHROME ||
  "/root/.cache/hyperframes/chrome/chrome-headless-shell/linux-152.0.7928.2/chrome-headless-shell-linux64/chrome-headless-shell";

let pass = 0, fail = 0;
const ok = (m) => { console.log(`  \x1b[32m✓\x1b[0m ${m}`); pass++; };
const bad = (m, extra) => {
  console.log(`  \x1b[31m✗\x1b[0m ${m}`);
  if (extra !== undefined) console.log("      " + JSON.stringify(extra));
  fail++;
};

(async () => {
  const browser = await chromium.launch({
    executablePath: CHROME,
    args: ["--no-sandbox", ...(process.env.VK_PROXY ? [`--proxy-server=${process.env.VK_PROXY}`] : [])],
  });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } }); // 控制台是 PC 页面，用桌面视口
  const errs = [];
  page.on("pageerror", (e) => errs.push(String(e)));

  await page.goto(URL, { waitUntil: "networkidle" });
  await page.waitForTimeout(500);

  const ver = (await page.textContent("#ver")).trim();
  /^v\d+\.\d+/.test(ver) ? ok(`页面显示版本 ${ver}`) : bad(`版本显示不对：${ver}`);

  // ① 远端更新 → 提示"有新版本"并给下载链接
  await page.route("**/api.github.com/**", (r) =>
    r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ tag_name: "v99.0.0" }) }));
  await page.click("#chk");
  await page.waitForTimeout(1200);
  let txt = (await page.textContent("#upd")).trim();
  /有新版本 v99\.0\.0/.test(txt) ? ok(`识别出新版本：${txt}`) : bad(`没识别出新版本：${txt}`);
  const href = await page.getAttribute("#upd a", "href");
  href && href.includes("releases") ? ok("带上了下载链接") : bad(`下载链接不对：${href}`);

  // ② 远端还是当前版本 → 已是最新
  await page.unroute("**/api.github.com/**");
  await page.route("**/api.github.com/**", (r) =>
    r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ tag_name: ver }) }));
  await page.click("#chk");
  await page.waitForTimeout(1200);
  txt = (await page.textContent("#upd")).trim();
  /已是最新/.test(txt) ? ok(`同版本判定正确：${txt}`) : bad(`同版本判定错：${txt}`);

  // ③ 查不到（断网 / 被墙）→ 不能白屏或报错，要给人话
  await page.unroute("**/api.github.com/**");
  await page.route("**/api.github.com/**", (r) => r.abort());
  await page.click("#chk");
  await page.waitForTimeout(1200);
  txt = (await page.textContent("#upd")).trim();
  /检查失败/.test(txt) ? ok(`网络不通时给了人话：${txt}`) : bad(`网络不通时提示不对：${txt}`);

  // ④ 页面不能因此报 JS 错误
  errs.length === 0 ? ok("没有 JS 报错") : bad("页面有 JS 报错", errs);

  await browser.close();
  console.log();
  if (fail === 0) console.log(`\x1b[32m全部通过（${pass} 项）\x1b[0m`);
  else console.log(`\x1b[31m失败 ${fail} 项 / 通过 ${pass} 项\x1b[0m`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => { console.error("跑挂了:", e); process.exit(1); });
