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
  return Promise.reject(new Error(t('err.no_bridge')));
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
  // 内置模板（preset_rules 的返回，后端已按"本机存在源目录"过滤）。
  // 还没建成本地规则的模板会作为"未添加"行直接显示在主列表里。
  presets: [],
  // 已勾选但还没建成规则的内置模板（键 = source_template 小写）。
  pickedTpl: new Set(),
  focusId: null,
  filters: { q: '', category: '', onlyEnabled: false, onlyAttention: false },
  busy: false,
  drag: null,
  // 表头点击排序：key=null 表示用后端返回顺序（category, sort_order, id）。
  // 客户端排序只影响展示，不写库 —— 拖拽排序（sort_order）才是持久顺序。
  sort: { key: null, dir: 'asc' },
};

// 分类标签走 i18n（键 cat.<category>）；未收录的分类回退原始值。
function categoryLabel(c) {
  if (!c) return c;
  const label = t('cat.' + c);
  return label === 'cat.' + c ? c : label;
}

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
    return rule.link_type === 'symlink_dir' ? t('badge.symlink') : t('badge.junction');
  }
  if (rule.badge === '❌') return t('badge.unlinked');
  if (rule.badge === '🚫') return t('badge.foreign');
  return t('badge.unknown');
}

function toast(title, message, kind) {
  const box = document.createElement('div');
  box.className = 'toast ' + (kind || '');
  const titleEl = document.createElement('div');
  titleEl.className = 'toast-title';
  titleEl.textContent = title;
  const m = document.createElement('div');
  m.textContent = message || '';
  box.appendChild(titleEl);
  if (message) box.appendChild(m);
  $('toasts').appendChild(box);
  setTimeout(function () { box.remove(); }, kind === 'error' ? 9000 : 4500);
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast(t('common.copied'), text.length > 60 ? text.slice(0, 60) + '…' : text, 'ok');
  } catch (e) {
    toast(t('common.copy_failed'), t('common.copy_manual'), 'warn');
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
    // 内置模板：后端已按"本机是否存在该源目录"过滤，模板源目录不存在的机型自然拿不到条目。
    // 命令不存在（旧后端）时静默降级为"没有模板"，不影响主列表。
    try { state.presets = await invoke('preset_rules', {}); } catch (e) { state.presets = []; }
    // 清掉已经不存在的勾选项
    const ids = new Set(state.snap.rules.map(function (r) { return r.id; }));
    state.picked.forEach(function (id) { if (!ids.has(id)) state.picked.delete(id); });
    pruneTplPicks();
    render();
  } catch (e) {
    toast(t('toast.load_failed'), e.message, 'error');
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
  renderSortIndicators();
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
      btn.textContent = t('banner.clean_now');
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
    sub = t('header.no_root');
  } else {
    const first = ready[0] || roots[0];
    const mark = first.state === 'ready'
      ? t('header.root_ready')
      : tf('header.root_unavailable', { msg: first.message });
    sub = tf('header.root_prefix', { path: first.path, mark: mark });
    if (ready.length && first.message && first.state === 'ready') sub += ' · ' + first.message;
    if (roots.length > 1) sub += tf('header.roots_more', { n: roots.length });
  }
  $('root-summary').textContent = sub;

  const s = state.snap.stats;
  $('stat-summary').textContent =
    tf('stat.summary', { total: s.total, enabled: s.enabled, ok: s.ok, warn: s.warn, plain: s.plain }) +
    (s.other ? ' ❔ ' + s.other : '');
}

function renderFilterOptions() {
  const sel = $('filter-category');
  if (sel.options.length === 0) {
    const all = document.createElement('option');
    all.value = '';
    all.textContent = t('filter.all_categories');
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
  const rows = (state.snap.rules || []).filter(function (r) {
    if (f.category && r.category !== f.category) return false;
    if (f.onlyEnabled && !r.want_enabled) return false;
    if (f.onlyAttention && !r.needs_attention) return false;
    if (q) {
      const hay = (r.name + ' ' + r.source + ' ' + r.source_template + ' ' + r.target + ' ' + r.target_subpath).toLowerCase();
      if (hay.indexOf(q) < 0) return false;
    }
    return true;
  });
  return sortRows(rows);
}

// ---------------------------------------------------------------------------
// 内置模板（还没添加成规则的预设）
//
// 预设库（内置 + 用户外置）本来的唯一入口是「添加规则」弹窗顶部的下拉。
// 这里把"还没建过规则"的模板直接铺成主列表的行（带「未添加」标记）：
// 勾选后点「应用选中」就自动建规则 + 建链，不用再开表单弹窗逐字段填。
// ---------------------------------------------------------------------------

/** 模板身份键：source_template 是路径模板，Windows 路径大小写不敏感。 */
function tplKey(tpl) { return String(tpl || '').toLowerCase(); }

/** 主列表里要显示的模板行：排除已经建过同源规则的，再套用当前的分类 / 搜索筛选。
 *  「只看已开启 / 只看需处理」描述的是规则状态，模板还没有状态 —— 这两项筛选
 *  生效时直接不展示模板，免得用户以为它们是规则。 */
function visibleTemplates() {
  const f = state.filters;
  if (f.onlyEnabled || f.onlyAttention) return [];
  const used = new Set((state.snap.rules || []).map(function (r) { return tplKey(r.source_template); }));
  const q = f.q.trim().toLowerCase();
  return (state.presets || []).filter(function (p) {
    if (used.has(tplKey(p.source_template))) return false;
    if (f.category && p.category !== f.category) return false;
    if (q) {
      const hay = (p.name + ' ' + p.source + ' ' + p.source_template + ' ' + p.target_subpath).toLowerCase();
      if (hay.indexOf(q) < 0) return false;
    }
    return true;
  });
}

/** 模板建规则时默认落到的目标根：与「添加规则」弹窗的默认值一致（第一个根）。 */
function tplTargetRoot() {
  const roots = (state.snap && state.snap.roots) || [];
  return roots[0] || null;
}

/** 模板行的目标路径预览（根 + 模板的默认子路径）。 */
function tplTargetPath(p) {
  const root = tplTargetRoot();
  if (!root) return p.target_subpath;
  return String(root.path).replace(/[\\\/]+$/, '') + '\\' + p.target_subpath;
}

/** 模板 → add_rule 的表单（字段名必须是 snake_case，契约见 view.rs 的 RuleForm）。 */
function templateForm(p, rootId) {
  return {
    name: p.name,
    category: p.category,
    source_template: p.source_template,
    target_root_id: rootId,
    target_subpath: p.target_subpath,
    link_type: 'junction',
    want_enabled: true,
    rebuild_on_boot: true,
    notes: p.note || null,
    excludes: [],
    auto_clean_policy: '',
  };
}

/** 勾选状态清理：模板从列表消失（后端不再返回 / 已建成规则）时同步取消勾选。
 *  只看模板自身是否还存在，不受当前筛选影响 —— 与规则勾选的保留语义一致。 */
function pruneTplPicks() {
  if (!state.pickedTpl.size) return;
  const used = new Set((state.snap.rules || []).map(function (r) { return tplKey(r.source_template); }));
  const alive = new Set(
    (state.presets || [])
      .filter(function (p) { return !used.has(tplKey(p.source_template)); })
      .map(function (p) { return tplKey(p.source_template); })
  );
  state.pickedTpl.forEach(function (k) { if (!alive.has(k)) state.pickedTpl.delete(k); });
}

/** 把当前勾选的模板逐个建成规则（add_rule），返回新建规则的 id 列表。
 *  建失败的单独弹 toast 并跳过，不拖累其余模板。 */
async function addPickedTemplates() {
  const picked = (state.presets || []).filter(function (p) { return state.pickedTpl.has(tplKey(p.source_template)); });
  if (!picked.length) return [];
  const root = tplTargetRoot();
  if (!root) {
    toast(t('rule.no_root_title'), t('rule.no_root_hint'), 'warn');
    return [];
  }
  const ids = [];
  for (const p of picked) {
    try {
      const id = await invoke('add_rule', { form: templateForm(p, root.id) });
      ids.push(id);
    } catch (e) {
      toast(tf('tpl.add_failed', { name: p.name }), e.message, 'error');
    }
  }
  return ids;
}

/** 表头点击排序：客户端、不写库（持久顺序靠拖拽 sort_order）。
 *  key=null 时原样返回（保持后端顺序）。badge 按"好→坏"排，旗标按
 *  期望开启优先、再按开机重建。 */
function sortRows(rows) {
  const s = state.sort;
  if (!s.key) return rows;
  const dir = s.dir === 'desc' ? -1 : 1;
  const badgeRank = { '✅': 0, '⚠️': 1, '❌': 2, '🚫': 3 };
  const val = function (r) {
    switch (s.key) {
      case 'badge': return badgeRank[r.badge] !== undefined ? badgeRank[r.badge] : 9;
      case 'name': return r.name || '';
      case 'category': return r.category || '';
      case 'source': return r.source || '';
      case 'target': return r.target || '';
      case 'flag': return (r.want_enabled ? 0 : 1) + ',' + (r.rebuild_on_boot ? 0 : 1);
      default: return '';
    }
  };
  return rows.slice().sort(function (a, b) {
    const va = val(a), vb = val(b);
    if (va < vb) return -dir;
    if (va > vb) return dir;
    return 0;
  });
}

/** 在当前排序列表头显示 ↑/↓，其余列清掉。表头是静态 HTML，每次 render 重画指示。 */
function renderSortIndicators() {
  document.querySelectorAll('th[data-sort]').forEach(function (th) {
    const old = th.querySelector('.sort-ind');
    if (old) old.remove();
    if (th.dataset.sort !== state.sort.key) return;
    const ind = document.createElement('span');
    ind.className = 'sort-ind';
    ind.textContent = state.sort.dir === 'desc' ? ' ↓' : ' ↑';
    th.appendChild(ind);
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
  const tpls = visibleTemplates();
  const tbody = $('rows');
  tbody.textContent = '';

  $('filter-summary').textContent =
    tf('filter.summary', { shown: rows.length, total: (state.snap.rules || []).length }) +
    (totalPicked() ? tf('filter.summary_picked', { n: totalPicked() }) : '');
  syncSelectAll(rows);

  const empty = $('empty');
  if (!rows.length && !tpls.length) {
    empty.classList.remove('hidden');
    empty.textContent = state.snap.rules.length
      ? t('empty.no_match')
      : t('empty.no_rules');
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
    handle.title = t('row.drag_tip');
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
    on.title = r.want_enabled ? t('row.expect_on') : t('row.expect_off');
    flags.appendChild(on);
    const bt = document.createElement('span');
    bt.className = 'toggle ' + (r.rebuild_on_boot ? 'on' : 'off');
    bt.textContent = r.rebuild_on_boot ? '⟳' : '·';
    bt.title = r.rebuild_on_boot ? t('row.rebuild_on') : t('row.rebuild_off');
    flags.appendChild(bt);
    tdFlags.appendChild(flags);
    tr.appendChild(tdFlags);

    // 行内操作
    const tdAct = document.createElement('td');
    const wrap = document.createElement('div');
    wrap.className = 'row-actions';
    [
      { act: 'apply', label: t('row.act.apply'), primary: true, when: r.badge !== '✅' },
      { act: 'restore', label: t('row.act.restore'), when: r.badge === '✅' || r.badge === '⚠️' },
      { act: 'relink', label: t('row.act.relink'), when: r.badge === '⚠️' || r.badge === '❌' },
      { act: 'clean', label: t('row.act.clean'), when: true },
      { act: 'edit', label: t('row.act.edit'), when: true },
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
  // 内置模板行排在真实规则之后：它们只是"可一键添加的预设"（.tpl-row 淡背景 +
  // 「未添加」徽标），与库里已有规则视觉上分开。
  tpls.forEach(function (p) { frag.appendChild(templateRow(p)); });
  tbody.appendChild(frag);
}

/** 单条内置模板行。勾选后随「应用选中」一并建规则 + 建链，全程不开表单。 */
function templateRow(p) {
  const key = tplKey(p.source_template);
  const tr = document.createElement('tr');
  tr.className = 'tpl-row';
  tr.dataset.tpl = p.source_template;

  // 勾选框（模板没有 id，用 source_template 当键）
  const tdCheck = document.createElement('td');
  const cb = document.createElement('input');
  cb.type = 'checkbox';
  cb.checked = state.pickedTpl.has(key);
  cb.dataset.act = 'pick-tpl';
  cb.dataset.tpl = p.source_template;
  tdCheck.appendChild(cb);
  tr.appendChild(tdCheck);

  // 状态徽标：明确"还没添加成规则"
  const tdBadge = document.createElement('td');
  const badge = document.createElement('span');
  badge.className = 'badge tpl';
  badge.textContent = t('tpl.badge');
  badge.title = t('tpl.badge_tip');
  tdBadge.appendChild(badge);
  tr.appendChild(tdBadge);

  // 名称 + 提示（有 note 用 note，没有就给默认的"勾选即应用"说明）
  const tdName = document.createElement('td');
  const nm = document.createElement('div');
  nm.className = 'cell-name';
  nm.textContent = p.name;
  tdName.appendChild(nm);
  const note = document.createElement('div');
  note.className = 'row-note';
  note.textContent = p.note ? ('⚠ ' + p.note) : t('tpl.hint');
  tdName.appendChild(note);
  tr.appendChild(tdName);

  // 分类
  const tdCat = document.createElement('td');
  const pill = document.createElement('span');
  pill.className = 'pill';
  pill.textContent = categoryLabel(p.category);
  tdCat.appendChild(pill);
  tr.appendChild(tdCat);

  // 源路径（展开后的路径，title 显示原始模板）
  const tdSrc = document.createElement('td');
  const src = document.createElement('div');
  src.className = 'cell-path';
  src.textContent = p.source;
  src.title = p.source_template;
  tdSrc.appendChild(src);
  tr.appendChild(tdSrc);

  // 目标（预计落在第一个目标根下）
  const tdTgt = document.createElement('td');
  const tgt = document.createElement('div');
  tgt.className = 'cell-path root';
  tgt.textContent = tplTargetPath(p);
  tdTgt.appendChild(tgt);
  tr.appendChild(tdTgt);

  // 旗标：模板还没有状态，按"新建即期望开启 + 开机重建"灰显
  const tdFlags = document.createElement('td');
  const flags = document.createElement('div');
  flags.className = 'row-flags';
  [['●', t('row.expect_on')], ['⟳', t('row.rebuild_on')]].forEach(function (f2) {
    const s = document.createElement('span');
    s.className = 'toggle tpl';
    s.textContent = f2[0];
    s.title = f2[1];
    flags.appendChild(s);
  });
  tdFlags.appendChild(flags);
  tr.appendChild(tdFlags);

  // 行操作：一键"添加并应用"（与勾选后按「应用选中」等价，只是省一步）
  const tdAct = document.createElement('td');
  const wrap = document.createElement('div');
  wrap.className = 'row-actions';
  const btn = document.createElement('button');
  btn.className = 'mini primary';
  btn.textContent = t('tpl.add_apply');
  btn.dataset.act = 'tpl-apply';
  btn.dataset.tpl = p.source_template;
  wrap.appendChild(btn);
  tdAct.appendChild(wrap);
  tr.appendChild(tdAct);

  return tr;
}

function renderDetail() {
  const host = $('detail');
  const rule = (state.snap.rules || []).find(function (r) { return r.id === state.focusId; });
  if (!rule) {
    host.classList.add('hidden');
    host.textContent = '';
    return;
  }
  host.classList.remove('hidden');
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
  if (rule.want_enabled) chips.appendChild(chip(t('detail.chip.want_enabled')));
  if (rule.rebuild_on_boot) chips.appendChild(chip(t('detail.chip.rebuild')));
  if (rule.needs_elevation) chips.appendChild(chip(t('detail.chip.needs_elevation')));
  if (rule.has_acl) chips.appendChild(chip(t('detail.chip.has_acl')));
  if (rule.last_files) chips.appendChild(chip(tf('detail.chip.files', { n: rule.last_files, size: humanBytes(rule.last_bytes || 0) })));
  head.appendChild(chips);
  host.appendChild(head);

  const grid = document.createElement('div');
  grid.className = 'detail-grid';
  addRow(grid, t('detail.key.status'), rule.label, true);
  addRow(grid, t('detail.key.source_template'), rule.source_template);
  addRow(grid, t('detail.key.expanded'), rule.source);
  addRow(grid, t('detail.key.target'), rule.target);
  addRow(grid, t('detail.key.link_type'), rule.link_type === 'symlink_dir' ? t('detail.link.symlink') : t('detail.link.junction'));
  if (rule.notes) addRow(grid, t('detail.key.notes'), rule.notes, true);
  if (rule.excludes && rule.excludes.length) {
    addRow(grid, t('detail.key.excludes'), rule.excludes.join(t('list.sep')), true);
  }
  if (rule.auto_clean_policy) {
    addRow(grid, t('detail.key.auto_clean'), describeAutoPolicy(rule.auto_clean_policy), true);
  }
  if (rule.unknown_vars && rule.unknown_vars.length) {
    addRow(grid, t('detail.key.unknown_vars'), rule.unknown_vars.join(t('list.sep')), true);
  }
  (rule.warnings || []).forEach(function (w, i) {
    addRow(grid, i === 0 ? t('detail.key.warning') : '', w, true);
  });
  if (rule.applied_at) addRow(grid, t('detail.key.applied_at'), rule.applied_at, true);
  host.appendChild(grid);

  const actions = document.createElement('div');
  actions.className = 'detail-actions';
  host.appendChild(actions);
  actions.appendChild(mkBtn(t('detail.act.apply'), 'apply', rule.id, true));
  if (rule.needs_elevation && rule.badge !== '✅') {
    actions.appendChild(mkBtn(t('detail.act.apply_elevated'), 'apply-elevated', rule.id));
  }
  actions.appendChild(mkBtn(t('detail.act.restore'), 'restore', rule.id));
  actions.appendChild(mkBtn(t('detail.act.relink'), 'relink', rule.id));
  actions.appendChild(mkBtn(t('detail.act.locks'), 'locks', rule.id));
  actions.appendChild(mkBtn(t('detail.act.clean'), 'clean', rule.id));
  actions.appendChild(mkBtn(t('detail.act.copy_source'), 'copy-source', rule.id));
  actions.appendChild(mkBtn(t('detail.act.reveal'), 'reveal', rule.id));
  actions.appendChild(mkBtn(t('detail.act.edit'), 'edit', rule.id));
}

function describeAutoPolicy(p) {
  if (p === 'empty') return t('detail.auto.empty');
  const m = /older:(\d+)/.exec(p);
  if (m) {
    const secs = Number(m[1]);
    if (secs % 86400 === 0) return tf('detail.auto.days', { n: secs / 86400 });
    if (secs % 3600 === 0) return tf('detail.auto.hours', { n: secs / 3600 });
    return tf('detail.auto.secs', { n: secs });
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
    (opts.lines || []).forEach(function (line) {
      const p = document.createElement('div');
      p.className = line.kind ? 'hint ' + line.kind : 'field';
      p.textContent = line.text;
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

/** 勾选总数：规则 + 还没添加成规则的内置模板。 */
function totalPicked() { return state.picked.size + state.pickedTpl.size; }

function requirePicked(what) {
  if (!state.picked.size) {
    toast(t('toast.no_pick'), tf('toast.no_pick_hint', { what: what }), 'warn');
    return false;
  }
  return true;
}

/** 「应用选中」专用：规则和内置模板都算数（模板会先建成规则再一起建链）。
 *  其余批量操作（恢复 / 开关 / 删除 / …）只对已有规则有意义，仍走 requirePicked。 */
function requireAnyPick(what) {
  if (!totalPicked()) {
    toast(t('toast.no_pick'), tf('toast.no_pick_hint', { what: what }), 'warn');
    return false;
  }
  return true;
}

/** 勾选的模板先建规则，再和已勾选的规则一起交回给调用方建链。 */
async function applyPickedAll() {
  const created = await addPickedTemplates();
  const ids = pickedIds().concat(created);
  if (!ids.length) return;
  await runApply(ids, false);
}

const OPTS_NORMAL = { dry_run: false, allow_merge: false, skip_lock_check: false, sha256: false };

/** 走提权 worker（会弹一次 UAC）。 */
async function runElevated(kind, ids) {
  if (!ids.length) return;
  state.busy = true;
  showProgress(t('act.elev_request'));
  try {
    const out = await invoke('run_elevated_job', { kind: kind, ids: ids, opts: OPTS_NORMAL });
    hideProgress();
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    out.forEach(function (o) {
      if (!o.ok) toast(tf('act.fail_item', { id: o.rule_id, name: o.name }), o.message, 'error');
    });
    toast(
      kind === 'apply' ? t('act.elev_apply_done') : t('act.elev_restore_done'),
      tf('act.summary', { ok: ok }) + (failed ? tf('act.summary_failed', { n: failed }) : ''),
      failed ? 'warn' : 'ok'
    );
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast(t('act.elev_failed'), e.message, 'error');
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
        title: t('act.elev_needed_title'),
        lines: [
          { text: tf('act.elev_needed_line1', { total: ids.length, n: needElev.length }) },
          {
            text: t('act.elev_needed_line2') +
              (others ? tf('act.elev_needed_line2_rest', { n: others }) : ''),
            kind: '',
          },
        ],
        actions: [
          { label: t('act.elev_run'), kind: 'primary', value: 'elevate' },
          { label: t('act.elev_partial'), value: 'partial' },
          { label: t('common.cancel'), value: 'cancel' },
        ],
      });
      if (choice !== 'elevate' && choice !== 'partial') return;
      if (choice === 'elevate') return runElevated('apply', ids);
      if (choice === 'partial') {
        const rest = ids.filter(function (id) { return needElev.indexOf(id) < 0; });
        if (!rest.length) {
          toast(t('act.no_handle'), t('act.all_need_elev'), 'warn');
          return;
        }
        ids = rest;
      }
    }
  }

  state.busy = true;
  showProgress((dryRun ? t('act.dry_prefix') : '') + tf('act.processing', { n: ids.length }));
  try {
    const out = await invoke('apply_rules', { ids: ids, opts: { dry_run: !!dryRun, allow_merge: false, skip_lock_check: false, sha256: false } });
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    hideProgress();
    out.forEach(function (o) {
      if (!o.ok) toast(tf('act.fail_item', { id: o.rule_id, name: o.name }), o.message, 'error');
    });
    toast(dryRun ? t('act.dry_done') : t('act.apply_done'), tf('act.summary', { ok: ok }) + (failed ? tf('act.summary_failed', { n: failed }) : ''), failed ? 'warn' : 'ok');
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast(t('act.apply_failed'), e.message, 'error');
  } finally {
    state.busy = false;
  }
}

async function runRestore(ids) {
  if (!ids.length) return;
  state.busy = true;
  showProgress(tf('act.restoring', { n: ids.length }));
  try {
    const out = await invoke('restore_rules', { ids: ids, opts: { dry_run: false, allow_merge: true, skip_lock_check: true, sha256: false } });
    const ok = out.filter(function (o) { return o.ok; }).length;
    const failed = out.length - ok;
    hideProgress();
    out.forEach(function (o) {
      if (!o.ok) toast(tf('act.fail_item', { id: o.rule_id, name: o.name }), o.message, 'error');
    });
    toast(t('act.restore_done'), tf('act.summary', { ok: ok }) + (failed ? tf('act.summary_failed', { n: failed }) : ''), failed ? 'warn' : 'ok');
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast(t('act.restore_failed'), e.message, 'error');
  } finally {
    state.busy = false;
  }
}

/** 「恢复全部」panic 按钮：把所有**处于链接状态**的规则一键还原。
 *  场景：卸载软件 / 卸载 RAM 盘 / 出问题排查 —— 需要快回到"没有重定向"的世界。
 *  只认 ✅/⚠️（链接确实存在）的规则；plain_dir 本来就是真实目录，没什么可恢复。
 *  有提权规则时整批走提权进程（panic 场景少一次选择）。 */
async function restoreAllRules() {
  const linked = state.snap.rules.filter(function (r) {
    return r.badge === '✅' || r.badge === '⚠️';
  });
  if (!linked.length) {
    toast(t('act.all.nothing'), t('act.all.nothing_hint'), 'warn');
    return;
  }
  const needElev = linked.filter(function (r) { return r.needs_elevation; }).length;
  const ids = linked.map(function (r) { return r.id; });

  const body = document.createElement('div');
  const p1 = document.createElement('div');
  p1.className = 'field';
  p1.textContent = tf('act.all.intro', { n: linked.length });
  body.appendChild(p1);
  const p2 = document.createElement('div');
  p2.className = 'field';
  const h1 = document.createElement('div');
  h1.className = 'hint warn';
  h1.textContent = t('act.all.warn');
  p2.appendChild(h1);
  const h2 = document.createElement('div');
  h2.className = 'hint';
  h2.textContent = needElev
    ? tf('act.all.elev', { n: needElev })
    : t('act.all.keep_calm');
  p2.appendChild(h2);
  body.appendChild(p2);
  const list = document.createElement('div');
  list.className = 'field';
  list.textContent = tf('act.all.involved', { names: linked.map(function (r) { return r.name; }).join(t('list.sep')) });
  body.appendChild(list);

  openModal({
    title: t('act.all.title'),
    body: body,
    actions: [
      { label: t('common.cancel'), onClick: closeModal },
      {
        label: t('act.all.confirm'), kind: 'danger',
        onClick: async function () {
          closeModal();
          if (needElev) return runElevated('restore', ids);
          return runRestore(ids);
        },
      },
    ],
  });
}

async function doRelink(id) {
  try {
    const o = await invoke('relink_rule', { id: id });
    toast(o.ok ? t('act.relink_ok') : t('act.relink_fail'), o.message, o.ok ? 'ok' : 'error');
    await refresh(true);
  } catch (e) {
    toast(t('act.relink_failed'), e.message, 'error');
  }
}

async function doLocks(id) {
  toast(t('act.locks.scanning'), t('act.locks.scanning_hint'), '');
  try {
    const rep = await invoke('check_locks', { id: id });
    const body = document.createElement('div');
    if (rep.locked_files.length === 0 && rep.denied_files.length === 0) {
      const p = document.createElement('div');
      p.className = 'field';
      const hint = document.createElement('div');
      hint.className = 'hint ok';
      hint.textContent = tf('act.locks.clear', { n: rep.scanned, ms: rep.elapsed_ms });
      p.appendChild(hint);
      body.appendChild(p);
    } else {
      const p = document.createElement('div');
      p.className = 'field';
      const hint = document.createElement('div');
      hint.className = 'hint warn';
      hint.textContent = tf('act.locks.busy', { n: rep.locked_files.length }) + (rep.truncated ? t('act.locks.truncated') : '') +
        (rep.denied_files.length ? tf('act.locks.denied', { n: rep.denied_files.length }) : '');
      p.appendChild(hint);
      body.appendChild(p);

      if (rep.blockers.length) {
        const bl = document.createElement('div');
        bl.className = 'field';
        const lbl = document.createElement('label');
        lbl.textContent = t('act.locks.blockers');
        bl.appendChild(lbl);
        rep.blockers.forEach(function (b) {
          const line = document.createElement('div');
          line.className = 'hint err';
          line.textContent = '⛔ ' + b.app + '（PID ' + b.pid + '）' + (b.is_service ? t('act.locks.service') : '');
          bl.appendChild(line);
        });
        body.appendChild(bl);
      }
      const files = document.createElement('div');
      files.className = 'field';
      const fl = document.createElement('label');
      fl.textContent = t('act.locks.files');
      files.appendChild(fl);
      rep.locked_files.slice(0, 10).forEach(function (f) {
        const line = document.createElement('div');
        line.className = 'detail-val';
        line.textContent = f;
        files.appendChild(line);
      });
      body.appendChild(files);
    }
    openModal({ title: t('act.locks.title'), body: body, actions: [{ label: t('common.close'), onClick: closeModal }] });
    await refresh(true);
  } catch (e) {
    toast(t('act.locks.failed'), e.message, 'error');
  }
}

async function doBoot() {
  state.busy = true;
  showProgress(t('act.boot.running'));
  try {
    const r = await invoke('boot_rebuild');
    hideProgress();
    const lines = [r.roots.map(function (x) {
      return (x.ready ? '✅ ' : '❌ ') + x.path + (x.ready ? tf('act.boot.waited', { ms: x.waited_ms }) : '：' + (x.error || t('act.boot.not_ready')));
    }).join('\n'), r.one_line()].join('\n\n');
    toast(t('act.boot.done'), r.one_line(), r.has_problem() ? 'warn' : 'ok');
    if (r.warnings.length || r.failed.length) {
      const body = document.createElement('div');
      r.warnings.concat(r.failed.map(function (f) { return '#' + f[0] + ' ' + f[1]; })).forEach(function (w) {
        const d = document.createElement('div');
        d.className = 'hint warn';
        d.textContent = '⚠ ' + w;
        body.appendChild(d);
      });
      openModal({ title: t('act.boot.attention'), body: body, actions: [{ label: t('act.boot.got_it'), kind: 'primary', onClick: closeModal }] });
    }
    void lines;
    await refresh(true);
  } catch (e) {
    hideProgress();
    toast(t('act.boot.failed'), e.message, 'error');
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
  strip.textContent = t('ops.savings_loading');
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
  btnEvents.textContent = t('ops.tab_events');
  const btnOps = document.createElement('button');
  btnOps.className = 'btn';
  btnOps.textContent = t('ops.tab_ops');
  tabs.appendChild(btnEvents);
  tabs.appendChild(btnOps);

  async function load() {
    btnEvents.className = 'btn' + (mode === 'events' ? ' active' : '');
    btnOps.className = 'btn' + (mode === 'ops' ? ' active' : '');
    list.textContent = '';
    try {
      if (mode === 'events') {
        const evs = await invoke('recent_events', { limit: 100 });
        if (!evs.length) { list.textContent = t('ops.no_events'); return; }
        evs.forEach(function (e) {
          list.appendChild(logRow(e.at, e.kind, e.message, e.level));
        });
      } else {
        const ops = await invoke('recent_ops', { limit: 100 });
        if (!ops.length) { list.textContent = t('ops.no_ops'); return; }
        ops.forEach(function (o) {
          const msg = '#' + o.id + ' rule=' + (o.rule_id === null ? '-' : o.rule_id) +
            ' files=' + (o.file_count || 0) + ' ' + (o.source_path || '') +
            (o.error ? '  ERR: ' + o.error : '');
          list.appendChild(logRow(o.started_at, o.kind + '/' + o.phase, msg, o.phase === 'done' ? 'info' : 'warn'));
        });
      }
    } catch (e) {
      list.textContent = tf('ops.load_failed', { msg: e.message });
    }
  }
  btnEvents.addEventListener('click', function () { mode = 'events'; load(); });
  btnOps.addEventListener('click', function () { mode = 'ops'; load(); });
  await load();
  openModal({ title: t('ops.title'), body: body, wide: true, actions: [{ label: t('common.close'), onClick: closeModal }] });
}

/** 效益统计三卡：当前重定向 / 累计搬运 / 累计清理。
 * 只报可解释的数字，不做"节省 SSD 寿命"外推 —— 后端同此约定（usecase/stats.rs）。 */
async function loadSavings(strip) {
  try {
    const v = await invoke('savings');
    strip.textContent = '';
    strip.appendChild(savingCard(t('ops.card.redirected_title'), v.redirected_text,
      tf('ops.card.redirected_sub', { rules: v.redirected_rules, files: v.redirected_files })));
    strip.appendChild(savingCard(t('ops.card.moved_title'), v.moved_text,
      tf('ops.card.moved_sub', { n: v.moved_files })));
    strip.appendChild(savingCard(t('ops.card.cleaned_title'), v.cleaned_text,
      tf('ops.card.cleaned_sub', { n: v.cleaned_files })));
  } catch (e) {
    // 统计只是附属信息，拿不到就撤掉这块，别挡住下面的日志。
    strip.remove();
  }
}

function savingCard(title, value, sub) {
  const card = document.createElement('div');
  card.className = 'saving-card';
  const titleEl = document.createElement('div');
  titleEl.className = 'saving-title';
  titleEl.textContent = title;
  const v = document.createElement('div');
  v.className = 'saving-value';
  v.textContent = value;
  const s = document.createElement('div');
  s.className = 'saving-sub';
  s.textContent = sub;
  card.appendChild(titleEl);
  card.appendChild(v);
  card.appendChild(s);
  return card;
}

function logRow(time, kind, msg, level) {
  const row = document.createElement('div');
  row.className = 'log-item ' + (level === 'error' ? 'error' : level === 'warn' ? 'warn' : '');
  const timeEl = document.createElement('div');
  timeEl.className = 'log-time';
  timeEl.textContent = time;
  const k = document.createElement('div');
  k.className = 'log-kind';
  k.textContent = kind;
  const m = document.createElement('div');
  m.className = 'log-msg';
  m.textContent = msg;
  row.appendChild(timeEl);
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
    [t('settings.version'), snap.version + tf('settings.schema_tag', { v: snap.schema_version })],
    [t('settings.data_dir'), snap.data_dir],
    [t('settings.db'), snap.db_path],
  ].forEach(function (kv) {
    const d = document.createElement('div');
    d.className = 'hint';
    d.textContent = tf('settings.kv', { k: kv[0], v: kv[1] });
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
  closeLabel.textContent = t('settings.close_when');
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
  closeSpan.textContent = t('settings.close_to_tray');
  closeInline.appendChild(closeSpan);
  closeCheck.addEventListener('change', async function () {
    try {
      await invoke('set_close_to_tray', { enabled: closeCheck.checked });
      toast(t('common.saved'), closeCheck.checked ? t('settings.close_to_tray_on') : t('settings.close_to_tray_off'), 'ok');
    } catch (e) {
      toast(t('common.save_failed'), e.message, 'error');
    }
  });
  closeField.appendChild(closeInline);
  const closeHint = document.createElement('div');
  closeHint.className = 'hint';
  closeHint.textContent = t('settings.close_hint');
  closeField.appendChild(closeHint);
  body.appendChild(closeField);

  // ---- 定时（低峰期）自动清理 ----
  // 例行维护：每天到点把所有配了预授权策略（auto_clean_policy）的规则清一遍，
  // 让缓存别攒到触发水位。与水位自动清理共用同一套规则级授权，默认关闭。
  const schedField = document.createElement('div');
  schedField.className = 'field';
  const schedLabel = document.createElement('label');
  schedLabel.textContent = t('settings.sched');
  schedField.appendChild(schedLabel);
  let sched = { enabled: false, time: '03:00' };
  try { sched = await invoke('scheduled_clean_settings', {}); } catch (e) { /* 默认值 */ }
  const schedInline = document.createElement('label');
  schedInline.className = 'inline-check';
  const schedCheck = document.createElement('input');
  schedCheck.type = 'checkbox';
  schedCheck.checked = sched.enabled;
  schedInline.appendChild(schedCheck);
  const schedSpan = document.createElement('span');
  schedSpan.textContent = t('settings.sched_on');
  schedInline.appendChild(schedSpan);
  const schedTime = document.createElement('input');
  schedTime.type = 'time';
  schedTime.value = sched.time || '03:00';
  schedInline.appendChild(schedTime);
  async function saveSched() {
    try {
      await invoke('set_scheduled_clean', { enabled: schedCheck.checked, time: schedTime.value || '03:00' });
      toast(t('common.saved'), schedCheck.checked
        ? tf('settings.sched_saved_on', { time: schedTime.value || '03:00' })
        : t('settings.sched_saved_off'), 'ok');
    } catch (e) {
      toast(t('common.save_failed'), e.message, 'error');
    }
  }
  schedCheck.addEventListener('change', saveSched);
  schedTime.addEventListener('change', saveSched);
  schedField.appendChild(schedInline);
  const schedHint = document.createElement('div');
  schedHint.className = 'hint';
  schedHint.textContent = t('settings.sched_hint');
  schedField.appendChild(schedHint);
  body.appendChild(schedField);

  const rootField = document.createElement('div');
  rootField.className = 'field';
  const rootLabel = document.createElement('label');
  rootLabel.textContent = t('settings.root');
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
    b.textContent = r.path + ' · ' + (r.state === 'ready' ? t('header.root_ready') : r.message) +
      tf('settings.root_wait', { ms: r.wait_timeout_ms }) + (r.is_ramdisk ? t('settings.root_ramdisk') : '');
    const del = document.createElement('button');
    del.className = 'mini';
    del.textContent = t('common.delete');
    del.addEventListener('click', async function () {
      try {
        await invoke('delete_root', { id: r.id });
        closeModal();
        toast(t('settings.root_deleted'), '#' + r.id + ' ' + r.path, 'ok');
        await refresh(true);
      } catch (e) {
        toast(t('settings.root_delete_failed'), e.message, 'error');
      }
    });
    row.appendChild(a);
    row.appendChild(b);
    row.appendChild(del);
    rootList.appendChild(row);
  });
  if (!snap.roots || !snap.roots.length) rootList.textContent = t('settings.no_root');
  rootField.appendChild(rootList);
  body.appendChild(rootField);

  const addField = document.createElement('div');
  addField.className = 'field';
  const addLabel = document.createElement('label');
  addLabel.textContent = t('settings.add_root');
  addField.appendChild(addLabel);
  const row = document.createElement('div');
  row.className = 'field-row';
  const nameInput = mkInput(t('settings.name_ph'));
  const pathInput = mkInput(t('settings.path_ph'));
  const pick = mkPickButton(t('common.browse'), async function () {
    const p = await invoke('pick_folder', { title: t('settings.pick_title'), defaultPath: pathInput.value });
    if (p) pathInput.value = p;
  });
  const addBtn = document.createElement('button');
  addBtn.className = 'btn primary';
  addBtn.textContent = t('common.add');
  addBtn.addEventListener('click', async function () {
    try {
      await invoke('add_root', {
        label: nameInput.value || 'RAMDisk',
        path: pathInput.value,
        isRamdisk: true,
        waitTimeoutMs: 90000,
      });
      closeModal();
      toast(t('settings.root_added'), pathInput.value, 'ok');
      await refresh(true);
    } catch (e) {
      toast(t('settings.root_add_failed'), e.message, 'error');
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
  addHint.textContent = t('settings.add_hint');
  addField.appendChild(addHint);
  body.appendChild(addField);

  openModal({
    title: t('settings.title'),
    body: body,
    wide: true,
    actions: [
      { label: t('settings.export'), onClick: function () { closeModal(); doExport(); } },
      { label: t('settings.import'), onClick: function () { closeModal(); doImport(); } },
      { label: t('common.close'), kind: 'primary', onClick: closeModal },
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
  label.textContent = t('auto.label');
  field.appendChild(label);

  const box = document.createElement('div');
  box.className = 'log-list';
  const row = document.createElement('div');
  row.className = 'log-item';
  const kind = document.createElement('div');
  kind.className = 'log-kind';
  kind.textContent = !auto ? t('auto.unknown') : auto.registered ? t('auto.enabled') : t('auto.disabled');
  const msg = document.createElement('div');
  msg.className = 'log-msg';
  msg.textContent = auto ? auto.message : t('auto.read_failed');
  row.appendChild(kind);
  row.appendChild(msg);
  box.appendChild(row);
  field.appendChild(box);

  const actions = document.createElement('div');
  actions.className = 'detail-actions';
  actions.appendChild(
    mkSmallBtn(t('auto.enable'), async function () {
      try {
        const cmd = await invoke('autostart_enable');
        toast(t('auto.enabled_toast'), cmd, 'ok');
        closeModal();
        await openSettings();
      } catch (e) {
        toast(t('auto.enable_failed'), e.message, 'error');
      }
    }, 'primary')
  );
  actions.appendChild(
    mkSmallBtn(t('auto.disable'), async function () {
      try {
        const msg2 = await invoke('autostart_disable');
        toast(t('auto.disabled_toast'), msg2, 'ok');
        closeModal();
        await openSettings();
      } catch (e) {
        toast(t('auto.disable_failed'), e.message, 'error');
      }
    })
  );
  actions.appendChild(
    mkSmallBtn(t('auto.run_now'), async function () {
      try {
        const msg2 = await invoke('autostart_run_now');
        toast(t('auto.triggered'), msg2, 'ok');
        setTimeout(function () { refresh(true); }, 3000);
      } catch (e) {
        toast(t('auto.trigger_failed'), e.message, 'error');
      }
    })
  );
  field.appendChild(actions);

  const hint = document.createElement('div');
  hint.className = 'hint';
  if (auto) {
    const parts = [
      tf('auto.p1', { name: auto.task_name }),
      tf('auto.p2', { cmd: (auto.command || auto.recommended_exe + ' boot-rebuild') }),
      tf('auto.p3', { id: auto.elevated ? t('auto.admin') : t('auto.normal') }),
    ];
    if (auto.extra_args) {
      parts.push(tf('auto.p4', { args: auto.extra_args }));
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
    const path = await invoke('pick_save_file', { title: t('io.export_title'), defaultName: 'tempdirector-config.json' });
    if (!path) return;
    const r = await invoke('export_config', { path: path });
    toast(t('io.exported'), tf('io.exported_sub', { roots: r[0], rules: r[1], path: path }), 'ok');
  } catch (e) {
    toast(t('io.export_failed'), e.message, 'error');
  }
}

async function doImport() {
  try {
    const path = await invoke('pick_open_file', { title: t('io.import_title') });
    if (!path) return;
    const rep = await invoke('import_config', { path: path, ramRoot: null, dryRun: false });
    const msg = tf('io.import_msg', { roots: rep.roots_created, rules: rep.rules_created, skipped: rep.rules_skipped }) +
      (rep.errors.length ? tf('io.import_errors', { n: rep.errors.length }) : '');
    toast(t('io.imported'), msg, rep.errors.length ? 'warn' : 'ok');
    if (rep.errors.length) {
      const body = document.createElement('div');
      rep.errors.forEach(function (e) {
        const d = document.createElement('div');
        d.className = 'hint warn';
        d.textContent = '⚠ ' + e;
        body.appendChild(d);
      });
      openModal({ title: t('io.import_problems'), body: body, actions: [{ label: t('act.boot.got_it'), kind: 'primary', onClick: closeModal }] });
    }
    await refresh(true);
  } catch (e) {
    toast(t('io.import_failed'), e.message, 'error');
  }
}

// ---------------------------------------------------------------------------
// 添加 / 编辑规则
// ---------------------------------------------------------------------------

async function openRuleForm(id) {
  const snap = state.snap;
  if (!snap.roots.length) {
    toast(t('rule.no_root_title'), t('rule.no_root_hint'), 'warn');
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
      placeholder.textContent = t('rule.preset_ph');
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
      const presetWrap = mk(t('rule.preset'), presetSelect, t('rule.preset_hint'));
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
  nameInput = mkInput(t('rule.name_ph'));
  nameInput.value = rule ? rule.name : '';
  body.appendChild(mk(t('rule.name'), nameInput).field);

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
  body.appendChild(mk(t('rule.category'), catSelect).field);

  // 源路径 + 浏览 + 实时校验
  const srcField = document.createElement('div');
  srcField.className = 'field';
  const srcLabel = document.createElement('label');
  srcLabel.textContent = t('rule.source');
  srcField.appendChild(srcLabel);
  const srcRow = document.createElement('div');
  srcRow.className = 'field-row';
  srcInput = mkInput('%LOCALAPPDATA%\\Google\\Chrome\\User Data\\Default\\Cache');
  srcInput.value = rule ? rule.source_template : '';
  srcInput.style.flex = '1';
  const srcPick = mkPickButton(t('rule.pick_dir'), async function () {
    const p = await invoke('pick_folder', { title: t('rule.pick_dir_title'), defaultPath: srcInput.value });
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
    o.textContent = tf('rule.root_fmt', { id: r.id, label: r.label, path: r.path }) + (r.state === 'ready' ? '' : t('rule.root_unavailable_tag'));
    rootSelect.appendChild(o);
  });
  rootSelect.value = String(rule ? rule.target_root_id : snap.roots[0].id);

  subInput = mkInput(t('rule.sub_ph'));
  subInput.value = rule ? rule.target_subpath : '';

  const targetRow = document.createElement('div');
  targetRow.className = 'field-row';
  const rootFieldWrap = document.createElement('div');
  rootFieldWrap.className = 'field';
  const rootL = document.createElement('label');
  rootL.textContent = t('settings.root');
  rootFieldWrap.appendChild(rootL);
  rootFieldWrap.appendChild(rootSelect);
  const subFieldWrap = document.createElement('div');
  subFieldWrap.className = 'field';
  const subL = document.createElement('label');
  subL.textContent = t('rule.sub');
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
  [['junction', t('rule.link_junction')],
   ['symlink_dir', t('rule.link_symlink')]].forEach(function (p) {
    const o = document.createElement('option');
    o.value = p[0];
    o.textContent = p[1];
    linkSelect.appendChild(o);
  });
  linkSelect.value = rule ? rule.link_type : 'junction';
  body.appendChild(mk(t('detail.key.link_type'), linkSelect).field);

  // 开关
  const onCheck = document.createElement('input');
  onCheck.type = 'checkbox';
  onCheck.checked = rule ? rule.want_enabled : true;
  const bootCheck = document.createElement('input');
  bootCheck.type = 'checkbox';
  bootCheck.checked = rule ? rule.rebuild_on_boot : true;
  const flags = document.createElement('div');
  [[t('rule.want_enabled'), onCheck], [t('rule.rebuild_on_boot'), bootCheck]].forEach(function (p) {
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
  notes.placeholder = t('rule.notes_ph');
  notes.value = rule && rule.notes ? rule.notes : '';
  body.appendChild(mk(t('detail.key.notes'), notes).field);

  // 清理排除：一行一条，只影响「清理」，不影响应用/恢复的搬运
  const excludes = document.createElement('textarea');
  excludes.className = 'input';
  excludes.rows = 2;
  excludes.placeholder = t('rule.excludes_ph');
  excludes.value = rule && rule.excludes ? rule.excludes.join('\n') : '';
  const exWrap = mk(t('rule.excludes'), excludes);
  const exHint = document.createElement('div');
  exHint.className = 'hint';
  exHint.textContent = t('rule.excludes_hint');
  exWrap.field.appendChild(exHint);
  body.appendChild(exWrap.field);

  // 水位自动清理：RAM 盘到严重线（默认 95%）时的预授权，30 分钟冷却防反复删
  const autoSelect = document.createElement('select');
  autoSelect.className = 'select';
  [['', t('rule.auto.none')],
   ['empty', t('rule.auto.empty')],
   ['older', t('rule.auto.older')]].forEach(function (p) {
    const o = document.createElement('option');
    o.value = p[0];
    o.textContent = p[1];
    autoSelect.appendChild(o);
  });
  const autoHours = mkInput(t('rule.auto.hours_ph'));
  autoHours.type = 'number';
  autoHours.min = '1';
  autoHours.step = '1';
  autoHours.value = '24';
  const initPolicy = rule ? (rule.auto_clean_policy || '') : '';
  const initHours = /older:(\d+)/.exec(initPolicy);
  if (initPolicy === 'empty') autoSelect.value = 'empty';
  else if (initHours) { autoSelect.value = 'older'; autoHours.value = String(Math.max(1, Math.round(Number(initHours[1]) / 3600))); }
  const autoWrap = mk(t('rule.auto.label'), autoSelect);
  const autoRow = document.createElement('div');
  autoRow.className = 'field-row';
  autoRow.appendChild(autoWrap.field);
  const hoursWrap = mk(t('rule.auto.before'), autoHours);
  autoRow.appendChild(hoursWrap.field);
  const autoHint = document.createElement('div');
  autoHint.className = 'hint';
  autoHint.textContent = t('rule.auto.hint');
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
      const parts = [tf('rule.src.expanded', { path: info.expanded }) + (info.exists ? t('rule.src.exists') : t('rule.src.not_exist'))];
      if (info.unknown_vars.length) parts.push(tf('rule.src.unknown', { vars: info.unknown_vars.join(t('list.sep')) }));
      if (info.requires_elevation) parts.push(t('rule.src.elev'));
      srcHint.textContent = parts.join(' · ');
      srcHint.className = info.blocking.length ? 'hint err'
        : (info.requires_elevation || info.unknown_vars.length) ? 'hint warn' : 'hint ok';
      if (info.blocking.length) srcHint.textContent = '⛔ ' + info.blocking.join(t('list.sep'));
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
      let text = tf('rule.tgt.target', { path: info.target });
      if (!info.root_ready) text += t('rule.tgt.root_unavailable');
      if (info.occupied) text += t('rule.tgt.occupied');
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
    title: rule ? tf('rule.edit_title', { id: rule.id }) : t('rule.add_title'),
    body: body,
    actions: [
      { label: t('common.cancel'), onClick: closeModal },
      {
        label: rule ? t('common.save') : t('common.add'),
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
            toast(rule ? t('common.saved') : t('rule.added'), form.name, 'ok');
            await refresh(rule ? true : false);
          } catch (e) {
            toast(rule ? t('common.save_failed') : t('rule.add_failed'), e.message, 'error');
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
 * `targets` 是清理目标数组：单条规则时 `[{kind:'rule', id}]`；批量清理时
 * 把规则目标一次性传入，共用同一套策略。破坏性操作，走「先预演、看到清单再点确认删除」
 * 两步，删除按钮在预演前禁用。
 */
async function openCleanDialog(targets, headingText, dialogTitle) {
  const multi = targets.length > 1;
  const body = document.createElement('div');

  const head = document.createElement('div');
  head.className = 'field';
  head.textContent = headingText;
  body.appendChild(head);

  const warn = document.createElement('div');
  warn.className = 'hint warn';
  warn.textContent = t('clean.warn');
  body.appendChild(warn);

  // 策略单选
  const modes = [
    ['empty', t('clean.mode.empty')],
    ['before_day', t('clean.mode.day')],
    ['before_hour', t('clean.mode.hour')],
    ['between', t('clean.mode.between')],
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
  fl.textContent = t('clean.from');
  const tl = document.createElement('label');
  tl.textContent = t('clean.to');
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
  previewBtn.textContent = t('clean.preview');
  const delBtn = document.createElement('button');
  delBtn.className = 'btn danger';
  delBtn.textContent = t('clean.confirm_del');
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
      ? tf('clean.will_delete', { summary: pv.summary, dir: pv.dir })
      : tf('clean.none_match', { dir: pv.dir });
    previewHost.appendChild(sum);
    if (pv.links_skipped) {
      const lk = document.createElement('div');
      lk.className = 'hint warn';
      lk.textContent = tf('clean.links_skipped', { n: pv.links_skipped });
      previewHost.appendChild(lk);
    }
    if (pv.excluded_skipped) {
      const ex = document.createElement('div');
      ex.className = 'hint warn';
      ex.textContent = tf('clean.excluded', { n: pv.excluded_skipped });
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
      more.textContent = tf('clean.more', { n: pv.files });
      previewHost.appendChild(more);
    }
    delBtn.disabled = !pv.files;
    delBtn.textContent = pv.files ? tf('clean.confirm_del_n', { n: pv.files }) : t('clean.confirm_del');
  }

  function renderBatchPreview(rows, files, bytes, linksSkipped, excludedSkipped) {
    previewHost.textContent = '';
    const sum = document.createElement('div');
    sum.className = files ? 'hint' : 'hint ok';
    sum.textContent = files
      ? tf('clean.batch_will_delete', { files: files, size: humanBytes(bytes), rules: rows.length })
      : t('clean.batch_none');
    previewHost.appendChild(sum);
    if (linksSkipped) {
      const lk = document.createElement('div');
      lk.className = 'hint warn';
      lk.textContent = tf('clean.batch_links', { n: linksSkipped });
      previewHost.appendChild(lk);
    }
    if (excludedSkipped) {
      const ex = document.createElement('div');
      ex.className = 'hint warn';
      ex.textContent = tf('clean.excluded', { n: excludedSkipped });
      previewHost.appendChild(ex);
    }
    rows.forEach(function (it) {
      const line = document.createElement('div');
      line.className = it.error ? 'hint err' : 'hint';
      line.textContent = it.error
        ? tf('clean.item_error', { name: it.name, err: it.error })
        : ('· ' + it.name + '　' + (it.pv.files ? tf('clean.item_count', { files: it.pv.files, size: it.pv.bytes_text }) : t('clean.item_none')) + '　' + it.pv.dir);
      previewHost.appendChild(line);
    });
    delBtn.disabled = !files;
    delBtn.textContent = files ? tf('clean.confirm_del_n', { n: files }) : t('clean.confirm_del');
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
          st.textContent = tf('clean.preview_progress', { i: i + 1, n: targets.length });
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
      toast(t('clean.preview_failed'), e.message, 'error');
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
        toast(t('clean.done'), r.summary, r.skipped ? 'warn' : 'ok');
      } else {
        let deletedFiles = 0, deletedBytes = 0, skipped = 0, failed = 0;
        for (let i = 0; i < targets.length; i++) {
          delBtn.textContent = tf('clean.cleaning_progress', { i: i + 1, n: targets.length });
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
        toast(t('clean.batch_done'),
          tf('clean.batch_done_sub', { files: deletedFiles, size: humanBytes(deletedBytes) }) +
          (skipped ? tf('clean.batch_skipped', { n: skipped }) : '') + (failed ? tf('clean.batch_failed', { n: failed }) : ''),
          failed ? 'warn' : 'ok');
      }
      await refresh(true);
    } catch (e) {
      toast(t('clean.failed'), e.message, 'error');
      delBtn.disabled = false;
    }
  });

  openModal({
    title: dialogTitle || (multi ? t('clean.batch_title') : t('clean.title')),
    body: body,
    wide: true,
    actions: [{ label: t('common.close'), onClick: closeModal }],
  });
}

/** 底部「清理…」：按统一策略清理全部条目的目标目录。 */
async function openBatchCleanDialog() {
  const rules = state.snap.rules || [];
  if (!rules.length) {
    toast(t('clean.nothing'), t('clean.nothing_hint'), 'warn');
    return;
  }
  const targets = rules.map(function (r) { return { kind: 'rule', id: r.id }; });
  await openCleanDialog(targets, tf('clean.batch_heading', { n: rules.length }));
}

/** 工具栏「清理…」：按统一策略清理所有已选条目的目标目录。 */
async function openSelectedCleanDialog() {
  if (!requirePicked(t('pick.clean'))) return;
  const rules = (state.snap.rules || []).filter(function (r) { return state.picked.has(r.id); });
  if (!rules.length) {
    toast(t('clean.nothing'), t('clean.nothing_hint'), 'warn');
    return;
  }
  const targets = rules.map(function (r) { return { kind: 'rule', id: r.id }; });
  await openCleanDialog(
    targets,
    tf('clean.selected_batch_heading', { n: rules.length }),
    t('clean.selected_batch_title'),
  );
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
    ? tf('disk.scan_item', { name: (p.name || ''), i: (p.index + 1), n: total })
    : t('disk.scan_volumes');
});

/** 工具栏「空间统计」：实时扫描各规则缓存占用，并汇总相关卷容量。 */
async function openDiskStats() {
  const body = document.createElement('div');

  const status = document.createElement('div');
  status.className = 'hint';
  status.textContent = t('disk.scanning');
  body.appendChild(status);

  const host = document.createElement('div');
  body.appendChild(host);

  openModal({
    title: t('disk.title'),
    body: body,
    wide: true,
    actions: [
      { label: t('disk.rescan'), onClick: function () { load(); } },
      { label: t('common.close'), onClick: closeModal },
    ],
  });

  async function load() {
    host.textContent = '';
    status.className = 'hint';
    status.textContent = t('disk.scanning');
    scanHint = status;
    try {
      const data = await invoke('disk_usage');
      renderDiskStats(host, status, data);
    } catch (e) {
      status.className = 'hint err';
      status.textContent = t('disk.failed') + '：' + e.message;
    } finally {
      scanHint = null;
    }
  }
  await load();
}

function renderDiskStats(host, status, data) {
  host.textContent = '';
  status.className = 'hint ok';
  status.textContent = tf('disk.summary', { rules: data.rule_count, size: data.total_bytes_text, ms: data.elapsed_ms });

  // ---- 各规则占用排行（横向条形图） ----
  const barsTitle = document.createElement('div');
  barsTitle.className = 'chart-title';
  barsTitle.textContent = t('disk.bars_title');
  host.appendChild(barsTitle);

  const rules = (data.rules || []).filter(function (r) { return r.bytes > 0; });
  if (!rules.length) {
    const none = document.createElement('div');
    none.className = 'hint';
    none.textContent = t('disk.bars_none');
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
  volTitle.textContent = t('disk.vol_title');
  host.appendChild(volTitle);

  const vols = data.volumes || [];
  if (!vols.length) {
    const none = document.createElement('div');
    none.className = 'hint';
    none.textContent = t('disk.vol_none');
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
  nm.textContent = (v.roles && v.roles.length)
    ? tf('disk.vol_roles', { path: v.path, roles: v.roles.join(' · ') })
    : v.path;
  const txt = document.createElement('div');
  txt.className = 'vol-text';
  txt.textContent = tf('disk.vol_text', { used: v.used_text, total: v.total_text, free: v.free_text, pct: v.used_pct.toFixed(1) });
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
      await openCleanDialog([{ kind: 'rule', id: id }], tf('clean.title_rule', { name: r ? r.name : ('#' + id) }));
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
      catch (e) { toast(t('toast.reveal_failed'), e.message, 'error'); }
      return;
    }
    // 内置模板行：「添加并应用」= 建规则 + 立刻建链（等价于勾选后按「应用选中」）
    if (act === 'tpl-apply') {
      const p = (state.presets || []).find(function (x) { return tplKey(x.source_template) === tplKey(target.dataset.tpl); });
      if (!p) return;
      const root = tplTargetRoot();
      if (!root) { toast(t('rule.no_root_title'), t('rule.no_root_hint'), 'warn'); return; }
      try {
        const newId = await invoke('add_rule', { form: templateForm(p, root.id) });
        await runApply([newId], false);
      } catch (e) {
        toast(tf('tpl.add_failed', { name: p.name }), e.message, 'error');
      }
      return;
    }
    return;
  }

  const el = ev.target;
  const closest = el.closest ? el.closest.bind(el) : function () { return null; };
  // 表头点击排序（th[data-sort]）：同列切换升降序，新列默认升序。
  const sortTh = closest('th[data-sort]');
  if (sortTh) {
    const key = sortTh.dataset.sort;
    if (state.sort.key === key) {
      state.sort.dir = state.sort.dir === 'asc' ? 'desc' : 'asc';
    } else {
      state.sort = { key: key, dir: 'asc' };
    }
    renderRows();
    renderSortIndicators();
    return;
  }
  // 「更多」菜单：点菜单外任意处收起；点「更多」切换；点菜单项后收起再走下面的分支。
  if (!closest('.more-wrap')) hideMoreMenu();
  if (el.id === 'btn-more') { toggleMoreMenu(); return; }
  if (closest('#more-menu')) hideMoreMenu();
  if (el.id === 'btn-refresh') await refresh(true);
  else if (el.id === 'btn-add') await openRuleForm(null);
  else if (el.id === 'btn-apply') { if (requireAnyPick(t('pick.apply'))) await applyPickedAll(); }
  else if (el.id === 'btn-restore') { if (requirePicked(t('pick.restore'))) await runRestore(pickedIds()); }
  else if (el.id === 'btn-enable' || el.id === 'btn-disable') {
    if (!requirePicked(t('pick.toggle'))) return;
    const enabled = el.id === 'btn-enable';
    try {
      await invoke('set_rules_enabled', { ids: pickedIds(), enabled: enabled });
      toast(enabled ? t('toast.enabled') : t('toast.disabled'), tf('toast.toggle_hint', { n: pickedIds().length }), 'ok');
      await refresh(true);
    } catch (e) { toast(t('toast.op_failed'), e.message, 'error'); }
  } else if (el.id === 'btn-locks') {
    if (!requirePicked(t('pick.detect'))) return;
    await doLocks(pickedIds()[0]);
  } else if (el.id === 'btn-clean') {
    await openSelectedCleanDialog();
  } else if (el.id === 'btn-disk') {
    await openDiskStats();
  } else if (el.id === 'btn-relink') {
    if (!requirePicked(t('pick.relink'))) return;
    for (const id of pickedIds()) await doRelink(id);
  } else if (el.id === 'btn-delete') {
    if (!requirePicked(t('pick.delete'))) return;
    const ids = pickedIds();
    const body = document.createElement('div');
    const p1 = document.createElement('div');
    p1.className = 'field';
    p1.textContent = tf('del.confirm_body', { n: ids.length });
    const p2 = document.createElement('div');
    p2.className = 'field';
    const h = document.createElement('div');
    h.className = 'hint warn';
    h.textContent = t('del.confirm_warn');
    p2.appendChild(h);
    body.appendChild(p1);
    body.appendChild(p2);
    openModal({
      title: t('del.confirm_title'),
      body: body,
      actions: [
        { label: t('common.cancel'), onClick: closeModal },
        {
          label: t('common.delete'), kind: 'danger',
          onClick: async function () {
            try {
              await invoke('delete_rules', { ids: ids });
              state.picked.clear();
              closeModal();
              toast(t('del.done'), tf('del.done_hint', { n: ids.length }), 'ok');
              await refresh(false);
            } catch (e) { toast(t('del.failed'), e.message, 'error'); }
          },
        },
      ],
    });
  } else if (el.id === 'btn-boot') await doBoot();
  else if (el.id === 'btn-restore-all') await restoreAllRules();
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
  // 内置模板行的勾选：模板还没有 id，用 source_template 小写当键。
  if (el.dataset && el.dataset.act === 'pick-tpl') {
    const key = tplKey(el.dataset.tpl);
    if (el.checked) state.pickedTpl.add(key); else state.pickedTpl.delete(key);
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
    } else if (!$('detail').classList.contains('hidden')) {
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
    toast(t('toast.reorder_failed'), e.message, 'error');
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
  toast(t('toast.tray_boot'), p.message, p.ok ? 'ok' : 'warn');
  refresh(true);
});

// ---------------------------------------------------------------------------
// Mock 后端（仅用于本地静态预览 / 无头截图核验，不参与打包逻辑）
// ---------------------------------------------------------------------------

function createMockBackend() {
  const rules = [
    mockRule(1, t('mock.rule.chrome'), 'browser', '%LOCALAPPDATA%\\Google\\Chrome\\User Data\\Default\\Cache', 'browsers\\chrome-cache', '✅', 'link_ok', { last_files: 34521, last_bytes: 1288490188, auto_clean_policy: 'older:86400' }),
    mockRule(2, t('mock.rule.edge'), 'browser', '%LOCALAPPDATA%\\Microsoft\\Edge\\User Data\\Default\\Cache', 'browsers\\edge-cache', '✅', 'link_ok', { last_files: 21002, last_bytes: 734003200 }),
    mockRule(3, t('mock.rule.firefox'), 'browser', '%APPDATA%\\Mozilla\\Firefox\\Profiles\\xxxx.default-release\\cache2', 'browsers\\firefox-cache2', '⚠️', 'link_ok_empty', {}),
    mockRule(4, t('mock.rule.photoshop'), 'design', 'C:\\Users\\me\\AppData\\Roaming\\Adobe\\Common\\Media Cache Files', 'design\\ps-media-cache', '⚠️', 'link_broken', { needs_attention: true }),
    mockRule(5, t('mock.rule.vscode'), 'dev', '%APPDATA%\\Code\\Cache', 'dev\\vscode-cache', '❌', 'plain_dir', { excludes: ['logs', 'CachedData\\old'] }),
    mockRule(6, t('mock.rule.wechat'), 'office', '%APPDATA%\\Tencent\\WeChat\\FileStorage', 'office\\wechat-files', '❌', 'plain_dir', { want_enabled: false }),
    mockRule(7, t('mock.rule.npm'), 'dev', '%LOCALAPPDATA%\\npm-cache', 'dev\\npm-cache', '✅', 'link_ok', { last_files: 88213, last_bytes: 3221225472 }),
    mockRule(8, t('mock.rule.premiere'), 'design', 'C:\\Users\\me\\AppData\\Roaming\\Adobe\\Common\\Media Cache', 'design\\pr-media-cache', '❌', 'plain_dir', { needs_elevation: true, has_acl: true, want_enabled: false }),
  ];

  /** 状态说明按当前语言生成（键在 i18n.js 的 mock.state.*，措辞与后端对齐）。
   *  切换语言时由 relabelRules() 重算，行为与真机一致（名称是用户数据，不重算）。 */
  function stateLabel(r) {
    if (r._state === 'link_ok') return t('mock.state.link_ok');
    if (r._state === 'link_ok_empty') return tf('mock.state.link_ok_empty', { target: r.target });
    if (r._state === 'link_broken') return tf('mock.state.link_broken', { target: r.target });
    return t('mock.state.plain_dir');
  }

  /** 语言切换后重算状态文案（mock.set_locale 里调用）。 */
  function relabelRules() {
    rules.forEach(function (r) {
      r.label = stateLabel(r);
      r.warnings = r.needs_elevation ? [t('rule.src.elev')] : [];
    });
  }

  function mockRule(id, name, category, tpl, sub, badge, stateKey, extra) {
    const e = extra || {};
    const r = {
      id: id, name: name, category: category,
      source_template: tpl,
      source: tpl.replace('%LOCALAPPDATA%', 'C:\\Users\\me\\AppData\\Local').replace('%APPDATA%', 'C:\\Users\\me\\AppData\\Roaming'),
      target: 'R:\\' + sub,
      target_root_id: 1, target_subpath: sub, link_type: 'junction',
      want_enabled: e.want_enabled !== false, rebuild_on_boot: true,
      has_acl: !!e.has_acl, needs_elevation: !!e.needs_elevation, notes: null,
      excludes: e.excludes || [],
      auto_clean_policy: e.auto_clean_policy || '',
      badge: badge, label: '',
      needs_attention: !!e.needs_attention || badge === '❌',
      unknown_vars: [], last_files: e.last_files || null, last_bytes: e.last_bytes || null,
      applied_at: e.last_files ? '2026-10-14 09:45:56' : null,
      _state: stateKey,
    };
    r.label = stateLabel(r);
    r.warnings = e.needs_elevation ? [t('rule.src.elev')] : [];
    return r;
  }

  let mockCloseToTray = true;
  let mockSched = { enabled: false, time: '03:00' };

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
        state: 'ready', message: tf('mock.root_ready', { free: '3.7 GB' }), unknown_vars: [],
      }],
      rules: rules.map(function (r) { const c = Object.assign({}, r); delete c._state; return c; }),
      stats: stats(),
      banners: [
        { level: 'warn', text: tf('mock.banner.attention', { n: stats().needs_attention }) },
        { level: 'error', text: tf('mock.banner.disk_critical', { label: 'RAMDisk 4G', path: 'R:', pct: '96.4', free: '147.5 MB' }), action: 'sweep' },
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
          rules.push(mockRule(id, f.name, f.category, f.source_template, f.target_subpath, '❌', 'plain_dir', { want_enabled: f.want_enabled, auto_clean_policy: f.auto_clean_policy || '' }));
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
            { root_id: 1, label: 'RAMDisk 4G', path: 'R:', used_pct: 70.0, free: 1288490188, free_text: '1.2 GB', total_text: '4.0 GB', level: 'ok', message: tf('mock.water_msg', { pct: '70.0', free: '1.2 GB' }) },
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
          const tgt = args.target || {};
          return Promise.resolve({
            dir: tgt.kind === 'folder' ? tgt.path : 'R:\\dev\\vscode-cache',
            label: '（模拟）VS Code 缓存',
            files: 1280, bytes: 536870912, bytes_text: '512.0 MB', links_skipped: 0,
            excluded_skipped: tgt.kind === 'rule' ? 12 : 0,
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
            r._state = 'link_ok'; r.badge = '✅'; r.label = stateLabel(r); r.needs_attention = false;
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
            r._state = 'plain_dir'; r.badge = '❌'; r.label = stateLabel(r); r.needs_attention = true;
            out.push({ rule_id: id, name: r.name, ok: true, action: 'restored', message: '已恢复：120 个文件 / 4.0 MB 已搬回', elapsed_ms: 31 });
          });
          return Promise.resolve(out);
        }
        case 'relink_rule': {
          const r = find(args.id);
          if (r && r.badge !== '✅') { r._state = 'link_ok'; r.badge = '✅'; r.label = stateLabel(r); r.needs_attention = false; }
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
            message: tf('mock.autostart_registered', { cmd: 'C:\\Program Files\\TempDirector\\tempdirector-gui.exe boot-rebuild' }),
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
            r._state = 'link_ok'; r.badge = '✅'; r.label = stateLabel(r); r.needs_attention = false;
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
        case 'scheduled_clean_settings':
          return Promise.resolve({ enabled: mockSched.enabled, time: mockSched.time });
        case 'set_scheduled_clean': {
          const tm = (args.time || '').trim();
          if (!/^\d{2}:\d{2}$/.test(tm)) return Promise.reject(new Error(t('toast.bad_time_format')));
          mockSched = { enabled: !!args.enabled, time: tm };
          return Promise.resolve(null);
        }
        case 'scheduled_clean_poll':
          // mock 永远"未到点"（避免预览界面真的删东西的错觉）
          return Promise.resolve({ ran: false, done: 0, failed: 0, skipped: 0, deleted_files: 0, deleted_bytes: 0, deleted_text: '0 B' });
        case 'set_locale':
          try { localStorage.setItem(LOCALE_KEY, args.locale); } catch (e) { /* 忽略 */ }
          // 与真机一致：语言切换后状态文案按新语言重算（随后 td-lang-change 会触发重渲染）
          relabelRules();
          return Promise.resolve(null);
        case 'get_locale':
          return Promise.resolve(getLang());
        default: return Promise.reject(new Error('mock 未实现命令：' + name));
      }
    },
    listen: function () { return Promise.resolve(function () {}); },
  };
}

function simulateProgress(total) {
  let i = 0;
  showProgress(tf('mock.simulate_running', { n: total }));
  const timer = setInterval(function () {
    i += 1;
    setProgress((i / total) * 100, tf('mock.simulate_detail', { i: i, n: total, files: i * 4200, mb: i * 137 }));
    if (i >= total) clearInterval(timer);
  }, 260);
}

// ---------------------------------------------------------------------------
// 语言切换
// ---------------------------------------------------------------------------

// 语言切换分段按钮（中文 | EN）：初值取自 i18n（localStorage，无记录默认英文）；
// 切换时通知后端并整体刷新。
//
// 为什么要整体 refresh 而不是只 render()：规则状态标签 / 横幅 / 水位消息这些文案是
// 后端按当前语言生成后随快照下发的，只重渲染 DOM 会用旧语言的缓存数据 ——
// 必须重新拉一次快照，切换后的列表才整片换语言（mock 后端在 set_locale 里原地重算，
// 走的是同一条契约）。
(function initLang() {
  const wrap = $('lang-switch');
  if (wrap) {
    const paint = function () {
      wrap.querySelectorAll('.lang-seg').forEach(function (b) {
        const on = b.dataset.lang === getLang();
        b.classList.toggle('active', on);
        b.setAttribute('aria-pressed', on ? 'true' : 'false');
      });
    };
    wrap.querySelectorAll('.lang-seg').forEach(function (b) {
      b.addEventListener('click', function () { setLang(b.dataset.lang); });
    });
    paint();
    window.addEventListener('td-lang-change', function (ev) {
      const lang = ev.detail.lang;
      invoke('set_locale', { locale: lang })
        .catch(function () { /* mock 或旧后端可忽略 */ })
        .then(function () { return refresh(true); })
        .catch(function () { if (state.snap) render(); });
      paint();
    });
  }
})();

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

(async function boot() {
  // 启动即把界面语言同步给后端：状态标签 / 横幅 / 水位消息等文案由后端按当前语言
  // 生成，不推送的话后端停留在自己的默认语言（中文），界面是英文、数据是中文。
  try { await invoke('set_locale', { locale: getLang() }); } catch (e) { /* mock 或旧后端 */ }
  await refresh(false);
  if (PARAMS.has('progress')) {
    showProgress(t('mock.progress_title'));
    setProgress(46, t('mock.progress_detail'));
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
  // 定时（低峰期）清理与水位同节拍巡检：到点且当天没跑过才执行（后端判定），
  // 真的删了文件时由 Rust 侧发系统通知，这里只需在执行后刷新快照。
  setInterval(async function () {
    try {
      const levels = await invoke('water_poll', {});
      if (levels.some(function (w) { return w.level === 'warn' || w.level === 'critical'; })) {
        await refresh(true);
      }
      const sc = await invoke('scheduled_clean_poll', {});
      if (sc.ran) await refresh(true);
    } catch (e) { /* 巡检失败静默，下轮再试 */ }
  }, 60000);
})();
