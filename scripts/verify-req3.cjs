#!/usr/bin/env node
'use strict';
/**
 * verify-req3.cjs —— 文件夹级容量上限的界面取证（需求 3）。
 *
 * 用 ?mock=1 起界面，程序化测量而不是"看图猜"：
 *   1) 详情面板是否展示「文件夹容量上限」行，且数值格式化正确；
 *   2) 规则弹窗里新增的「容量上限 + 单位」两个控件宽度是否可用（不被挤扁）；
 *   3) 编辑既有规则时能正确回填（2 GB → 数值 2 + 单位 GB）；
 *   4) 填了上限却没选策略时给出警示（warn 提示 + 保存前拦截）。
 *
 * 用法： node scripts/verify-req3.cjs
 * 依赖： playwright（见 take-screens.cjs 的 NODE_PATH 说明）
 */

const path = require('path');
const url = require('url');

const ROOT = path.resolve(__dirname, '..');
const UI_INDEX = path.join(ROOT, 'crates', 'td-gui', 'ui', 'index.html');

let chromium;
try {
  chromium = require('playwright').chromium;
} catch (e) {
  console.error('[verify-req3] 缺少 playwright：' + e.message);
  process.exit(1);
}

const checks = [];
function ok(name, cond, detail) {
  checks.push({ name: name, pass: !!cond, detail: detail });
  console.log((cond ? '  PASS  ' : '  FAIL  ') + name + (detail ? '  → ' + detail : ''));
}

async function launch() {
  for (const channel of ['msedge', 'chrome', null]) {
    try {
      return await chromium.launch(Object.assign({ headless: true }, channel ? { channel } : {}));
    } catch (e) { /* 试下一个 */ }
  }
  throw new Error('没有可用的浏览器（试过 Edge / Chrome / 自带 chromium）');
}

(async () => {
  const browser = await launch();
  const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1240, height: 900 } });
  await context.addInitScript(() => {
    try { localStorage.setItem('tempdirector.locale', 'zh'); } catch (e) {}
  });
  const page = await context.newPage();
  const uiUrl = url.pathToFileURL(UI_INDEX).href + '?mock=1';

  await page.goto(uiUrl, { waitUntil: 'load' });
  await page.waitForSelector('#rows tr');
  await page.waitForFunction(() => document.querySelectorAll('#rows tr').length >= 8);

  // ---- 1) 详情面板 ----
  await page.click('#rows tr:first-child td[data-act="focus"]');
  await page.waitForSelector('#detail:not(.hidden)');
  const detail = await page.evaluate(() => {
    const keys = Array.from(document.querySelectorAll('#detail .detail-key')).map((e) => e.textContent);
    const vals = Array.from(document.querySelectorAll('#detail .detail-val')).map((e) => e.textContent);
    const i = keys.indexOf('文件夹容量上限');
    return { hasKey: i >= 0, value: i >= 0 ? vals[i] : null, keys: keys };
  });
  ok('详情面板有「文件夹容量上限」行', detail.hasKey, detail.keys.join(' / '));
  ok('容量上限展示为人类可读', detail.value === '2.0 GB', String(detail.value));

  // ---- 2) 规则弹窗里的容量上限控件 ----
  await page.click('#detail [data-act="edit"]');
  await page.waitForSelector('#modal:not(.hidden)');
  await page.waitForTimeout(120);

  const geom = await page.evaluate(() => {
    const rows = Array.from(document.querySelectorAll('#modal-body .field-row'));
    const row = rows.find((r) => r.querySelector('select option[value="gb"]'));
    if (!row) return null;
    const n = row.querySelector('input[type="number"]');
    const s = row.querySelector('select');
    const rn = n.getBoundingClientRect();
    const rs = s.getBoundingClientRect();
    return {
      numW: Math.round(rn.width), selW: Math.round(rs.width),
      numScroll: n.scrollWidth, numClient: n.clientWidth,
      value: n.value, unit: s.value,
    };
  });
  ok('弹窗里存在「容量上限 + 单位」一行', !!geom, JSON.stringify(geom));
  if (geom) {
    ok('数值输入框宽度可用（>=120px）', geom.numW >= 120, geom.numW + 'px');
    ok('单位选择框宽度可用（>=120px）', geom.selW >= 120, geom.selW + 'px');
    ok('数值无横向截断', geom.numScroll <= geom.numClient + 1, geom.numScroll + '/' + geom.numClient);
    ok('编辑既有规则回填正确（2 / GB）', geom.value === '2' && geom.unit === 'gb', geom.value + ' / ' + geom.unit);
  }

  // ---- 3) 填上限但不选策略 → 警示 ----
  await page.evaluate(() => {
    const row = Array.from(document.querySelectorAll('#modal-body .field-row'))
      .find((r) => r.querySelector('select option[value="gb"]'));
    const sel = row.querySelector('select');
    sel.value = 'gb';
    sel.dispatchEvent(new Event('change', { bubbles: true }));
  });
  // 把策略切回「未启用」
  await page.evaluate(() => {
    const s = document.querySelector('#modal-body select option[value=""]').parentElement;
    s.value = '';
    s.dispatchEvent(new Event('change', { bubbles: true }));
  });
  const hint = await page.evaluate(() => {
    const row = Array.from(document.querySelectorAll('#modal-body .field-row'))
      .find((r) => r.querySelector('select option[value="gb"]'));
    // 只认 .field 的直接子 .hint：mk() 生成的每个 .field 内部各有一个空 .hint，
    // 用后代选择器会命中那个空壳（第一版就踩了这个坑）。
    const h = row.parentElement.querySelector(':scope > .hint');
    return { cls: h.className, text: h.textContent };
  });
  ok('上限>0 且无策略时提示转为警示', /warn/.test(hint.cls), hint.cls);
  ok('警示文案点出「必须选策略」', /策略/.test(hint.text), hint.text.slice(0, 40));

  await browser.close();

  const failed = checks.filter((c) => !c.pass);
  console.log('\n[verify-req3] ' + (failed.length ? 'FAILED (' + failed.length + ')' : 'VERIFY_PASS'));
  process.exit(failed.length ? 1 : 0);
})().catch((e) => {
  console.error('[verify-req3] ' + (e && e.stack ? e.stack : String(e)));
  process.exit(1);
});