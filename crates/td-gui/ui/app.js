/* TempDirector 前端逻辑。
 *
 * 设计约束（都是踩过的坑或明确的取舍）：
 *  - 不引 npm / 打包器：这是一张表格 + 几个弹窗，前端工具链只会带来维护负担。
 *    所以走 `withGlobalTauri`，用 window.__TAURI__.core.invoke 调命令。
 *  - CSP 是 `style-src 'self'`：**不写内联 style 属性、不注入 <style>**，
 *    需要动态样式时只走 CSSOM（el.style.x = y）。
 *  - 弹窗/详情面板都用事件委托（data-act + data-id），避免每次重渲染都重新绑定。
 *  - `?mock=1` 会挂上一套内存假数据（本地打开 index.html 即可看/调界面），
 *    用于无头浏览器截图核验；`&progress=1` 额外展示进度浮层。
 */
'use strict';

// ---------------------------------------------------------------------------
// 后端桥
// ---------------------------------------------------------------------------

const PARAMS = new URLSearchParams(location.search);
const MOCK = PARAMS.has('mock');

function tauriInvoke(name, args) {
  const g = window.__TAURI__;
  if (g && g.core && typeof g.core.invoke === 'function') return g.core.invoke(name, args || {});
  const internals = window.__TAURI_INTERNALS__;
  if (internals && typeof internals.invoke === 'function') return internals.invoke(name, args || {});
  return Promise.reject(new Error('未检测到 Tauri IPC 桥'));
}

function tauriListen(event, handler) {
  const g = window.__TAURI__;
  if (g && g.event && typeof g.event.listen === 'function') return g.event.listen(event, handler);
  return Promise.resolve(function () {});
}

const backend = MOCK ? createMockBackend() : {
  invoke: tauriInvoke,
  listen: tauriListen,
};

function invoke(name, args) {
  return backend.invoke(name, args).catch(function (err) {
    const msg = typeof err === 'string' ? err : (err && err.message) || String(err);
    throw new Error(msg);
  });
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

const state = {
  snap: null,
  picked: new Set(),
  focusId: null,
  filters: { q: '', category: '', onlyEnabled: false, onlyAttention: false },
  busy: false,
  drag: null,
};

const CATEGORY_LABEL = {
  browser: '浏览器', design: '设计', dev: '开发', system: '系统',
  media: '影音', office: '办公', game: '游戏', other: '其他',
};

function categoryLabel(c) { return CATEGORY_LABEL[c] || c; }

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

function $(id) { return document.getElementById(id); }

function esc(s) {
  return String(s === null || s === undefined ? '' : s).replace(/[&<>"']/g, function (c) {
    return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
  });
}

function humanBytes(n) {
  if (!n || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return i === 0 ? n + ' B' : v.toFixed(1) + ' ' + units[i];
}

function badgeClass(badge) {
  if (badge === '✅') return 'ok';
  if (badge === '⚠️') return 'warn';
  if (badge === '🚫') return 'foreign';
  return 'plain';
}

/** 徽标后缀文案。
 * 只有"确实存在链接"的状态才配说联接/符号链接 —— 真实目录显示「未链接」，
 * 否则会出现「❌ 联接」这种自相矛盾的显示。 */
function badgeSuffix(rule) {
  if (rule.badge === '✅' || rule.badge === '⚠️') {
    return rule.link_type === 'symlink_dir' ? '符号链接' : '联接';
  }
  if (rule.badge === '❌') return '未链接';
  if (rule.badge === '🚫') return '外来';
  return '未知';
}

function toast(title, message, kind) {
  const box = document.createElement('div');
  box.className = 'toast ' + (kind || '');
  const t = document.createElement('div');
  t.className = 'toast-title';
  t.textContent = title;
  const m = document.createElement('div');
  m.textContent = message || '';
  box.appendChild(t);
  if (message) box.appendChild(m);
  $('toasts').appendChild(box);
  setTimeout(function () { box.remove(); }, kind === 'error' ? 9000 : 4500);
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('已复制', text.length > 60 ? text.slice(0, 60) + '…' : text, 'ok');
  } catch (e) {
    toast('复制失败', '请手动选择文本复制', 'warn');
  }
}

// ---------------------------------------------------------------------------
// 数据加载
// ---------------------------------------------------------------------------

async function refresh(keepFocus) {
  try {
    state.snap = await invoke('ui_snapshot');
    const alive = state.snap.rules.some(function (r) { return r.id === state.focusId; });
    if (!keepFocus || !alive) state.focusId = state.snap.rules.length ? state.snap.rules[0].id : null;
    // 清掉已经不存在的勾选项
    const ids = new Set(state.snap.rules.map(function (r) { return r.id; }));
    state.picked.forEach(function (id) { if (!ids.has(id)) state.picked.delete(id); });
    render();
  } catch (e) {
    toast('读取状态失败', e.message, 'error');
  }
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

function render() {
  renderBanners();
  renderHeader();
  renderFilterOptions();
  renderRows();
  renderDetail();
}

function renderBanners() {
  const host = $('banners');
  host.textContent = '';
  (state.snap.banners || []).forEach(function (b) {
    const d = document.createElement('div');
    d.className = 'banner ' + b.level;
    const dot = document.createElement('span');
    dot.className = 'dot';
    const txt = document.createElement('span');
    txt.textContent = b.text;
    d.appendChild(dot);
    d.appendChild(txt);
    if (b.action === 'sweep') {
      const btn = document.createElement('button');
      btn.className = 'banner-action';
      btn.textContent = '一键清理';
      btn.addEventListener('click', openBatchCleanDialog);
      d.appendChild(btn);
    }
    host.appendChild(d);
  });
}

function renderHeader() {
  const roots = state.snap.roots || [];
  const ready = roots.filter(function (r) { return r.state === 'ready'; });
  let sub;
  if (!roots.length) {
    sub = '未配置目标根 —— 先到「设置」里添加 RAM 盘';
  } else {
    const first = ready[0] || roots[0];
    const mark = first.state === 'ready' ? '就绪' : '不可用（' + first.message + '）';
    sub = '目标根 ' + first.path + ' ' + mark;
    if (ready.length && first.message && first.state === 'ready') sub += ' · ' + first.message;
    if (roots.length > 1) sub += ' · 共 ' + roots.length + ' 个目标根';
  }
  $('root-summary').textContent = sub;

  const s = state.snap.stats;
  $('stat-summary').textContent =
    '共 ' + s.total + ' 条 · 已开启 ' + s.enabled +
    ' · ✅ ' + s.ok + ' ⚠️ ' + s.warn + ' ❌ ' + s.plain +
    (s.other ? ' ❔ ' + s.other : '');
}

function renderFilterOptions() {
  const sel = $('filter-category');
  if (sel.options.length === 0) {
    const all = document.createElement('option');
    all.value = '';
    all.textContent = '全部分类';
    sel.appendChild(all);
    (state.snap.categories || []).forEach(function (c) {
      const o = document.createElement('option');
      o.value = c;
      o.textContent = categoryLabel(c);
      sel.appendChild(o);
    });
  }
  sel.value = state.filters.category;
}

function filteredRules() {
  const f = state.filters;
  const q = f.q.trim().toLowerCase();
  return (state.snap.rules || []).filter(function (r) {
    if (f.category && r.category !== f.category) return false;
    if (f.onlyEnabled && !r.want_enabled) return false;
    if (f.onlyAttention && !r.needs_attention) return false;
    if (q) {
      const hay = (r.name + ' ' + r.source + ' ' + r.source_template + ' ' + r.target + ' ' + r.target_subpath).toLowerCase();
      if (hay.indexOf(q) < 0) return false;
    }
    return true;
  });
}

/** 让筛选区与表头两处「全选」复选框与当前勾选状态保持一致。 */
function syncSelectAll(rows) {
  const total = rows.length;
  let picked = 0;
  rows.forEach(function (r) { if (state.picked.has(r.id)) picked++; });
  const checked = total > 0 && picked === total;
  const indeterminate = picked > 0 && picked < total;
  [$('sel-all'), $('sel-all-head')].forEach(function (cb) {
    if (!cb) return;
    cb.checked = checked;
    cb.indeterminate = indeterminate;
  });
}

function renderRows() {
  const rows = filteredRules();
  const tbody = $('rows');
  tbody.textContent = '';

  $('filter-summary').textContent =
    '显示 ' + rows.length + ' / ' + (state.snap.rules || []).length + ' 条' +
    (state.picked.size ? ' · 已勾选 ' + state.picked.size + ' 条' : '');
  syncSelectAll(rows);

  const empty = $('empty');
  if (!rows.length) {
    empty.classList.remove('hidden');
    empty.textContent = state.snap.rules.length
      ? '没有符合筛选条件的规则'
      : '还没有任何规则 —— 点左上角「＋ 添加规则」开始';
    return;
  }
  empty.classList.add('hidden');

  const frag = document.createDocumentFragment();
  rows.forEach(function (r) {
    const tr = document.createElement('tr');
    tr.dataset.id = String(r.id);
    if (state.picked.has(r.id)) tr.className += ' picked';
    if (r.id === state.focusId) tr.className += ' selected';

    // 勾选框 + 拖拽把手
    const tdCheck = document.createElement('td');
    const handle = document.createElement('span');
    handle.className = 'drag-handle';
    handle.textContent = '⋮⋮';
    handle.title = '拖动调整顺序（同一分类内）';
    tdCheck.appendChild(handle);
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = state.picked.has(r.id);
    cb.dataset.act = 'pick';
    cb.dataset.id = String(r.id);
    tdCheck.appendChild(cb);
    tr.appendChild(tdCheck);

    // 状态徽标
    const tdBadge = document.createElement('td');
    const badge = document.createElement('span');
    badge.className = 'badge ' + badgeClass(r.badge);
    badge.textContent = (r.badge || '❔') + ' ' + badgeSuffix(r);
    badge.title = r.label;
    tdBadge.appendChild(badge);
    tr.appendChild(tdBadge);

    // 名称 + 需要处理的原因
    const tdName = document.createElement('td');
    const nm = document.createElement('div');
    nm.className = 'cell-name';
    nm.textContent = r.name;
    tdName.appendChild(nm);
    if (r.needs_attention) {
      const note = document.createElement('div');
      note.className = 'row-note err';
      note.textContent = r.label;
      tdName.appendChild(note);
    } else if (r.warnings && r.warnings.length) {
      const note = document.createElement('div');
      note.className = 'row-note';
      note.textContent = r.warnings[0];
      tdName.appendChild(note);
    }
    tdName.dataset.act = 'focus';
    tdName.dataset.id = String(r.id);
    tr.appendChild(tdName);

    // 分类
    const tdCat = document.createElement('td');
    const pill = document.createElement('span');
    pill.className = 'pill';
    pill.textContent = categoryLabel(r.category);
    tdCat.appendChild(pill);
    tr.appendChild(tdCat);

    // 源路径
    const tdSrc = document.createElement('td');
    const src = document.createElement('div');
    src.className = 'cell-path';
    src.textContent = r.source;
    src.title = r.source_template;
    tdSrc.appendChild(src);
    tr.appendChild(tdSrc);

    // 目标
    const tdTgt = document.createElement('td');
    const tgt = document.createElement('div');
    tgt.className = 'cell-path root';
    tgt.textContent = r.target;
    tdTgt.appendChild(tgt);
    tr.appendChild(tdTgt);

    // 旗标：期望开启 + 开机重建合并成一列
    // （两个布尔各占一列太浪费横向空间，完整含义在详情面板 chips 里也有一份）
    const tdFlags = document.createElement('td');
    const flags = document.createElement('div');
    flags.className = 'row-flags';
    const on = document.createElement('span');
    on.className = 'toggle ' + (r.want_enabled ? 'on' : 'off');
    on.textContent = r.want_enabled ? '●' : '○';
    on.title = r.want_enabled ? '期望开启（参与「应用选中」）' : '期望关闭';
    flags.appendChild(on);
    const bt = document.createElement('span');
    bt.className = 'toggle ' + (r.rebuild_on_boot ? 'on' : 'off');
    bt.textContent = r.rebuild_on_boot ? '⟳' : '·';
    bt.title = r.rebuild_on_boot ? '开机自动重建目标目录' : '不参与开机重建';
    flags.appendChild(bt);
    tdFlags.appendChild(flags);
    tr.appendChild(tdFlags);

    // 行内操作
    const tdAct = document.createElement('td');
    const wrap = document.createElement('div');
    wrap.className = 'row-actions';
    [
      { act: 'apply', label: '应用', primary: true, when: r.badge !== '✅' },
      { act: 'restore', label: '恢复', when: r.badge === '✅' || r.badge === '⚠️' },
      { act: 'relink', label: '重链', when: r.badge === '⚠️' || r.badge === '❌' },
      { act: 'clean', label: '清理…', when: true },
      { act: 'edit', label: '编辑', when: true },
    ].forEach(function (b) {
      if (!b.when) return;
      const btn = document.createElement('button');
      btn.className = 'mini' + (b.primary ? ' primary' : '');
      btn.textContent = b.label;
      btn.dataset.act = b.act;
      btn.dataset.id = String(r.id);
      wrap.appendChild(btn);
    });
    tdAct.appendChild(wrap);
    tr.appendChild(tdAct);

    frag.appendChild(tr);
  });
  tbody.appendChild(frag);
}

function renderDetail() {
  const host = $('detail');
  const rule = (state.snap.rules || []).find(function (r) { return r.id === state.focusId; });
  if (!rule) {
    // 只收起、不清空内容：滑出动画期间还要有东西可看，下次打开时再重建。
    host.classList.remove('open');
    return;
  }
  host.classList.add('open');
  host.textContent = '';

  const head = document.createElement('div');
  head.className = 'detail-head';
  const badge = document.createElement('span');
  badge.className = 'badge ' + badgeClass(rule.badge);
  badge.textContent = rule.badge;
  const title = document.createElement('span');
  title.className = 'detail-title';
  title.textContent = rule.name;
  const cat = document.createElement('span');
  cat.className = 'pill';
  cat.textContent = categoryLabel(rule.category);
  head.appendChild(badge);
  head.appendChild(title);
  head.appendChild(cat);
  const chips = document.createElement('span');
  chips.className = 'chips';
  if (rule.want_enabled) chips.appendChild(chip('期望开启'));
  if (rule.rebuild_on_boot) chips.appendChild(chip('开机重建'));
  if (rule.needs_elevation) chips.appendChild(chip('需要管理员权限'));
  if (rule.has_acl) chips.appendChild(chip('已存权限模板'));
  if (rule.last_files) chips.appendChild(chip(rule.last_files + ' 个文件 · ' + humanBytes(rule.last_bytes || 0)));
  head.appendChild(chips);
  // 关闭按钮必须最后 append：`.detail-close { margin-left: auto }` 靠"最后一个
  // 弹性项"把它顶到最右，若它后面还有 chips，chips 会被挤到它右边。
  const close = document.createElement('button');
  close.className = 'icon-btn detail-close';
  close.textContent = '✕';
  close.title = '关闭详情（Esc）';
  close.addEventListener('click', function () {
    state.focusId = null;
    renderDetail();
    renderRows();
  });
  head.appendChild(close);
  host.appendChild(head);

  const grid = document.createElement('div');
  grid.className = 'detail-grid';
  addRow(grid, '状态', rule.label, true);
  addRow(grid, '源路径模板', rule.source_template);
  addRow(grid, '展开后', rule.source);
  addRow(grid, '目标路径', rule.target);
  addRow(grid, '链接类型', rule.link_type === 'symlink_dir' ? '目录符号链接（需要特权）' : '目录联接 Junction（免提权）');
  if (rule.notes) addRow(grid, '备注', rule.notes, true);
  if (rule.excludes && rule.excludes.length) {
    addRow(grid, '清理排除', rule.excludes.join('；'), true);
  }
  if (rule.auto_clean_policy) {
    addRow(grid, '水位自动清理', describeAutoPolicy(rule.auto_clean_policy), true);
  }
  if (rule.unknown_vars && rule.unknown_vars.length) {
    addRow(grid, '未解析变量', rule.unknown_vars.join('、'), true);
  }
  (rule.warnings || []).forEach(function (w, i) {
    addRow(grid, i === 0 ? '警告' : '', w, true);
  });
  if (rule.applied_at) addRow(grid, '上次应用', rule.applied_at, true);
  host.appendChild(grid);

  const actions = document.createElement('div');
  actions.className = 'detail-actions';
  host.appendChild(actions);
  actions.appendChild(mkBtn('应用（搬运 + 建链）', 'apply', rule.id, true));
  if (rule.needs_elevation && rule.badge !== '✅') {
    actions.appendChild(mkBtn('以管理员身份应用', 'apply-elevated', rule.id));
  }
  actions.appendChild(mkBtn('恢复（拆链 + 搬回）', 'restore', rule.id));
  actions.appendChild(mkBtn('仅重建链接', 'relink', rule.id));
  actions.appendChild(mkBtn('检测占用', 'locks', rule.id));
  actions.appendChild(mkBtn('清理…', 'clean', rule.id));
  actions.appendChild(mkBtn('复制源路径', 'copy-source', rule.id));
  actions.appendChild(mkBtn('打开位置', 'reveal', rule.id));
  actions.appendChild(mkBtn('编辑', 'edit', rule.id));
}

function describeAutoPolicy(p) {
  if (p === 'empty') return '水位到严重线时清空全部';
  const m = /older:(\d+)/.exec(p);
  if (m) {
    const secs = Number(m[1]);
    if (secs % 86400 === 0) return '水位到严重线时清空早于 ' + (secs / 86400) + ' 天修改的';
    if (secs % 3600 === 0) return '水位到严重线时清空早于 ' + (secs / 3600) + ' 小时修改的';
    return '水位到严重线时清空早于 ' + secs + ' 秒修改的';
  }
  return p;
}

function chip(text) {
  const s = document.createElement('span');
  s.className = 'pill';
  s.textContent = text;
  return s;
}

function mkBtn(label, act, id, primary) {
  const b = document.createElement('button');
  b.className = 'btn' + (primary ? ' primary' : ' ghost');
  b.textContent = label;
  b.dataset.act = act;
  b.dataset.id = String(id);
  return b;
}

function addRow(grid, key, value, plain) {
  const k = document.createElement('div');
  k.className = 'detail-key';
  k.textContent = key;
  const v = document.createElement('div');
  v.className = 'detail-val' + (plain ? ' plain' : '');
  v.textContent = value;
  grid.appendChild(k);
  grid.appendChild(v);
}

// ---------------------------------------------------------------------------
// 弹窗
// ---------------------------------------------------------------------------

/** 需要"等用户选一个"的弹窗（✕ / Esc 也会 resolve 成 cancel，避免 Promise 悬挂）。 */
let modalResolver = null;

function openModal(opts) {
  $('modal-title').textContent = opts.title;
  const body = $('modal-body');
  body.textContent = '';
  // 只接受 DOM 节点：**不用 innerHTML**。规则名/路径都来自用户数据，
  // 一旦哪天有人图省事拼字符串，innerHTML 就是一个注入点。
  if (opts.body) body.appendChild(opts.body);
  $('modal-card').className = 'modal-card' + (opts.wide ? ' wide' : '');

  const foot = $('modal-foot');
  foot.textContent = '';
  (opts.actions || []).forEach(function (a) {
    const b = document.createElement('button');
    b.className = 'btn ' + (a.kind || 'ghost');
    b.textContent = a.label;
    if (a.disabled) b.disabled = true;
    b.addEventListener('click', function () { a.onClick(); });
    foot.appendChild(b);
  });
  $('modal').classList.remove('hidden');
}

function closeModal() {
  $('modal').classList.add('hidden');
  $('modal-body').textContent = '';
  const r = modalResolver;
  modalResolver = null;
  if (r) r('cancel');
}

function askChoice(opts) {
  return new Promise(function (resolve) {
    let settled = false;
    function finish(value) {
      if (settled) return;
      settled = true;
      modalResolver = null;
      $('modal').classList.add('hidden');
      $('modal-body').textContent = '';
      resolve(value);
    }
    modalResolver = finish;

    const body = document.createElement('div');
    (opts.lines || []).forEach(function (t) {
      const p = document.createElement('div');
      p.className = t.kind ? 'hint ' + t.kind : 'field';
      p.textContent = t.text;
      body.appendChild(p);
    });
    openModal({
      title: opts.title,
      body: body,
      actions: (opts.actions || []).map(function (a) {
        return {
          label: a.label,
          kind: a.kind,
          onClick: function () { finish(a.value); },
        };
      }),
    });
  });
}

// ---------------------------------------------------------------------------
// 进度
// ---------------------------------------------------------------------------

function showProgress(title) {
  $('progress-title').textContent = title;
  $('progress-fill').style.width = '0%';
  $('progress-detail').textContent = '';
  $('progress').classList.remove('hidden');
}

function setProgress(pct, detail) {
  $('progress-fill').style.width = Math.max(0, Math.min(100, pct)) + '%';
  $('progress-detail').textContent = detail || '';
}

function hideProgress() { $('progress').classList.add('hidden'); }

// ---------------------------------------------------------------------------
// 动作
// ---------------------------------------------------------------------------

function pickedIds() { return Array.from(state.picked); }

function requirePicked(what) {
  if (!state.picked.size) {
    toast('没有选中规则', '先用左侧复选框勾选要' + what + '的规则', 'warn');
    return false;
  }
  return true;
}

const OPTS_NORMAL = { dry_run: false, allow_merge: false, skip_lock_check: false, sha256: false };

/** 走提权 worker（会弹一次 UAC）。 */
async function runElevated(kind, ids) {
  if (!ids.length) return;
  state.busy = true;
  showProgress('正在请求管理员权限…请在 UAC 弹窗上点「是」');
  try {
    const out = await invoke('run_elevated_job', { kind: kind, ids: ids, opts: OPTS_NORMAL });
    hideProgress();
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    out.forEach(function (o) {
      if (!o.ok) toast('失败：#' + o.rule_id + ' ' + o.name, o.message, 'error');
    });
    toast(
      kind === 'apply' ? '提权应用完成' : '提权恢复完成',
      ok + ' 条成功' + (failed ? '，' + failed + ' 条失败' : ''),
      failed ? 'warn' : 'ok'
    );
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast('提权执行失败', e.message, 'error');
  } finally {
    state.busy = false;
  }
}

/** 选中项里有没有"需要管理员权限"的规则。 */
function rulesNeedingElevation(ids) {
  return ids.filter(function (id) {
    const r = state.snap.rules.find(function (x) { return x.id === id; });
    return r && r.needs_elevation;
  });
}

async function runApply(ids, dryRun) {
  if (!ids.length) return;

  if (!dryRun) {
    const needElev = rulesNeedingElevation(ids);
    if (needElev.length) {
      const others = ids.length - needElev.length;
      const choice = await askChoice({
        title: '有规则需要管理员权限',
        lines: [
          { text: '选中的 ' + ids.length + ' 条里有 ' + needElev.length + ' 条位于受保护位置（Program Files / ProgramData / Windows 下），搬运与建链需要管理员权限。' },
          {
            text: '选择「以管理员身份执行」会弹出一次 UAC 授权窗口；授权后由一个一次性进程完成这次操作（它只认规则 id，不接受任意路径）。' +
              (others ? '其余 ' + others + ' 条会一起在提权进程里处理。' : ''),
            kind: '',
          },
        ],
        actions: [
          { label: '以管理员身份执行', kind: 'primary', value: 'elevate' },
          { label: '只处理不需要权限的', value: 'partial' },
          { label: '取消', value: 'cancel' },
        ],
      });
      if (choice !== 'elevate' && choice !== 'partial') return;
      if (choice === 'elevate') return runElevated('apply', ids);
      if (choice === 'partial') {
        const rest = ids.filter(function (id) { return needElev.indexOf(id) < 0; });
        if (!rest.length) {
          toast('没有可处理的规则', '选中的都需要管理员权限', 'warn');
          return;
        }
        ids = rest;
      }
    }
  }

  state.busy = true;
  showProgress((dryRun ? '预演：' : '') + '正在处理 ' + ids.length + ' 条规则…');
  try {
    const out = await invoke('apply_rules', { ids: ids, opts: { dry_run: !!dryRun, allow_merge: false, skip_lock_check: false, sha256: false } });
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    hideProgress();
    out.forEach(function (o) {
      if (!o.ok) toast('失败：#' + o.rule_id + ' ' + o.name, o.message, 'error');
    });
    toast(dryRun ? '预演完成' : '执行完成', ok + ' 条成功' + (failed ? '，' + failed + ' 条失败' : ''), failed ? 'warn' : 'ok');
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast('执行失败', e.message, 'error');
  } finally {
    state.busy = false;
  }
}

async function runRestore(ids) {
  if (!ids.length) return;
  state.busy = true;
  showProgress('正在恢复 ' + ids.length + ' 条规则…');
  try {
    const out = await invoke('restore_rules', { ids: ids, opts: { dry_run: false, allow_merge: true, skip_lock_check: true, sha256: false } });
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    hideProgress();
    out.forEach(function (o) {
      if (!o.ok) toast('失败：#' + o.rule_id + ' ' + o.name, o.message, 'error');
    });
    toast('恢复完成', ok + ' 条成功' + (failed ? '，' + failed + ' 条失败' : ''), failed ? 'warn' : 'ok');
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast('恢复失败', e.message, 'error');
  } finally {
    state.busy = false;
  }
}

async function doRelink(id) {
  try {
    const o = await invoke('relink_rule', { id: id });
    toast(o.ok ? '已重建链接' : '失败', o.message, o.ok ? 'ok' : 'error');
    await refresh(true);
  } catch (e) {
    toast('重建失败', e.message, 'error');
  }
}

async function doLocks(id) {
  toast('正在检测占用…', '扫描源目录中的文件句柄', '');
  try {
    const rep = await invoke('check_locks', { id: id });
    const body = document.createElement('div');
    if (rep.locked_files.length === 0 && rep.denied_files.length === 0) {
      const p = document.createElement('div');
      p.className = 'field';
      const hint = document.createElement('div');
      hint.className = 'hint ok';
      hint.textContent = '✅ 未发现占用（已扫描 ' + rep.scanned + ' 个文件，' + rep.elapsed_ms + 'ms），可以安全搬运。';
      p.appendChild(hint);
      body.appendChild(p);
    } else {
      const p = document.createElement('div');
      p.className = 'field';
      const hint = document.createElement('div');
      hint.className = 'hint warn';
      hint.textContent = rep.locked_files.length + ' 个文件被占用' + (rep.truncated ? '（已截断）' : '') +
        (rep.denied_files.length ? '，' + rep.denied_files.length + ' 个文件无权限访问' : '');
      p.appendChild(hint);
      body.appendChild(p);

      if (rep.blockers.length) {
        const bl = document.createElement('div');
        bl.className = 'field';
        const lbl = document.createElement('label');
        lbl.textContent = '占用者（关闭它们后重试）';
        bl.appendChild(lbl);
        rep.blockers.forEach(function (b) {
          const line = document.createElement('div');
          line.className = 'hint err';
          line.textContent = '⛔ ' + b.app + '（PID ' + b.pid + '）' + (b.is_service ? ' [服务]' : '');
          bl.appendChild(line);
        });
        body.appendChild(bl);
      }
      const files = document.createElement('div');
      files.className = 'field';
      const fl = document.createElement('label');
      fl.textContent = '被占用的文件（最多 10 个）';
      files.appendChild(fl);
      rep.locked_files.slice(0, 10).forEach(function (f) {
        const line = document.createElement('div');
        line.className = 'detail-val';
        line.textContent = f;
        files.appendChild(line);
      });
      body.appendChild(files);
    }
    openModal({ title: '占用检测结果', body: body, actions: [{ label: '关闭', onClick: closeModal }] });
    await refresh(true);
  } catch (e) {
    toast('检测失败', e.message, 'error');
  }
}

async function doBoot() {
  state.busy = true;
  showProgress('正在执行开机重建…');
  try {
    const r = await invoke('boot_rebuild');
    hideProgress();
    const lines = [r.roots.map(function (x) {
      return (x.ready ? '✅ ' : '❌ ') + x.path + (x.ready ? '（等待 ' + x.waited_ms + 'ms）' : '：' + (x.error || '未就绪'));
    }).join('\n'), r.one_line()].join('\n\n');
    toast('开机重建完成', r.one_line(), r.has_problem() ? 'warn' : 'ok');
    if (r.warnings.length || r.failed.length) {
      const body = document.createElement('div');
      r.warnings.concat(r.failed.map(function (f) { return '#' + f[0] + ' ' + f[1]; })).forEach(function (w) {
        const d = document.createElement('div');
        d.className = 'hint warn';
        d.textContent = '⚠ ' + w;
        body.appendChild(d);
      });
      openModal({ title: '开机重建：需要留意', body: body, actions: [{ label: '知道了', kind: 'primary', onClick: closeModal }] });
    }
    void lines;
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast('开机重建失败', e.message, 'error');
  } finally {
    state.busy = false;
  }
}

async function openOps() {
  const body = document.createElement('div');
  // 效益统计条：三个数字与下面的操作流水同源（operations 表），
  // 放在运维弹窗里而不是首页 —— 它是"回顾"信息，不是"当前要处理"的信息。
  const strip = document.createElement('div');
  strip.className = 'savings-strip';
  strip.textContent = '效益统计读取中…';
  body.appendChild(strip);
  loadSavings(strip);
  const tabs = document.createElement('div');
  tabs.className = 'log-tabs';
  body.appendChild(tabs);
  const list = document.createElement('div');
  list.className = 'log-list';
  body.appendChild(list);

  let mode = 'events';
  const btnEvents = document.createElement('button');
  btnEvents.className = 'btn active';
  btnEvents.textContent = '事件';
  const btnOps = document.createElement('button');
  btnOps.className = 'btn';
  btnOps.textContent = '操作流水';
  tabs.appendChild(btnEvents);
  tabs.appendChild(btnOps);

  async function load() {
    btnEvents.className = 'btn' + (mode === 'events' ? ' active' : '');
    btnOps.className = 'btn' + (mode === 'ops' ? ' active' : '');
    list.textContent = '';
    try {
      if (mode === 'events') {
        const evs = await invoke('recent_events', { limit: 100 });
        if (!evs.length) { list.textContent = '（暂无事件）'; return; }
        evs.forEach(function (e) {
          list.appendChild(logRow(e.at, e.kind, e.message, e.level));
        });
      } else {
        const ops = await invoke('recent_ops', { limit: 100 });
        if (!ops.length) { list.textContent = '（暂无操作流水）'; return; }
        ops.forEach(function (o) {
          const msg = '#' + o.id + ' rule=' + (o.rule_id === null ? '-' : o.rule_id) +
            ' files=' + (o.file_count || 0) + ' ' + (o.source_path || '') +
            (o.error ? '  ERR: ' + o.error : '');
          list.appendChild(logRow(o.started_at, o.kind + '/' + o.phase, msg, o.phase === 'done' ? 'info' : 'warn'));
        });
      }
    } catch (e) {
      list.textContent = '读取失败：' + e.message;
    }
  }
  btnEvents.addEventListener('click', function () { mode = 'events'; load(); });
  btnOps.addEventListener('click', function () { mode = 'ops'; load(); });
  await load();
  openModal({ title: '运维日志（事件 / 操作流水）', body: body, wide: true, actions: [{ label: '关闭', onClick: closeModal }] });
}

/** 效益统计三卡：当前重定向 / 累计搬运 / 累计清理。
 * 只报可解释的数字，不做"节省 SSD 寿命"外推 —— 后端同此约定（usecase/stats.rs）。 */
async function loadSavings(strip) {
  try {
    const v = await invoke('savings');
    strip.textContent = '';
    strip.appendChild(savingCard('当前重定向到 RAM 盘', v.redirected_text,
      v.redirected_rules + ' 条规则生效 · ' + v.redirected_files + ' 个文件不写 SSD'));
    strip.appendChild(savingCard('累计搬运', v.moved_text,
      v.moved_files + ' 个文件从 SSD 搬入'));
    strip.appendChild(savingCard('累计清理释放', v.cleaned_text,
      v.cleaned_files + ' 个文件已删除'));
  } catch (e) {
    // 统计只是附属信息，拿不到就撤掉这块，别挡住下面的日志。
    strip.remove();
  }
}

function savingCard(title, value, sub) {
  const card = document.createElement('div');
  card.className = 'saving-card';
  const t = document.createElement('div');
  t.className = 'saving-title';
  t.textContent = title;
  const v = document.createElement('div');
  v.className = 'saving-value';
  v.textContent = value;
  const s = document.createElement('div');
  s.className = 'saving-sub';
  s.textContent = sub;
  card.appendChild(t);
  card.appendChild(v);
  card.appendChild(s);
  return card;
}

function logRow(time, kind, msg, level) {
  const row = document.createElement('div');
  row.className = 'log-item ' + (level === 'error' ? 'error' : level === 'warn' ? 'warn' : '');
  const t = document.createElement('div');
  t.className = 'log-time';
  t.textContent = time;
  const k = document.createElement('div');
  k.className = 'log-kind';
  k.textContent = kind;
  const m = document.createElement('div');
  m.className = 'log-msg';
  m.textContent = msg;
  row.appendChild(t);
  row.appendChild(k);
  row.appendChild(m);
  return row;
}

async function openSettings() {
  const snap = state.snap;
  const body = document.createElement('div');

  const info = document.createElement('div');
  info.className = 'field';
  [
    ['版本', snap.version + '（schema v' + snap.schema_version + '）'],
    ['数据目录', snap.data_dir],
    ['数据库', snap.db_path],
  ].forEach(function (kv) {
    const d = document.createElement('div');
    d.className = 'hint';
    d.textContent = kv[0] + '：' + kv[1];
    info.appendChild(d);
  });
  body.appendChild(info);

  // ---- 开机自启（M3）----
  await appendAutostartSection(body, snap);

  // ---- 关闭窗口行为 ----
  // 托盘常驻是水位巡检 / 预授权自动清理"无人值守"的前提：窗口关掉后前端
  // 定时器不再可靠，只有进程活着 + 托盘在，后台能力才继续生效。
  const closeField = document.createElement('div');
  closeField.className = 'field';
  const closeLabel = document.createElement('label');
  closeLabel.textContent = '关闭窗口时';
  closeField.appendChild(closeLabel);
  let closeToTray = true;
  try { closeToTray = (await invoke('ui_settings', {})).close_to_tray; } catch (e) { closeToTray = true; }
  const closeCheck = document.createElement('input');
  closeCheck.type = 'checkbox';
  closeCheck.checked = closeToTray;
  const closeInline = document.createElement('label');
  closeInline.className = 'inline-check';
  closeInline.appendChild(closeCheck);
  const closeSpan = document.createElement('span');
  closeSpan.textContent = '最小化到托盘（继续后台巡检水位与自动清理）';
  closeInline.appendChild(closeSpan);
  closeCheck.addEventListener('change', async function () {
    try {
      await invoke('set_close_to_tray', { enabled: closeCheck.checked });
      toast('已保存', closeCheck.checked ? '关闭窗口时最小化到托盘' : '关闭窗口时直接退出', 'ok');
    } catch (e) {
      toast('保存失败', e.message, 'error');
    }
  });
  closeField.appendChild(closeInline);
  const closeHint = document.createElement('div');
  closeHint.className = 'hint';
  closeHint.textContent = '退出请走托盘的「退出」；取消勾选则点 ✕ 直接退出程序（后台巡检与自动清理将停止）。';
  closeField.appendChild(closeHint);
  body.appendChild(closeField);

  const rootField = document.createElement('div');
  rootField.className = 'field';
  const rootLabel = document.createElement('label');
  rootLabel.textContent = '目标根（RAM 盘）';
  rootField.appendChild(rootLabel);
  const rootList = document.createElement('div');
  rootList.className = 'log-list';
  (snap.roots || []).forEach(function (r) {
    const row = document.createElement('div');
    row.className = 'log-item';
    const a = document.createElement('div');
    a.className = 'log-kind';
    a.textContent = '#' + r.id + ' ' + r.label;
    const b = document.createElement('div');
    b.className = 'log-msg';
    b.textContent = r.path + ' · ' + (r.state === 'ready' ? '就绪' : r.message) +
      ' · 等待上限 ' + r.wait_timeout_ms + 'ms' + (r.is_ramdisk ? ' · RAM 盘' : '');
    const del = document.createElement('button');
    del.className = 'mini';
    del.textContent = '删除';
    del.addEventListener('click', async function () {
      try {
        await invoke('delete_root', { id: r.id });
        closeModal();
        toast('已删除目标根', '#' + r.id + ' ' + r.path, 'ok');
        await refresh(true);
      } catch (e) {
        toast('删除失败', e.message, 'error');
      }
    });
    row.appendChild(a);
    row.appendChild(b);
    row.appendChild(del);
    rootList.appendChild(row);
  });
  if (!snap.roots || !snap.roots.length) rootList.textContent = '（尚未配置目标根，先在下面添加）';
  rootField.appendChild(rootList);
  body.appendChild(rootField);

  const addField = document.createElement('div');
  addField.className = 'field';
  const addLabel = document.createElement('label');
  addLabel.textContent = '添加目标根';
  addField.appendChild(addLabel);
  const row = document.createElement('div');
  row.className = 'field-row';
  const nameInput = mkInput('名称，如 RAMDisk 4G');
  const pathInput = mkInput('路径，如 R: 或 R:\\Redirect');
  const pick = mkPickButton('浏览…', async function () {
    const p = await invoke('pick_folder', { title: '选择 RAM 盘的根目录（或直接手填 R:）', defaultPath: pathInput.value });
    if (p) pathInput.value = p;
  });
  const addBtn = document.createElement('button');
  addBtn.className = 'btn primary';
  addBtn.textContent = '添加';
  addBtn.addEventListener('click', async function () {
    try {
      await invoke('add_root', {
        label: nameInput.value || 'RAMDisk',
        path: pathInput.value,
        isRamdisk: true,
        waitTimeoutMs: 90000,
      });
      closeModal();
      toast('已添加目标根', pathInput.value, 'ok');
      await refresh(true);
    } catch (e) {
      toast('添加失败', e.message, 'error');
    }
  });
  // 只有"路径"输入框拉满，两个按钮各自紧凑 —— 否则「浏览/添加」会被撑成一大条。
  nameInput.style.flex = '0 0 180px';
  pathInput.style.flex = '1 1 auto';
  pick.style.flex = '0 0 auto';
  addBtn.style.flex = '0 0 auto';
  row.appendChild(nameInput);
  row.appendChild(pathInput);
  row.appendChild(pick);
  row.appendChild(addBtn);
  addField.appendChild(row);
  const addHint = document.createElement('div');
  addHint.className = 'hint';
  addHint.textContent = '提示：直接填「R:」即可。目标盘建议格式化为 NTFS（Junction 需要 NTFS 才可靠）。';
  addField.appendChild(addHint);
  body.appendChild(addField);

  openModal({
    title: '设置',
    body: body,
    wide: true,
    actions: [
      { label: '导出配置', onClick: function () { closeModal(); doExport(); } },
      { label: '导入配置', onClick: function () { closeModal(); doImport(); } },
      { label: '关闭', kind: 'primary', onClick: closeModal },
    ],
  });
}

function mkInput(placeholder) {
  const i = document.createElement('input');
  i.className = 'input';
  i.placeholder = placeholder;
  return i;
}

function mkSmallBtn(label, onClick, kind) {
  const b = document.createElement('button');
  b.className = 'btn' + (kind ? ' ' + kind : ' ghost');
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

/** 设置页里的「开机自启」区块。 */
async function appendAutostartSection(body, snap) {
  let auto = null;
  try {
    auto = await invoke('autostart_status');
  } catch (e) {
    auto = null;
  }

  const field = document.createElement('div');
  field.className = 'field';
  const label = document.createElement('label');
  label.textContent = '开机自启（登录时自动重建目标目录）';
  field.appendChild(label);

  const box = document.createElement('div');
  box.className = 'log-list';
  const row = document.createElement('div');
  row.className = 'log-item';
  const kind = document.createElement('div');
  kind.className = 'log-kind';
  kind.textContent = !auto ? '❔ 未知' : auto.registered ? '✅ 已启用' : '❌ 未启用';
  const msg = document.createElement('div');
  msg.className = 'log-msg';
  msg.textContent = auto ? auto.message : '读取任务状态失败';
  row.appendChild(kind);
  row.appendChild(msg);
  box.appendChild(row);
  field.appendChild(box);

  const actions = document.createElement('div');
  actions.className = 'detail-actions';
  actions.appendChild(
    mkSmallBtn('启用', async function () {
      try {
        const cmd = await invoke('autostart_enable');
        toast('已启用开机自启', cmd, 'ok');
        closeModal();
        await openSettings();
      } catch (e) {
        toast('启用失败', e.message, 'error');
      }
    }, 'primary')
  );
  actions.appendChild(
    mkSmallBtn('停用', async function () {
      try {
        const msg2 = await invoke('autostart_disable');
        toast('已停用', msg2, 'ok');
        closeModal();
        await openSettings();
      } catch (e) {
        toast('停用失败', e.message, 'error');
      }
    })
  );
  actions.appendChild(
    mkSmallBtn('立即运行一次', async function () {
      try {
        const msg2 = await invoke('autostart_run_now');
        toast('已触发', msg2, 'ok');
        setTimeout(function () { refresh(true); }, 3000);
      } catch (e) {
        toast('触发失败', e.message, 'error');
      }
    })
  );
  field.appendChild(actions);

  const hint = document.createElement('div');
  hint.className = 'hint';
  if (auto) {
    const parts = [
      '注册的是用户级登录任务（' + auto.task_name + '），不需要管理员权限，只在你登录时运行。',
      '命令：' + (auto.command || auto.recommended_exe + ' boot-rebuild'),
      '当前进程身份：' + (auto.elevated ? '管理员' : '普通用户') + '。',
    ];
    if (auto.extra_args) {
      parts.push('已把数据目录写进任务参数（便携模式）：' + auto.extra_args);
    }
    hint.textContent = parts.join(' ');
  } else {
    hint.textContent = '';
  }
  field.appendChild(hint);
  body.appendChild(field);
}

function mkPickButton(label, onClick) {
  const b = document.createElement('button');
  b.className = 'btn';
  b.textContent = label;
  b.addEventListener('click', onClick);
  return b;
}

async function doExport() {
  try {
    const path = await invoke('pick_save_file', { title: '导出规则配置', defaultName: 'tempdirector-config.json' });
    if (!path) return;
    const r = await invoke('export_config', { path: path });
    toast('已导出', r[0] + ' 个目标根、' + r[1] + ' 条规则 → ' + path, 'ok');
  } catch (e) {
    toast('导出失败', e.message, 'error');
  }
}

async function doImport() {
  try {
    const path = await invoke('pick_open_file', { title: '导入规则配置' });
    if (!path) return;
    const rep = await invoke('import_config', { path: path, ramRoot: null, dryRun: false });
    const msg = '新建目标根 ' + rep.roots_created + '，规则 ' + rep.rules_created +
      '，跳过 ' + rep.rules_skipped + (rep.errors.length ? '，问题 ' + rep.errors.length + ' 条' : '');
    toast('导入完成', msg, rep.errors.length ? 'warn' : 'ok');
    if (rep.errors.length) {
      const body = document.createElement('div');
      rep.errors.forEach(function (e) {
        const d = document.createElement('div');
        d.className = 'hint warn';
        d.textContent = '⚠ ' + e;
        body.appendChild(d);
      });
      openModal({ title: '导入时遇到的问题', body: body, actions: [{ label: '知道了', kind: 'primary', onClick: closeModal }] });
    }
    await refresh(true);
  } catch (e) {
    toast('导入失败', e.message, 'error');
  }
}

// ---------------------------------------------------------------------------
// 添加 / 编辑规则
// ---------------------------------------------------------------------------

async function openRuleForm(id) {
  const snap = state.snap;
  if (!snap.roots.length) {
    toast('先配置目标根', '还没有目标根，先到「设置」里添加 RAM 盘', 'warn');
    return;
  }
  const rule = id ? snap.rules.find(function (r) { return r.id === id; }) : null;

  const body = document.createElement('div');
  const mk = function (labelText, node, hintText, hintClass) {
    const f = document.createElement('div');
    f.className = 'field';
    const l = document.createElement('label');
    l.textContent = labelText;
    f.appendChild(l);
    f.appendChild(node);
    const h = document.createElement('div');
    h.className = 'hint ' + (hintClass || '');
    h.textContent = hintText || '';
    f.appendChild(h);
    return { field: f, hint: h };
  };

  // 常用预设（仅新增时显示；后端已按本机实际存在过滤，没装的软件不会出现）
  let nameInput = null;
  let catSelect = null;
  let srcInput = null;
  let subInput = null;
  if (!rule) {
    let presets = [];
    try { presets = await invoke('preset_rules', {}); } catch (e) { presets = []; }
    if (presets.length) {
      const presetSelect = document.createElement('select');
      presetSelect.className = 'select';
      const placeholder = document.createElement('option');
      placeholder.value = '';
      placeholder.textContent = '从常用预设选择（已按本机安装的软件过滤）…';
      presetSelect.appendChild(placeholder);
      const groups = {};
      presets.forEach(function (p) {
        (groups[p.category] = groups[p.category] || []).push(p);
      });
      Object.keys(groups).forEach(function (cat) {
        const og = document.createElement('optgroup');
        og.label = categoryLabel(cat);
        groups[cat].forEach(function (p) {
          const o = document.createElement('option');
          o.value = p.source_template;
          o.textContent = p.name;
          og.appendChild(o);
        });
        presetSelect.appendChild(og);
      });
      const presetWrap = mk('常用预设（可选）', presetSelect,
        '选中后自动填充名称、分类、源路径与目标子目录，可再修改');
      const presetHint = document.createElement('div');
      presetHint.className = 'hint';
      presetWrap.field.appendChild(presetHint);
      presetSelect.addEventListener('change', function () {
        const p = presets.find(function (x) { return x.source_template === presetSelect.value; });
        if (!p) { presetHint.textContent = ''; presetHint.className = 'hint'; return; }
        if (nameInput) nameInput.value = p.name;
        if (catSelect) catSelect.value = p.category;
        if (srcInput) { srcInput.value = p.source_template; checkSource(); }
        if (subInput) { subInput.value = p.target_subpath; checkTarget(); }
        presetHint.textContent = (p.note ? '⚠ ' + p.note + ' · ' : '') + p.source;
        presetHint.className = p.note ? 'hint warn' : 'hint';
      });
      body.appendChild(presetWrap.field);
    }
  }

  // 名称
  nameInput = mkInput('例如：Chrome 缓存');
  nameInput.value = rule ? rule.name : '';
  body.appendChild(mk('规则名称', nameInput).field);

  // 分类
  catSelect = document.createElement('select');
  catSelect.className = 'select';
  snap.categories.forEach(function (c) {
    const o = document.createElement('option');
    o.value = c;
    o.textContent = categoryLabel(c);
    catSelect.appendChild(o);
  });
  catSelect.value = rule ? rule.category : 'other';
  body.appendChild(mk('分类', catSelect).field);

  // 源路径 + 浏览 + 实时校验
  const srcField = document.createElement('div');
  srcField.className = 'field';
  const srcLabel = document.createElement('label');
  srcLabel.textContent = '源路径（软件原生缓存目录，支持 %VAR% 模板）';
  srcField.appendChild(srcLabel);
  const srcRow = document.createElement('div');
  srcRow.className = 'field-row';
  srcInput = mkInput('%LOCALAPPDATA%\\Google\\Chrome\\User Data\\Default\\Cache');
  srcInput.value = rule ? rule.source_template : '';
  srcInput.style.flex = '1';
  const srcPick = mkPickButton('选择目录…', async function () {
    const p = await invoke('pick_folder', { title: '选择软件的缓存目录', defaultPath: srcInput.value });
    if (p) { srcInput.value = p; checkSource(); }
  });
  srcRow.appendChild(srcInput);
  srcRow.appendChild(srcPick);
  srcField.appendChild(srcRow);
  const srcHint = document.createElement('div');
  srcHint.className = 'hint';
  srcField.appendChild(srcHint);
  body.appendChild(srcField);

  // 目标根 + 子目录
  const rootSelect = document.createElement('select');
  rootSelect.className = 'select';
  snap.roots.forEach(function (r) {
    const o = document.createElement('option');
    o.value = String(r.id);
    o.textContent = '#' + r.id + ' ' + r.label + '（' + r.path + '）' + (r.state === 'ready' ? '' : ' —— 当前不可用');
    rootSelect.appendChild(o);
  });
  rootSelect.value = String(rule ? rule.target_root_id : snap.roots[0].id);

  subInput = mkInput('例如：browsers\\chrome-cache');
  subInput.value = rule ? rule.target_subpath : '';

  const targetRow = document.createElement('div');
  targetRow.className = 'field-row';
  const rootFieldWrap = document.createElement('div');
  rootFieldWrap.className = 'field';
  const rootL = document.createElement('label');
  rootL.textContent = '目标根（RAM 盘）';
  rootFieldWrap.appendChild(rootL);
  rootFieldWrap.appendChild(rootSelect);
  const subFieldWrap = document.createElement('div');
  subFieldWrap.className = 'field';
  const subL = document.createElement('label');
  subL.textContent = '目标子目录（RAM 盘下的相对路径）';
  subFieldWrap.appendChild(subL);
  subFieldWrap.appendChild(subInput);
  targetRow.appendChild(rootFieldWrap);
  targetRow.appendChild(subFieldWrap);
  body.appendChild(targetRow);
  const tgtHint = document.createElement('div');
  tgtHint.className = 'hint';
  body.appendChild(tgtHint);

  // 链接类型
  const linkSelect = document.createElement('select');
  linkSelect.className = 'select';
  [['junction', '目录联接 Junction（推荐，不需要管理员权限）'],
   ['symlink_dir', '目录符号链接（需要管理员权限或开发者模式，仅在目标是网络路径时需要）']].forEach(function (p) {
    const o = document.createElement('option');
    o.value = p[0];
    o.textContent = p[1];
    linkSelect.appendChild(o);
  });
  linkSelect.value = rule ? rule.link_type : 'junction';
  body.appendChild(mk('链接类型', linkSelect).field);

  // 开关
  const onCheck = document.createElement('input');
  onCheck.type = 'checkbox';
  onCheck.checked = rule ? rule.want_enabled : true;
  const bootCheck = document.createElement('input');
  bootCheck.type = 'checkbox';
  bootCheck.checked = rule ? rule.rebuild_on_boot : true;
  const flags = document.createElement('div');
  [['期望开启（会参与「应用选中」）', onCheck], ['开机自动重建目标目录', bootCheck]].forEach(function (p) {
    const l = document.createElement('label');
    l.className = 'inline-check';
    l.appendChild(p[1]);
    const s = document.createElement('span');
    s.textContent = p[0];
    l.appendChild(s);
    flags.appendChild(l);
  });
  body.appendChild(flags);

  // 备注
  const notes = document.createElement('input');
  notes.className = 'input';
  notes.placeholder = '备注，例如：应用前请先关闭 Chrome';
  notes.value = rule && rule.notes ? rule.notes : '';
  body.appendChild(mk('备注', notes).field);

  // 清理排除：一行一条，只影响「清理」，不影响应用/恢复的搬运
  const excludes = document.createElement('textarea');
  excludes.className = 'input';
  excludes.rows = 2;
  excludes.placeholder = '一行一条，可留空。\n例：sessions（任意层级）\n例：logs\\old（从根锚定）\n例：*.log（通配符）';
  excludes.value = rule && rule.excludes ? rule.excludes.join('\n') : '';
  const exWrap = mk('清理排除（可选）', excludes);
  const exHint = document.createElement('div');
  exHint.className = 'hint';
  exHint.textContent = '清理时跳过匹配的子目录或文件（目录命中则整棵子树保留）；应用/恢复的搬运不受影响。';
  exWrap.field.appendChild(exHint);
  body.appendChild(exWrap.field);

  // 水位自动清理：RAM 盘到严重线（默认 95%）时的预授权，30 分钟冷却防反复删
  const autoSelect = document.createElement('select');
  autoSelect.className = 'select';
  [['', '未启用（水位越线只提醒，不自动删）'],
   ['empty', '清空全部（最激进）'],
   ['older', '只清早于 N 小时修改的']].forEach(function (p) {
    const o = document.createElement('option');
    o.value = p[0];
    o.textContent = p[1];
    autoSelect.appendChild(o);
  });
  const autoHours = mkInput('小时数，例如 24');
  autoHours.type = 'number';
  autoHours.min = '1';
  autoHours.step = '1';
  autoHours.value = '24';
  const initPolicy = rule ? (rule.auto_clean_policy || '') : '';
  const initHours = /older:(\d+)/.exec(initPolicy);
  if (initPolicy === 'empty') autoSelect.value = 'empty';
  else if (initHours) { autoSelect.value = 'older'; autoHours.value = String(Math.max(1, Math.round(Number(initHours[1]) / 3600))); }
  const autoWrap = mk('水位自动清理（预授权）', autoSelect);
  const autoRow = document.createElement('div');
  autoRow.className = 'field-row';
  autoRow.appendChild(autoWrap.field);
  const hoursWrap = mk('早于（小时）', autoHours);
  autoRow.appendChild(hoursWrap.field);
  const autoHint = document.createElement('div');
  autoHint.className = 'hint';
  autoHint.textContent = 'RAM 盘水位到严重线时按此策略自动清理（排除模式照常保护）；30 分钟冷却，不会反复删。';
  autoRow.appendChild(autoHint);
  function syncAutoHours() { hoursWrap.field.style.display = autoSelect.value === 'older' ? '' : 'none'; }
  autoSelect.addEventListener('change', syncAutoHours);
  syncAutoHours();
  body.appendChild(autoRow);

  function autoPolicyValue() {
    if (autoSelect.value === 'empty') return 'empty';
    if (autoSelect.value === 'older') {
      const h = Math.floor(Number(autoHours.value));
      return h > 0 ? 'older:' + (h * 3600) : '';
    }
    return '';
  }

  async function checkSource() {
    const v = srcInput.value.trim();
    if (!v) { srcHint.textContent = ''; srcHint.className = 'hint'; return; }
    try {
      const info = await invoke('inspect_source', { template: v });
      const parts = ['展开后：' + info.expanded + (info.exists ? '（存在）' : '（当前不存在）')];
      if (info.unknown_vars.length) parts.push('未解析变量：' + info.unknown_vars.join('、'));
      if (info.requires_elevation) parts.push('该路径在受保护位置，应用时需要管理员权限');
      srcHint.textContent = parts.join(' · ');
      srcHint.className = info.blocking.length ? 'hint err'
        : (info.requires_elevation || info.unknown_vars.length) ? 'hint warn' : 'hint ok';
      if (info.blocking.length) srcHint.textContent = '⛔ ' + info.blocking.join('；');
    } catch (e) {
      srcHint.textContent = e.message;
      srcHint.className = 'hint err';
    }
  }

  async function checkTarget() {
    const sub = subInput.value.trim();
    if (!sub) { tgtHint.textContent = ''; tgtHint.className = 'hint'; return; }
    try {
      const info = await invoke('inspect_target', { rootId: Number(rootSelect.value), subpath: sub });
      let text = '目标：' + info.target;
      if (!info.root_ready) text += ' · 目标根当前不可用';
      if (info.occupied) text += ' · 该位置已有内容（应用时需要开启「合并」）';
      tgtHint.textContent = text;
      tgtHint.className = 'hint ' + (!info.root_ready || info.occupied ? 'warn' : '');
    } catch (e) {
      tgtHint.textContent = e.message;
      tgtHint.className = 'hint err';
    }
  }

  srcInput.addEventListener('blur', checkSource);
  subInput.addEventListener('blur', checkTarget);
  rootSelect.addEventListener('change', checkTarget);
  if (rule) { checkSource(); checkTarget(); }

  openModal({
    title: rule ? '编辑规则 #' + rule.id : '添加规则',
    body: body,
    actions: [
      { label: '取消', onClick: closeModal },
      {
        label: rule ? '保存' : '添加',
        kind: 'primary',
        onClick: async function () {
          // ⚠️ 字段名必须是 snake_case：Tauri 只把「命令参数名」从 camelCase 转
          // 成 snake_case（如 rootId→root_id），嵌套结构体的字段名原样反序列化。
          // 这里发 sourceTemplate 而 Rust RuleForm 要 source_template，就会报
          // `missing field source_template`（真机才暴露，mock 校验形状能拦住）。
          const form = {
            name: nameInput.value,
            category: catSelect.value,
            source_template: srcInput.value,
            target_root_id: Number(rootSelect.value),
            target_subpath: subInput.value,
            link_type: linkSelect.value,
            want_enabled: onCheck.checked,
            rebuild_on_boot: bootCheck.checked,
            notes: notes.value || null,
            excludes: excludes.value.split(/\r?\n/).map(function (s) { return s.trim(); })
              .filter(function (s) { return s.length > 0; }),
            auto_clean_policy: autoPolicyValue(),
          };
          try {
            if (rule) await invoke('update_rule', { id: rule.id, form: form });
            else await invoke('add_rule', { form: form });
            closeModal();
            toast(rule ? '已保存' : '已添加规则', form.name, 'ok');
            await refresh(rule ? true : false);
          } catch (e) {
            toast(rule ? '保存失败' : '添加失败', e.message, 'error');
          }
        },
      },
    ],
  });
}

// ---------------------------------------------------------------------------
// 文件夹清理（清空 / 按时间删除）
// ---------------------------------------------------------------------------

function nowSecs() { return Math.floor(Date.now() / 1000); }

/** 「昨天及更早」的界点 = 今天 00:00（本地时区）。早于它的就是昨天或更早。 */
function todayMidnightSecs() {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

function toLocalInputValue(d) {
  const p = function (n) { return String(n).padStart(2, '0'); };
  return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) +
    'T' + p(d.getHours()) + ':' + p(d.getMinutes());
}

/**
 * 打开清理对话框。
 *
 * `targets` 是清理目标数组：单条规则时 `[{kind:'rule', id}]`；底部「清理…」批量清理时
 * 把全部规则一次性传入，共用同一套策略。破坏性操作，走「先预演、看到清单再点确认删除」
 * 两步，删除按钮在预演前禁用。
 */
async function openCleanDialog(targets, headingText) {
  const multi = targets.length > 1;
  const body = document.createElement('div');

  const head = document.createElement('div');
  head.className = 'field';
  head.textContent = headingText;
  body.appendChild(head);

  const warn = document.createElement('div');
  warn.className = 'hint warn';
  warn.textContent = '清理会永久删除文件（不进回收站）。占用中的文件会自动跳过，删不掉的会如实汇报。';
  body.appendChild(warn);

  // 策略单选
  const modes = [
    ['empty', '清空全部文件'],
    ['before_day', '昨天及更早修改的'],
    ['before_hour', '1 小时前修改的（不含最近 1 小时）'],
    ['between', '指定时间段内修改的'],
  ];
  const radios = [];
  modes.forEach(function (m, i) {
    const label = document.createElement('label');
    label.className = 'inline-check';
    const r = document.createElement('input');
    r.type = 'radio';
    r.name = 'cleanmode';
    r.value = m[0];
    if (i === 0) r.checked = true;
    label.appendChild(r);
    const s = document.createElement('span');
    s.textContent = m[1];
    label.appendChild(s);
    body.appendChild(label);
    radios.push(r);
  });

  // 时间段输入（仅 between 时显示）
  const rangeWrap = document.createElement('div');
  rangeWrap.className = 'field';
  const row = document.createElement('div');
  row.className = 'field-row';
  const fromInput = document.createElement('input');
  fromInput.type = 'datetime-local';
  fromInput.className = 'input';
  const toInput = document.createElement('input');
  toInput.type = 'datetime-local';
  toInput.className = 'input';
  const yesterday = new Date();
  yesterday.setHours(0, 0, 0, 0);
  fromInput.value = toLocalInputValue(yesterday);
  toInput.value = toLocalInputValue(new Date());
  const fl = document.createElement('label');
  fl.textContent = '从';
  const tl = document.createElement('label');
  tl.textContent = '到';
  row.appendChild(fl); row.appendChild(fromInput);
  row.appendChild(tl); row.appendChild(toInput);
  rangeWrap.appendChild(row);
  body.appendChild(rangeWrap);

  function selectedMode() {
    for (const r of radios) { if (r.checked) return r.value; }
    return 'empty';
  }
  function syncRange() {
    if (selectedMode() === 'between') rangeWrap.classList.remove('hidden');
    else rangeWrap.classList.add('hidden');
  }
  radios.forEach(function (r) { r.addEventListener('change', syncRange); });
  syncRange();

  function buildPolicy() {
    const mode = selectedMode();
    if (mode === 'empty') return { mode: 'empty' };
    if (mode === 'before_day') return { mode: 'before', cutoff: todayMidnightSecs() };
    if (mode === 'before_hour') return { mode: 'before', cutoff: nowSecs() - 3600 };
    const from = Math.floor(new Date(fromInput.value).getTime() / 1000);
    const to = Math.floor(new Date(toInput.value).getTime() / 1000);
    return { mode: 'between', from: from, to: to };
  }

  // 预演结果区
  const previewHost = document.createElement('div');
  previewHost.className = 'field';
  body.appendChild(previewHost);

  const btnRow = document.createElement('div');
  btnRow.className = 'field-row';
  const previewBtn = document.createElement('button');
  previewBtn.className = 'btn';
  previewBtn.textContent = '预演（看看会删哪些）';
  const delBtn = document.createElement('button');
  delBtn.className = 'btn danger';
  delBtn.textContent = '确认删除';
  delBtn.disabled = true;
  btnRow.appendChild(previewBtn);
  btnRow.appendChild(delBtn);
  body.appendChild(btnRow);

  function ruleName(t) {
    if (t.kind === 'rule') {
      const r = state.snap.rules.find(function (x) { return x.id === t.id; });
      return r ? r.name : ('#' + t.id);
    }
    return t.path;
  }

  function renderPreview(pv) {
    previewHost.textContent = '';
    const sum = document.createElement('div');
    sum.className = pv.files ? 'hint' : 'hint ok';
    sum.textContent = pv.files
      ? ('将删除：' + pv.summary + '（目录：' + pv.dir + '）')
      : ('没有符合条件的文件（目录：' + pv.dir + '）');
    previewHost.appendChild(sum);
    if (pv.links_skipped) {
      const lk = document.createElement('div');
      lk.className = 'hint warn';
      lk.textContent = '注意：目录内有 ' + pv.links_skipped + ' 个链接，不会被删除或进入。';
      previewHost.appendChild(lk);
    }
    if (pv.excluded_skipped) {
      const ex = document.createElement('div');
      ex.className = 'hint warn';
      ex.textContent = '已有 ' + pv.excluded_skipped + ' 个条目因规则的清理排除被跳过（不受影响）。';
      previewHost.appendChild(ex);
    }
    (pv.sample || []).forEach(function (s) {
      const line = document.createElement('div');
      line.className = 'hint';
      line.textContent = '· ' + s.path + '　' + humanBytes(s.bytes) + '　' +
        new Date(s.mtime * 1000).toLocaleString();
      previewHost.appendChild(line);
    });
    if (pv.files > 20) {
      const more = document.createElement('div');
      more.className = 'hint';
      more.textContent = '…（仅列出前 20 条，共 ' + pv.files + ' 个）';
      previewHost.appendChild(more);
    }
    delBtn.disabled = !pv.files;
    delBtn.textContent = pv.files ? ('确认删除 ' + pv.files + ' 个文件') : '确认删除';
  }

  function renderBatchPreview(rows, files, bytes, linksSkipped, excludedSkipped) {
    previewHost.textContent = '';
    const sum = document.createElement('div');
    sum.className = files ? 'hint' : 'hint ok';
    sum.textContent = files
      ? ('共将删除：' + files + ' 个文件 / ' + humanBytes(bytes) + '（' + rows.length + ' 条规则）')
      : '没有符合条件的文件';
    previewHost.appendChild(sum);
    if (linksSkipped) {
      const lk = document.createElement('div');
      lk.className = 'hint warn';
      lk.textContent = '注意：部分目录内有 ' + linksSkipped + ' 个链接，不会被删除或进入。';
      previewHost.appendChild(lk);
    }
    if (excludedSkipped) {
      const ex = document.createElement('div');
      ex.className = 'hint warn';
      ex.textContent = '已有 ' + excludedSkipped + ' 个条目因规则的清理排除被跳过（不受影响）。';
      previewHost.appendChild(ex);
    }
    rows.forEach(function (it) {
      const line = document.createElement('div');
      line.className = it.error ? 'hint err' : 'hint';
      line.textContent = it.error
        ? ('· ' + it.name + '：预演失败 —— ' + it.error)
        : ('· ' + it.name + '　' + (it.pv.files ? (it.pv.files + ' 个 / ' + it.pv.bytes_text) : '无需清理') + '　' + it.pv.dir);
      previewHost.appendChild(line);
    });
    delBtn.disabled = !files;
    delBtn.textContent = files ? ('确认删除 ' + files + ' 个文件') : '确认删除';
  }

  previewBtn.addEventListener('click', async function () {
    const policy = buildPolicy();
    previewBtn.disabled = true;
    try {
      if (!multi) {
        const pv = await invoke('clean_preview', { target: targets[0], policy: policy });
        renderPreview(pv);
      } else {
        const rows = [];
        let files = 0, bytes = 0, linksSkipped = 0, excludedSkipped = 0;
        for (let i = 0; i < targets.length; i++) {
          previewHost.textContent = '';
          const st = document.createElement('div');
          st.className = 'hint';
          st.textContent = '预演中 ' + (i + 1) + '/' + targets.length + ' …';
          previewHost.appendChild(st);
          const name = ruleName(targets[i]);
          try {
            const pv = await invoke('clean_preview', { target: targets[i], policy: policy });
            rows.push({ name: name, pv: pv });
            files += pv.files;
            bytes += pv.bytes;
            linksSkipped += pv.links_skipped;
            excludedSkipped += pv.excluded_skipped || 0;
          } catch (e) {
            rows.push({ name: name, error: e.message });
          }
        }
        renderBatchPreview(rows, files, bytes, linksSkipped, excludedSkipped);
      }
    } catch (e) {
      toast('预演失败', e.message, 'error');
    } finally {
      previewBtn.disabled = false;
    }
  });

  delBtn.addEventListener('click', async function () {
    delBtn.disabled = true;
    const policy = buildPolicy();
    try {
      if (!multi) {
        const r = await invoke('clean_run', { target: targets[0], policy: policy, dryRun: false });
        closeModal();
        toast('清理完成', r.summary, r.skipped ? 'warn' : 'ok');
      } else {
        let deletedFiles = 0, deletedBytes = 0, skipped = 0, failed = 0;
        for (let i = 0; i < targets.length; i++) {
          delBtn.textContent = '清理中 ' + (i + 1) + '/' + targets.length + ' …';
          try {
            const r = await invoke('clean_run', { target: targets[i], policy: policy, dryRun: false });
            deletedFiles += r.deleted_files;
            deletedBytes += r.deleted_bytes;
            skipped += r.skipped;
          } catch (e) {
            failed++;
          }
        }
        closeModal();
        toast('批量清理完成',
          '删除 ' + deletedFiles + ' 个文件 / ' + humanBytes(deletedBytes) +
          (skipped ? '，跳过 ' + skipped + ' 个' : '') + (failed ? '，失败 ' + failed + ' 条' : ''),
          failed ? 'warn' : 'ok');
      }
      await refresh(true);
    } catch (e) {
      toast('清理失败', e.message, 'error');
      delBtn.disabled = false;
    }
  });

  openModal({
    title: multi ? '批量清理（全部规则）' : '清理文件夹',
    body: body,
    wide: true,
    actions: [{ label: '关闭', onClick: closeModal }],
  });
}

/** 底部「清理…」：按统一策略清理全部条目的目标目录。 */
async function openBatchCleanDialog() {
  const rules = state.snap.rules || [];
  if (!rules.length) {
    toast('没有可清理的规则', '先添加规则再清理', 'warn');
    return;
  }
  const targets = rules.map(function (r) { return { kind: 'rule', id: r.id }; });
  await openCleanDialog(targets, '全部 ' + rules.length + ' 条规则：统一策略清理各自的目标目录');
}

// ---------------------------------------------------------------------------
// 磁盘空间占用统计
// ---------------------------------------------------------------------------

/** 扫描进度提示节点；扫描中由 td-disk-progress 事件更新。 */
let scanHint = null;

backend.listen('td-disk-progress', function (ev) {
  if (!scanHint) return;
  const p = ev.payload || {};
  const total = p.total || 0;
  scanHint.textContent = total
    ? ('正在扫描 ' + (p.name || '') + '（' + (p.index + 1) + '/' + total + '）…')
    : '正在汇总卷容量…';
});

/** 工具栏「空间统计」：实时扫描各规则缓存占用，并汇总相关卷容量。 */
async function openDiskStats() {
  const body = document.createElement('div');

  const status = document.createElement('div');
  status.className = 'hint';
  status.textContent = '正在扫描各规则缓存目录的占用…';
  body.appendChild(status);

  const host = document.createElement('div');
  body.appendChild(host);

  openModal({
    title: '磁盘空间占用统计',
    body: body,
    wide: true,
    actions: [
      { label: '重新扫描', onClick: function () { load(); } },
      { label: '关闭', onClick: closeModal },
    ],
  });

  async function load() {
    host.textContent = '';
    status.className = 'hint';
    status.textContent = '正在扫描各规则缓存目录的占用…';
    scanHint = status;
    try {
      const data = await invoke('disk_usage');
      renderDiskStats(host, status, data);
    } catch (e) {
      status.className = 'hint err';
      status.textContent = '统计失败：' + e.message;
    } finally {
      scanHint = null;
    }
  }
  await load();
}

function renderDiskStats(host, status, data) {
  host.textContent = '';
  status.className = 'hint ok';
  status.textContent = '共 ' + data.rule_count + ' 条规则，缓存目录合计占用 ' +
    data.total_bytes_text + '（扫描耗时 ' + data.elapsed_ms + ' ms）';

  // ---- 各规则占用排行（横向条形图） ----
  const barsTitle = document.createElement('div');
  barsTitle.className = 'chart-title';
  barsTitle.textContent = '各规则缓存占用排行';
  host.appendChild(barsTitle);

  const rules = (data.rules || []).filter(function (r) { return r.bytes > 0; });
  if (!rules.length) {
    const none = document.createElement('div');
    none.className = 'hint';
    none.textContent = '（所有规则目录当前都为空或不存在）';
    host.appendChild(none);
  } else {
    const max = rules.reduce(function (m, r) { return Math.max(m, r.bytes); }, 1);
    const bars = document.createElement('div');
    bars.className = 'bars';
    rules.forEach(function (r) {
      bars.appendChild(barRow(r.name, (r.bytes / max) * 100, r.bytes_text, r.dir));
    });
    host.appendChild(bars);
  }

  // ---- 磁盘卷容量 ----
  const volTitle = document.createElement('div');
  volTitle.className = 'chart-title';
  volTitle.textContent = '磁盘卷容量';
  host.appendChild(volTitle);

  const vols = data.volumes || [];
  if (!vols.length) {
    const none = document.createElement('div');
    none.className = 'hint';
    none.textContent = '（未取到卷容量信息）';
    host.appendChild(none);
  } else {
    const list = document.createElement('div');
    list.className = 'vol-list';
    vols.forEach(function (v) { list.appendChild(volumeRow(v)); });
    host.appendChild(list);
  }
}

function barRow(name, pct, val, title) {
  const row = document.createElement('div');
  row.className = 'bar-row';
  const nm = document.createElement('div');
  nm.className = 'bar-name';
  nm.textContent = name;
  nm.title = title || name;
  const track = document.createElement('div');
  track.className = 'bar-track';
  const fill = document.createElement('div');
  fill.className = 'bar-fill';
  fill.style.width = Math.max(1, Math.min(100, pct)) + '%';
  track.appendChild(fill);
  const va = document.createElement('div');
  va.className = 'bar-val';
  va.textContent = val;
  row.appendChild(nm);
  row.appendChild(track);
  row.appendChild(va);
  return row;
}

function volumeRow(v) {
  const row = document.createElement('div');
  row.className = 'vol-row';
  const head = document.createElement('div');
  head.className = 'vol-head';
  const nm = document.createElement('div');
  nm.className = 'vol-name';
  nm.textContent = v.path + (v.roles && v.roles.length ? '（' + v.roles.join(' · ') + '）' : '');
  const txt = document.createElement('div');
  txt.className = 'vol-text';
  txt.textContent = '已用 ' + v.used_text + ' / 共 ' + v.total_text +
    ' · 可用 ' + v.free_text + '（' + v.used_pct.toFixed(1) + '%）';
  head.appendChild(nm);
  head.appendChild(txt);
  row.appendChild(head);
  const track = document.createElement('div');
  track.className = 'vol-track';
  const fill = document.createElement('div');
  fill.className = 'vol-fill' + (v.used_pct >= 90 ? ' hot' : v.used_pct >= 75 ? ' warm' : '');
  fill.style.width = Math.max(0, Math.min(100, v.used_pct)) + '%';
  track.appendChild(fill);
  row.appendChild(track);
  return row;
}

// ---------------------------------------------------------------------------
// 事件绑定（事件委托）
// ---------------------------------------------------------------------------

document.addEventListener('click', async function (ev) {
  const target = ev.target.closest('[data-act]');
  if (target) {
    const act = target.dataset.act;
    const id = Number(target.dataset.id);
    if (act === 'focus') {
      state.focusId = id;
      renderRows();
      renderDetail();
      return;
    }
    if (act === 'apply') { await runApply([id], false); return; }
    if (act === 'apply-elevated') { await runElevated('apply', [id]); return; }
    if (act === 'restore') { await runRestore([id]); return; }
    if (act === 'relink') { await doRelink(id); return; }
    if (act === 'locks') { await doLocks(id); return; }
    if (act === 'clean') {
      const r = state.snap.rules.find(function (x) { return x.id === id; });
      await openCleanDialog([{ kind: 'rule', id: id }], '规则：' + (r ? r.name : ('#' + id)));
      return;
    }
    if (act === 'edit') { await openRuleForm(id); return; }
    if (act === 'copy-source') {
      const r = state.snap.rules.find(function (x) { return x.id === id; });
      if (r) await copyText(r.source);
      return;
    }
    if (act === 'reveal') {
      const r = state.snap.rules.find(function (x) { return x.id === id; });
      if (!r) return;
      try { await invoke('reveal_in_explorer', { path: r.source }); }
      catch (e) { toast('打开失败', e.message, 'error'); }
      return;
    }
    return;
  }

  const el = ev.target;
  const closest = el.closest ? el.closest.bind(el) : function () { return null; };
  // 「更多」菜单：点菜单外任意处收起；点「更多」切换；点菜单项后收起再走下面的分支。
  if (!closest('.more-wrap')) hideMoreMenu();
  if (el.id === 'btn-more') { toggleMoreMenu(); return; }
  if (closest('#more-menu')) hideMoreMenu();
  if (el.id === 'btn-refresh') await refresh(true);
  else if (el.id === 'btn-add') await openRuleForm(null);
  else if (el.id === 'btn-apply') { if (requirePicked('应用')) await runApply(pickedIds(), false); }
  else if (el.id === 'btn-restore') { if (requirePicked('恢复')) await runRestore(pickedIds()); }
  else if (el.id === 'btn-enable' || el.id === 'btn-disable') {
    if (!requirePicked('开关')) return;
    const enabled = el.id === 'btn-enable';
    try {
      await invoke('set_rules_enabled', { ids: pickedIds(), enabled: enabled });
      toast(enabled ? '已设为期望开启' : '已设为期望关闭', pickedIds().length + ' 条规则', 'ok');
      await refresh(true);
    } catch (e) { toast('操作失败', e.message, 'error'); }
  } else if (el.id === 'btn-locks') {
    if (!requirePicked('检测')) return;
    await doLocks(pickedIds()[0]);
  } else if (el.id === 'btn-clean') {
    await openBatchCleanDialog();
  } else if (el.id === 'btn-disk') {
    await openDiskStats();
  } else if (el.id === 'btn-relink') {
    if (!requirePicked('重建')) return;
    for (const id of pickedIds()) await doRelink(id);
  } else if (el.id === 'btn-delete') {
    if (!requirePicked('删除')) return;
    const ids = pickedIds();
    const body = document.createElement('div');
    const p1 = document.createElement('div');
    p1.className = 'field';
    p1.textContent = '将删除 ' + ids.length + ' 条规则的配置记录。';
    const p2 = document.createElement('div');
    p2.className = 'field';
    const h = document.createElement('div');
    h.className = 'hint warn';
    h.textContent = '注意：这只会删掉规则记录，不会改动磁盘上的目录或链接。如果想把链接还原成真实目录，请先执行「恢复」。';
    p2.appendChild(h);
    body.appendChild(p1);
    body.appendChild(p2);
    openModal({
      title: '确认删除规则',
      body: body,
      actions: [
        { label: '取消', onClick: closeModal },
        {
          label: '删除', kind: 'danger',
          onClick: async function () {
            try {
              await invoke('delete_rules', { ids: ids });
              state.picked.clear();
              closeModal();
              toast('已删除', ids.length + ' 条规则记录', 'ok');
              await refresh(false);
            } catch (e) { toast('删除失败', e.message, 'error'); }
          },
        },
      ],
    });
  } else if (el.id === 'btn-boot') await doBoot();
  else if (el.id === 'btn-ops') await openOps();
  else if (el.id === 'btn-settings') await openSettings();
  else if (el.id === 'modal-close') closeModal();
});

/** 切换工具栏「更多」下拉（诊断 / 低频操作收纳在这里，主按钮区只留高频动作）。 */
function toggleMoreMenu() {
  const m = document.getElementById('more-menu');
  if (m) m.classList.toggle('hidden');
}

function hideMoreMenu() {
  const m = document.getElementById('more-menu');
  if (m) m.classList.add('hidden');
}

document.addEventListener('change', function (ev) {
  const el = ev.target;
  if (el.dataset && el.dataset.act === 'pick') {
    const id = Number(el.dataset.id);
    if (el.checked) state.picked.add(id); else state.picked.delete(id);
    renderRows();
    return;
  }
  if (el.id === 'filter-category') { state.filters.category = el.value; renderRows(); }
  else if (el.id === 'only-enabled') { state.filters.onlyEnabled = el.checked; renderRows(); }
  else if (el.id === 'only-attention') { state.filters.onlyAttention = el.checked; renderRows(); }
  else if (el.id === 'sel-all' || el.id === 'sel-all-head') {
    const rows = filteredRules();
    if (el.checked) rows.forEach(function (r) { state.picked.add(r.id); });
    else rows.forEach(function (r) { state.picked.delete(r.id); });
    renderRows();
  }
});

document.addEventListener('input', function (ev) {
  if (ev.target.id === 'search') {
    state.filters.q = ev.target.value;
    renderRows();
  }
});

document.addEventListener('keydown', function (ev) {
  if (ev.key === 'Escape') {
    // 弹窗优先于详情：弹窗开着时 Esc 只关弹窗，不顺手把详情也收掉。
    if (!$('modal').classList.contains('hidden')) {
      closeModal();
    } else if ($('detail').classList.contains('open')) {
      state.focusId = null;
      renderDetail();
      renderRows();
    }
  }
  if ((ev.ctrlKey || ev.metaKey) && ev.key === 'r') {
    ev.preventDefault();
    refresh(true);
  }
});

// ---------------------------------------------------------------------------
// 拖拽排序（同一分类内）。列表按 category 分组展示，跨分类拖拽没有意义
// （后端 ORDER BY category 优先），在 dragover 阶段就拒绝。
// ---------------------------------------------------------------------------

/** 只有抓着把手才允许拖整行：若行常驻 draggable，文本选择与点击都会被拖拽劫持。 */
document.addEventListener('mousedown', function (ev) {
  const h = ev.target.closest('.drag-handle');
  if (!h) return;
  const tr = h.closest('tr');
  if (tr) tr.draggable = true;
});

document.addEventListener('dragstart', function (ev) {
  const tr = ev.target.closest && ev.target.closest('tr');
  if (!tr || !tr.draggable || !tr.dataset.id) return;
  const r = state.snap.rules.find(function (x) { return x.id === Number(tr.dataset.id); });
  if (!r) return;
  state.drag = { id: r.id, category: r.category };
  ev.dataTransfer.effectAllowed = 'move';
  ev.dataTransfer.setData('text/plain', String(r.id));
});

document.addEventListener('dragover', function (ev) {
  if (!state.drag) return;
  const tr = ev.target.closest && ev.target.closest('tr');
  if (!tr || !tr.dataset.id) return;
  const r = state.snap.rules.find(function (x) { return x.id === Number(tr.dataset.id); });
  // 跨分类 / 拖到自己：不 preventDefault = 不允许落点
  if (!r || r.category !== state.drag.category || r.id === state.drag.id) return;
  ev.preventDefault();
  ev.dataTransfer.dropEffect = 'move';
  tr.classList.add('drag-over');
});

document.addEventListener('drop', async function (ev) {
  if (!state.drag) return;
  const drag = state.drag;
  state.drag = null;
  clearDragFeedback();
  const tr = ev.target.closest && ev.target.closest('tr');
  if (!tr || !tr.dataset.id) return;
  ev.preventDefault();
  const targetId = Number(tr.dataset.id);
  if (targetId === drag.id) return;

  // 落点在行的上半 → 插到目标之前；下半 → 之后。
  const rect = tr.getBoundingClientRect();
  const before = ev.clientY < rect.top + rect.height / 2;
  const orderedIds = categoryOrderAfterDrop(drag.id, targetId, before);
  try {
    await invoke('reorder_rules', { orderedIds: orderedIds });
    await refresh(true);
  } catch (e) {
    toast('调整顺序失败', e.message, 'error');
  }
});

document.addEventListener('dragend', function () {
  state.drag = null;
  clearDragFeedback();
});

function clearDragFeedback() {
  document.querySelectorAll('tr.drag-over').forEach(function (tr) { tr.classList.remove('drag-over'); });
  document.querySelectorAll('tr[draggable="true"]').forEach(function (tr) { tr.draggable = false; });
}

/** 组装拖拽后该分类的全量顺序（后端要求完整覆盖该分类 ——
 *  界面正被筛选/搜索时也要从全量快照取，否则被隐藏的规则顺序会悬空）。 */
function categoryOrderAfterDrop(dragId, targetId, before) {
  const all = state.snap.rules;
  const dragRule = all.find(function (r) { return r.id === dragId; });
  const group = all.filter(function (r) { return r.category === dragRule.category; });
  const rest = group.filter(function (r) { return r.id !== dragId; });
  const idx = rest.findIndex(function (r) { return r.id === targetId; });
  rest.splice(before ? idx : idx + 1, 0, dragRule);
  return rest.map(function (r) { return r.id; });
}

// 后端进度事件
backend.listen('td-progress', function (ev) {
  const p = ev.payload || {};
  const pct = p.total ? ((p.index + 0.5) / p.total) * 100 : 0;
  setProgress(pct, '#' + (p.index + 1) + '/' + p.total + ' ' + p.name + ' · ' + (p.detail || p.phase));
});

backend.listen('td-tray-boot', function (ev) {
  const p = ev.payload || {};
  toast('托盘：开机重建', p.message, p.ok ? 'ok' : 'warn');
  refresh(true);
});

// ---------------------------------------------------------------------------
// Mock 后端（仅用于本地静态预览 / 无头截图核验，不参与打包逻辑）
// ---------------------------------------------------------------------------

function createMockBackend() {
  const rules = [
    mockRule(1, 'Chrome 缓存', 'browser', '%LOCALAPPDATA%\\Google\\Chrome\\User Data\\Default\\Cache', 'browsers\\chrome-cache', '✅', 'link_ok', '链接正常', { last_files: 34521, last_bytes: 1288490188, auto_clean_policy: 'older:86400' }),
    mockRule(2, 'Edge 缓存', 'browser', '%LOCALAPPDATA%\\Microsoft\\Edge\\User Data\\Default\\Cache', 'browsers\\edge-cache', '✅', 'link_ok', '链接正常', { last_files: 21002, last_bytes: 734003200 }),
    mockRule(3, 'Firefox 缓存', 'browser', '%APPDATA%\\Mozilla\\Firefox\\Profiles\\xxxx.default-release\\cache2', 'browsers\\firefox-cache2', '⚠️', 'link_ok_empty', '目录联接 → R:\\browsers\\firefox-cache2（目标为空（RAM 盘刚重启））'),
    mockRule(4, 'Photoshop 暂存盘', 'design', 'C:\\Users\\me\\AppData\\Roaming\\Adobe\\Common\\Media Cache Files', 'design\\ps-media-cache', '⚠️', 'link_broken', '目录联接 断裂，目标不可达：R:\\design\\ps-media-cache', { needs_attention: true }),
    mockRule(5, 'VS Code 缓存', 'dev', '%APPDATA%\\Code\\Cache', 'dev\\vscode-cache', '❌', 'plain_dir', '未链接（真实目录）', { excludes: ['logs', 'CachedData\\old'] }),
    mockRule(6, '微信文件缓存', 'office', '%APPDATA%\\Tencent\\WeChat\\FileStorage', 'office\\wechat-files', '❌', 'plain_dir', '未链接（真实目录）', { want_enabled: false }),
    mockRule(7, 'npm 缓存', 'dev', '%LOCALAPPDATA%\\npm-cache', 'dev\\npm-cache', '✅', 'link_ok', '链接正常', { last_files: 88213, last_bytes: 3221225472 }),
    mockRule(8, 'Premiere 媒体缓存', 'design', 'C:\\Users\\me\\AppData\\Roaming\\Adobe\\Common\\Media Cache', 'design\\pr-media-cache', '❌', 'plain_dir', '未链接（真实目录）', { needs_elevation: true, has_acl: true, want_enabled: false }),
  ];

  function mockRule(id, name, category, tpl, sub, badge, stateKey, label, extra) {
    const e = extra || {};
    return {
      id: id, name: name, category: category,
      source_template: tpl,
      source: tpl.replace('%LOCALAPPDATA%', 'C:\\Users\\me\\AppData\\Local').replace('%APPDATA%', 'C:\\Users\\me\\AppData\\Roaming'),
      target: 'R:\\' + sub,
      target_root_id: 1, target_subpath: sub, link_type: 'junction',
      want_enabled: e.want_enabled !== false, rebuild_on_boot: true,
      has_acl: !!e.has_acl, needs_elevation: !!e.needs_elevation, notes: null,
      excludes: e.excludes || [],
      auto_clean_policy: e.auto_clean_policy || '',
      badge: badge, label: label,
      needs_attention: !!e.needs_attention || badge === '❌',
      warnings: e.needs_elevation ? ['该路径位于受保护位置，应用时需要管理员权限'] : [],
      unknown_vars: [], last_files: e.last_files || null, last_bytes: e.last_bytes || null,
      applied_at: e.last_files ? '2026-10-14 09:45:56' : null,
      _state: stateKey,
    };
  }

  let mockCloseToTray = true;

  function stats() {
    const s = { total: rules.length, enabled: 0, ok: 0, warn: 0, plain: 0, other: 0, needs_attention: 0 };
    rules.forEach(function (r) {
      if (r.want_enabled) s.enabled++;
      if (r.needs_attention) s.needs_attention++;
      if (r.badge === '✅') s.ok++;
      else if (r.badge === '⚠️') s.warn++;
      else if (r.badge === '❌') s.plain++;
      else s.other++;
    });
    return s;
  }

  function snapshot() {
    return {
      version: '0.1.0', schema_version: 1,
      data_dir: 'C:\\ProgramData\\TempDirector',
      db_path: 'C:\\ProgramData\\TempDirector\\tempdirector.db',
      categories: ['browser', 'design', 'dev', 'system', 'media', 'office', 'game', 'other'],
      roots: [{
        id: 1, label: 'RAMDisk 4G', path: 'R:', is_ramdisk: true, wait_timeout_ms: 90000,
        state: 'ready', message: '就绪，可用空间 3.7 GB', unknown_vars: [],
      }],
      rules: rules.map(function (r) { const c = Object.assign({}, r); delete c._state; return c; }),
      stats: stats(),
      banners: [
        { level: 'warn', text: '有 4 条规则需要处理（断链 / 真实目录 / 外来重解析点）' },
        { level: 'error', text: 'RAM 盘即将写满：RAMDisk 4G（R:）已用 96.4%，仅剩 147.5 MB —— 建议清理或缩小规则', action: 'sweep' },
      ],
      open_ops: [],
    };
  }

  const find = function (id) { return rules.find(function (r) { return r.id === id; }); };

  /** 校验前端发来的表单形状。
   * 为什么 mock 要做这件事：字段名对不上时真机是"保存失败：missing field xxx"，
   * 而 mock 若不校验，就会一路绿灯把问题藏到用户手上（这正是一次真实教训）。 */
  function assertFormShape(f) {
    const need = ['name', 'category', 'source_template', 'target_root_id',
      'target_subpath', 'link_type', 'want_enabled', 'rebuild_on_boot'];
    const miss = need.filter(function (k) { return !(f && (k in f)); });
    if (miss.length) {
      throw new Error('mock 校验失败：表单缺少字段 ' + miss.join('、') +
        '（前端发出去的字段名与 Rust 结构体对不上了）');
    }
    return f;
  }

  return {
    invoke: function (name, args) {
      args = args || {};
      switch (name) {
        case 'ui_snapshot': return Promise.resolve(snapshot());
        case 'recent_events':
          return Promise.resolve([
            { id: 5, rule_id: 2, level: 'info', kind: 'applied', message: 'Edge 缓存：跨卷复制：21002 个文件 / 700.0 MB', at: '2026-10-14 09:45:56' },
            { id: 4, rule_id: 1, level: 'info', kind: 'applied', message: 'Chrome 缓存：同卷重命名：34521 个文件 / 1.2 GB', at: '2026-10-14 09:45:55' },
            { id: 3, rule_id: 4, level: 'error', kind: 'apply_failed', message: 'Photoshop 暂存盘：目标根不可用（RAM 盘未挂载）', at: '2026-10-14 09:45:12' },
            { id: 2, rule_id: null, level: 'warn', kind: 'blocked', message: '微信文件缓存：3 个文件被占用，占用者：WeChat.exe（PID 8024）', at: '2026-10-14 09:44:01' },
            { id: 1, rule_id: null, level: 'info', kind: 'boot_rebuild', message: '目标卷 1/1 就绪；规则 6 条：新建目录 6，链接自动恢复 5，本就正常 1', at: '2026-10-14 09:40:00' },
          ]);
        case 'recent_ops':
          return Promise.resolve([
            { id: 3, rule_id: 5, kind: 'apply', phase: 'failed', source_path: 'C:\\Users\\me\\AppData\\Roaming\\Code\\Cache', target_path: 'R:\\dev\\vscode-cache', file_count: 120, bytes: 4096, error: '源目录正被占用，已中止', started_at: '2026-10-14 09:45:12', finished_at: '2026-10-14 09:45:12' },
            { id: 2, rule_id: 2, kind: 'apply', phase: 'done', source_path: 'C:\\Users\\me\\AppData\\Local\\Microsoft\\Edge\\User Data\\Default\\Cache', target_path: 'R:\\browsers\\edge-cache', file_count: 21002, bytes: 734003200, error: null, started_at: '2026-10-14 09:45:20', finished_at: '2026-10-14 09:45:56' },
          ]);
        case 'savings': {
          // 「当前重定向」与后端口径一致地实时算：期望开启 且 链接正常。
          // moved / cleaned 是历史累计（不在规则快照里），给一组与
          // recent_events / recent_ops 的 mock 流水对得上的数字。
          let rr = 0, rf = 0, rb = 0;
          rules.forEach(function (r) {
            if (r.want_enabled && (r._state === 'link_ok' || r._state === 'link_ok_empty')) {
              rr++; rf += r.last_files || 0; rb += r.last_bytes || 0;
            }
          });
          return Promise.resolve({
            redirected_rules: rr, redirected_files: rf, redirected_bytes: rb,
            redirected_text: humanBytes(rb),
            moved_files: 55523, moved_bytes: 2022493388, moved_text: humanBytes(2022493388),
            cleaned_files: 12830, cleaned_bytes: 1879048192, cleaned_text: humanBytes(1879048192),
          });
        }
        case 'add_root':
          return Promise.resolve(1);
        case 'delete_root':
          return Promise.resolve(null);
        case 'reorder_rules': {
          // 这些 id 原本占据的索引集合不变，按新顺序回填（其它规则不动）。
          const set = {};
          args.orderedIds.forEach(function (id) { set[id] = true; });
          const moved = rules.filter(function (r) { return set[r.id]; });
          let mi = 0;
          const next = rules.map(function (r) { return set[r.id] ? moved[mi++] : r; });
          rules.length = 0;
          rules.push.apply(rules, next);
          return Promise.resolve(args.orderedIds.length);
        }
        case 'add_rule': {
          const f = assertFormShape(args.form);
          const id = Math.max.apply(null, rules.map(function (r) { return r.id; })) + 1;
          rules.push(mockRule(id, f.name, f.category, f.source_template, f.target_subpath, '❌', 'plain_dir', '未链接（真实目录）', { want_enabled: f.want_enabled, auto_clean_policy: f.auto_clean_policy || '' }));
          return Promise.resolve(id);
        }
        case 'update_rule': {
          const r = find(args.id);
          assertFormShape(args.form);
          if (r) {
            r.name = args.form.name; r.category = args.form.category;
            r.source_template = args.form.source_template; r.target_subpath = args.form.target_subpath;
            r.source = args.form.source_template; r.target = 'R:\\' + args.form.target_subpath;
            r.want_enabled = args.form.want_enabled; r.rebuild_on_boot = args.form.rebuild_on_boot;
            r.notes = args.form.notes;
            r.excludes = args.form.excludes || [];
            r.auto_clean_policy = args.form.auto_clean_policy || '';
          }
          return Promise.resolve(null);
        }
        case 'delete_rules': {
          args.ids.forEach(function (id) {
            const i = rules.findIndex(function (r) { return r.id === id; });
            if (i >= 0) rules.splice(i, 1);
          });
          return Promise.resolve(args.ids.length);
        }
        case 'set_rules_enabled':
          args.ids.forEach(function (id) { const r = find(id); if (r) r.want_enabled = args.enabled; });
          return Promise.resolve(args.ids.length);
        case 'capture_acl': { const r = find(args.id); if (r) r.has_acl = true; return Promise.resolve('D:(A;;FA;;;SY)'); }
        case 'clear_acl': { const r = find(args.id); if (r) r.has_acl = false; return Promise.resolve(null); }
        case 'water_poll':
          return Promise.resolve([
            { root_id: 1, label: 'RAMDisk 4G', path: 'R:', used_pct: 70.0, free: 1288490188, free_text: '1.2 GB', total_text: '4.0 GB', level: 'ok', message: '已用 70.0%，剩余 1.2 GB' },
          ]);
        case 'preset_rules':
          return Promise.resolve([
            { name: 'Chrome 网页缓存', category: 'browser', source_template: '%LOCALAPPDATA%\\Google\\Chrome\\User Data\\Default\\Cache', target_subpath: 'browsers\\chrome-cache', note: null, source: 'C:\\Users\\me\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache', exists: true },
            { name: 'Edge 网页缓存', category: 'browser', source_template: '%LOCALAPPDATA%\\Microsoft\\Edge\\User Data\\Default\\Cache', target_subpath: 'browsers\\edge-cache', note: null, source: 'C:\\Users\\me\\AppData\\Local\\Microsoft\\Edge\\User Data\\Default\\Cache', exists: true },
            { name: 'npm 缓存', category: 'dev', source_template: '%LOCALAPPDATA%\\npm-cache', target_subpath: 'dev\\npm-cache', note: null, source: 'C:\\Users\\me\\AppData\\Local\\npm-cache', exists: true },
            { name: '用户 TEMP', category: 'system', source_template: '%LOCALAPPDATA%\\Temp', target_subpath: 'system\\user-temp', note: '体积最大也最容易被占用：应用时被占的文件会自动跳过', source: 'C:\\Users\\me\\AppData\\Local\\Temp', exists: true },
            { name: '微信网页缓存', category: 'office', source_template: '%APPDATA%\\Tencent\\xwechat\\radium\\web', target_subpath: 'office\\wechat-radium-web', note: '只迁这个子目录；整个 xwechat 里有聊天记录，切勿整体迁移', source: 'C:\\Users\\me\\AppData\\Roaming\\Tencent\\xwechat\\radium\\web', exists: true },
          ]);
        case 'clean_preview': {
          const t = args.target || {};
          return Promise.resolve({
            dir: t.kind === 'folder' ? t.path : 'R:\\dev\\vscode-cache',
            label: '（模拟）VS Code 缓存',
            files: 1280, bytes: 536870912, bytes_text: '512.0 MB', links_skipped: 0,
            excluded_skipped: t.kind === 'rule' ? 12 : 0,
            sample: [
              { path: 'Cache\\f1', bytes: 1048576, mtime: Math.floor(Date.now() / 1000) - 86400 * 3 },
              { path: 'Cache\\f2', bytes: 2097152, mtime: Math.floor(Date.now() / 1000) - 3600 * 5 },
            ],
            summary: '1280 个文件 / 512.0 MB',
          });
        }
        case 'clean_run':
          return Promise.resolve({
            dir: 'R:\\dev\\vscode-cache', label: '（模拟）VS Code 缓存',
            deleted_files: 1280, deleted_bytes: 536870912, deleted_bytes_text: '512.0 MB',
            dirs_pruned: 3, skipped: 2, skip_examples: ['f3', 'f4'], links_skipped: 0,
            excluded_skipped: 12,
            summary: '已删除 1280 个文件 / 512.0 MB，剪除 3 个空目录；2 个文件被占用或无权限已跳过（例如：f3；f4）',
          });
        case 'inspect_source': {
          const tpl = String(args.template || '');
          const expanded = tpl.replace('%LOCALAPPDATA%', 'C:\\Users\\me\\AppData\\Local').replace('%APPDATA%', 'C:\\Users\\me\\AppData\\Roaming');
          const blocked = /^[a-z]:\\windows/i.test(expanded);
          return Promise.resolve({
            expanded: expanded, exists: tpl.indexOf('%NO_SUCH') < 0,
            blocking: blocked ? ['该路径属于系统关键目录，禁止重定向：' + expanded] : [],
            warnings: /program files/i.test(tpl) ? ['该路径位于受保护位置，移动/建链需要管理员权限'] : [],
            requires_elevation: /program files|programdata/i.test(tpl),
            unknown_vars: /%NO_SUCH/.test(tpl) ? ['NO_SUCH'] : [],
          });
        }
        case 'inspect_target': {
          const sub = String(args.subpath || '').replace(/^[\\\/]+/, '');
          const busy = /busy|占用/.test(sub);
          return Promise.resolve({
            target: 'R:\\' + sub, root_ready: true, exists: busy, occupied: busy,
            state: busy ? '未链接（真实目录）' : '源路径不存在', unknown_vars: [],
          });
        }
        case 'apply_rules': {
          const out = [];
          args.ids.forEach(function (id) {
            const r = find(id);
            if (!r) { out.push({ rule_id: id, name: '#' + id, ok: false, action: 'not_found', message: '不存在', elapsed_ms: 0 }); return; }
            if (r.needs_elevation || /ps-media|vscode/i.test(r.target_subpath)) {
              out.push({ rule_id: id, name: r.name, ok: false, action: 'failed', message: '源目录正被占用，已中止（未做任何改动）：3 个文件被占用，占用者：WeChat.exe（PID 8024）', elapsed_ms: 0 });
              return;
            }
            r.badge = '✅'; r.label = '链接正常'; r.needs_attention = false;
            out.push({ rule_id: id, name: r.name, ok: true, action: 'moved_and_linked', message: '同卷重命名：120 个文件 / 4.0 MB；已建链', elapsed_ms: 42 });
          });
          if (MOCK) simulateProgress(args.ids.length);
          return Promise.resolve(out);
        }
        case 'restore_rules': {
          const out = [];
          args.ids.forEach(function (id) {
            const r = find(id);
            if (!r) return;
            r.badge = '❌'; r.label = '未链接（真实目录）'; r.needs_attention = true;
            out.push({ rule_id: id, name: r.name, ok: true, action: 'restored', message: '已恢复：120 个文件 / 4.0 MB 已搬回', elapsed_ms: 31 });
          });
          return Promise.resolve(out);
        }
        case 'relink_rule': {
          const r = find(args.id);
          if (r && r.badge !== '✅') { r.badge = '✅'; r.label = '链接正常'; r.needs_attention = false; }
          return Promise.resolve({ rule_id: args.id, name: r ? r.name : '', ok: true, action: 'relinked', message: '链接已重建', elapsed_ms: 8 });
        }
        case 'check_locks':
          return Promise.resolve({
            locked_files: ['C:\\Users\\me\\AppData\\Roaming\\Tencent\\WeChat\\FileStorage\\msg\\a.bin', 'C:\\Users\\me\\AppData\\Roaming\\Tencent\\WeChat\\FileStorage\\msg\\b.bin'],
            denied_files: [], blockers: [{ app: 'WeChat.exe', pid: 8024, is_service: false }],
            scanned: 1284, truncated: false, elapsed_ms: 63,
          });
        case 'disk_usage': {
          const rows = rules.map(function (r) {
            const bytes = r.last_bytes || 0;
            return {
              id: r.id, name: r.name, dir: r.target,
              files: r.last_files || 0, bytes: bytes, bytes_text: humanBytes(bytes),
              exists: bytes > 0,
            };
          }).sort(function (a, b) { return b.bytes - a.bytes; });
          const totalBytes = rows.reduce(function (s, r) { return s + r.bytes; }, 0);
          return Promise.resolve({
            rules: rows,
            volumes: [
              { path: 'R:\\', total: 4294967296, free: 1288490188, used: 3006477108, total_text: '4.0 GB', free_text: '1.2 GB', used_text: '2.8 GB', used_pct: 70.0, is_ramdisk: true, roles: ['RAM 盘'] },
              { path: 'C:\\', total: 511101108224, free: 128849018880, used: 382252089344, total_text: '476.0 GB', free_text: '120.0 GB', used_text: '356.0 GB', used_pct: 74.8, is_ramdisk: false, roles: ['源盘'] },
            ],
            total_bytes: totalBytes, total_bytes_text: humanBytes(totalBytes),
            rule_count: rules.length, elapsed_ms: 42,
          });
        }
        case 'boot_rebuild':
          return Promise.resolve({
            roots: [{ root_id: 1, label: 'RAMDisk 4G', path: 'R:', ready: true, waited_ms: 120, error: null }],
            rules_total: 8, dirs_created: 8, links_recovered: 6, links_repaired: 0, already_ok: 2,
            warnings: ['Photoshop 暂存盘：源路径已变回真实目录（可能被软件更新覆盖），未自动处理，请在界面上重新应用'],
            failed: [], elapsed_ms: 37,
          });
        case 'autostart_status':
          return Promise.resolve({
            registered: true,
            task_name: 'TempDirector-BootRebuild',
            command: 'C:\\Program Files\\TempDirector\\tempdirector-gui.exe boot-rebuild',
            message: '已注册：C:\\Program Files\\TempDirector\\tempdirector-gui.exe boot-rebuild',
            elevated: false,
            data_dir: 'C:\\ProgramData\\TempDirector',
            extra_args: '',
            recommended_exe: 'C:\\Program Files\\TempDirector\\tempdirector-gui.exe',
          });
        case 'autostart_enable': return Promise.resolve('C:\\Program Files\\TempDirector\\tempdirector-gui.exe boot-rebuild');
        case 'autostart_disable': return Promise.resolve('已取消开机自启');
        case 'autostart_run_now': return Promise.resolve('已触发一次开机重建任务');
        case 'run_elevated_job': {
          const out = [];
          (args.ids || []).forEach(function (id) {
            const r = find(id);
            if (!r) return;
            r.badge = '✅'; r.label = '链接正常'; r.needs_attention = false;
            out.push({ rule_id: id, name: r.name, ok: true, action: 'moved_and_linked', message: '（提权）跨卷复制：120 个文件 / 4.0 MB；已建链', elapsed_ms: 88 });
          });
          return Promise.resolve(out);
        }
        case 'export_config': return Promise.resolve([1, rules.length]);
        case 'import_config': return Promise.resolve({ roots_reused: 1, roots_created: 0, rules_created: 5, rules_skipped: 2, errors: ['第 3 条规则「Photoshop 暂存盘」未导入：已存在指向同一路径的规则'], dry_run: false });
        case 'pick_folder': return Promise.resolve(null);
        case 'pick_save_file': return Promise.resolve(null);
        case 'pick_open_file': return Promise.resolve(null);
        case 'reveal_in_explorer': return Promise.resolve(null);
        case 'show_main_window': return Promise.resolve(null);
        case 'quit_app': return Promise.resolve(null);
        case 'ui_settings': return Promise.resolve({ close_to_tray: mockCloseToTray });
        case 'set_close_to_tray':
          mockCloseToTray = !!args.enabled;
          return Promise.resolve(null);
        default: return Promise.reject(new Error('mock 未实现命令：' + name));
      }
    },
    listen: function () { return Promise.resolve(function () {}); },
  };
}

function simulateProgress(total) {
  let i = 0;
  showProgress('正在处理 ' + total + ' 条规则…');
  const timer = setInterval(function () {
    i += 1;
    setProgress((i / total) * 100, '#' + i + '/' + total + ' 示例规则 · 已复制 ' + (i * 4200) + ' 个文件 / ' + (i * 137) + ' MB');
    if (i >= total) clearInterval(timer);
  }, 260);
}

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

(async function boot() {
  await refresh(false);
  if (PARAMS.has('progress')) {
    showProgress('正在处理 3 条规则…');
    setProgress(46, '#2/3 Edge 缓存 · 已复制 12800 个文件 / 512.4 MB');
  }
  if (PARAMS.has('modal')) {
    await openRuleForm(null);
  }
  if (PARAMS.has('settings')) {
    await openSettings();
  }
  if (PARAMS.has('ops')) {
    await openOps();
  }
  if (PARAMS.has('disk')) {
    await openDiskStats();
  }
  if (PARAMS.has('clean')) {
    await openBatchCleanDialog();
  }

  // RAM 盘水位巡检（60s）：级别变化才落事件（后端去抖）；
  // 发现 warn/critical 顺带刷新快照，让概览横幅及时出现。
  setInterval(async function () {
    try {
      const levels = await invoke('water_poll', {});
      if (levels.some(function (w) { return w.level === 'warn' || w.level === 'critical'; })) {
        await refresh(true);
      }
    } catch (e) { /* 巡检失败静默，下轮再试 */ }
  }, 60000);
})();
