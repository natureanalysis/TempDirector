/* TempDirector 前端多语言（中 / 英）。
 *
 * 设计约束：
 *  - 不引 npm：词典就是下面两棵平铺的「键 → 文案」对象，键用点分命名空间
 *    （toolbar.add、table.status、err.io…），与后端 td-core::i18n 的键风格一致。
 *  - 首次启动跟随系统语言（navigator.language），用户手动切换后写入 localStorage。
 *  - 切换语言：更新 localStorage → 重跑 applyStatic() → 派发 `td-lang-change` 事件，
 *    由 app.js 负责通知后端并整体重渲染（后端错误文案也随之换语言）。
 *  - 静态 HTML 用 data-i18n（文本）/ data-i18n-title（title）/ data-i18n-placeholder
 *    声明键，本文件载入即批量替换一次。
 */
'use strict';

const LOCALE_KEY = 'tempdirector.locale';

const DICT = {
  zh: {
    // 顶栏
    'app.title': 'TempDirector —— 缓存目录重定向',
    'header.reading': '正在读取…',
    'nav.ops': '运维日志',
    'nav.settings': '设置',
    'nav.refresh': '刷新',
    'nav.lang_zh': '中文',
    'nav.lang_en': 'English',

    // 工具栏
    'toolbar.add': '＋ 添加规则',
    'toolbar.apply': '应用选中',
    'toolbar.restore': '恢复选中',
    'toolbar.clean': '清理…',
    'toolbar.clean_tip': '按统一策略清理全部规则的目标目录',
    'toolbar.enable': '开启',
    'toolbar.disable': '关闭',
    'toolbar.delete': '删除',
    'toolbar.more': '更多 ▾',
    'toolbar.more_tip': '诊断与低频操作',
    'menu.locks': '检测占用',
    'menu.disk': '空间统计',
    'menu.relink': '仅重建链接',
    'menu.boot': '执行开机重建',
    'menu.restore_all': '恢复全部规则…',

    // 筛选
    'filter.search': '搜索名称 / 源路径 / 目标子目录…',
    'filter.only_enabled': '只看已开启',
    'filter.only_attention': '只看需处理',
    'filter.select_all': '全选当前结果',
    'filter.select_all_tip': '全选 / 取消全选当前结果',
    'filter.all_categories': '全部分类',

    // 表头
    'table.status': '状态',
    'table.name': '名称',
    'table.category': '分类',
    'table.source': '源路径',
    'table.target': '目标（RAM 盘）',
    'table.flag': '旗标',
    'table.flag_tip': '左：期望开启（● 开 / ○ 关）；右：开机自动重建（⟳ 参与）',

    // 通用
    'common.close': '关闭',
    'common.ok': '确定',
    'common.cancel': '取消',
    'common.confirm': '确认',
    'common.save': '保存',
    'common.delete': '删除',
    'common.add': '添加',
    'common.copied': '已复制',
    'common.copy_failed': '复制失败',
    'common.copy_manual': '请手动选择文本复制',
    'common.none': '无',

    // 进度
    'progress.working': '处理中…',

    // 分类标签
    'cat.browser': '浏览器',
    'cat.design': '设计',
    'cat.dev': '开发',
    'cat.system': '系统',
    'cat.media': '影音',
    'cat.office': '办公',
    'cat.game': '游戏',
    'cat.other': '其他',

    // 徽标后缀
    'badge.junction': '联接',
    'badge.symlink': '符号链接',
    'badge.unlinked': '未链接',
    'badge.foreign': '外来',
    'badge.unknown': '未知',

    // 头部 / 统计
    'header.no_root': '未配置目标根 —— 先到「设置」里添加 RAM 盘',
    'header.root_ready': '就绪',
    'header.root_unavailable': '不可用（{msg}）',
    'header.root_prefix': '目标根 {path} {mark}',
    'header.roots_more': ' · 共 {n} 个目标根',
    'stat.summary': '共 {total} 条 · 已开启 {enabled} · ✅ {ok} ⚠️ {warn} ❌ {plain}',

    // 横幅 / 提示
    'banner.clean_now': '一键清理',
    'toast.load_failed': '读取状态失败',
    'toast.no_pick': '没有选中规则',
    'toast.no_pick_hint': '先用左侧复选框勾选要{what}的规则',
    'toast.bad_time_format': '时刻格式应为 HH:MM（如 03:00）',

    // 筛选摘要 / 空态
    'filter.summary': '显示 {shown} / {total} 条',
    'filter.summary_picked': ' · 已勾选 {n} 条',
    'empty.no_match': '没有符合筛选条件的规则',
    'empty.no_rules': '还没有任何规则 —— 点左上角「＋ 添加规则」开始',
    'list.sep': '；',

    // 行
    'row.drag_tip': '拖动调整顺序（同一分类内）',
    'row.expect_on': '期望开启（参与「应用选中」）',
    'row.expect_off': '期望关闭',
    'row.rebuild_on': '开机自动重建目标目录',
    'row.rebuild_off': '不参与开机重建',
    'row.act.apply': '应用',
    'row.act.restore': '恢复',
    'row.act.relink': '重链',
    'row.act.clean': '清理…',
    'row.act.edit': '编辑',

    // 详情
    'detail.chip.want_enabled': '期望开启',
    'detail.chip.rebuild': '开机重建',
    'detail.chip.needs_elevation': '需要管理员权限',
    'detail.chip.has_acl': '已存权限模板',
    'detail.chip.files': '{n} 个文件 · {size}',
    'detail.key.status': '状态',
    'detail.key.source_template': '源路径模板',
    'detail.key.expanded': '展开后',
    'detail.key.target': '目标路径',
    'detail.key.link_type': '链接类型',
    'detail.key.notes': '备注',
    'detail.key.excludes': '清理排除',
    'detail.key.auto_clean': '水位自动清理',
    'detail.key.unknown_vars': '未解析变量',
    'detail.key.warning': '警告',
    'detail.key.applied_at': '上次应用',
    'detail.link.symlink': '目录符号链接（需要特权）',
    'detail.link.junction': '目录联接 Junction（免提权）',
    'detail.act.apply': '应用（搬运 + 建链）',
    'detail.act.apply_elevated': '以管理员身份应用',
    'detail.act.restore': '恢复（拆链 + 搬回）',
    'detail.act.relink': '仅重建链接',
    'detail.act.locks': '检测占用',
    'detail.act.clean': '清理…',
    'detail.act.copy_source': '复制源路径',
    'detail.act.reveal': '打开位置',
    'detail.act.edit': '编辑',
    'detail.auto.empty': '水位到严重线时清空全部',
    'detail.auto.days': '水位到严重线时清空早于 {n} 天修改的',
    'detail.auto.hours': '水位到严重线时清空早于 {n} 小时修改的',
    'detail.auto.secs': '水位到严重线时清空早于 {n} 秒修改的',
    'mock.progress_title': '正在处理 3 条规则…',
  },

  en: {
    // 顶栏
    'app.title': 'TempDirector — Cache Redirect',
    'header.reading': 'Loading…',
    'nav.ops': 'Activity log',
    'nav.settings': 'Settings',
    'nav.refresh': 'Refresh',
    'nav.lang_zh': '中文',
    'nav.lang_en': 'English',

    // 工具栏
    'toolbar.add': '＋ Add rule',
    'toolbar.apply': 'Apply selected',
    'toolbar.restore': 'Restore selected',
    'toolbar.clean': 'Clean…',
    'toolbar.clean_tip': 'Clean every rule’s target directory with the unified policy',
    'toolbar.enable': 'Enable',
    'toolbar.disable': 'Disable',
    'toolbar.delete': 'Delete',
    'toolbar.more': 'More ▾',
    'toolbar.more_tip': 'Diagnostics and low-frequency actions',
    'menu.locks': 'Check locks',
    'menu.disk': 'Disk usage',
    'menu.relink': 'Relink only',
    'menu.boot': 'Run boot rebuild',
    'menu.restore_all': 'Restore all rules…',

    // 筛选
    'filter.search': 'Search name / source path / target subpath…',
    'filter.only_enabled': 'Enabled only',
    'filter.only_attention': 'Needs action only',
    'filter.select_all': 'Select all current results',
    'filter.select_all_tip': 'Select / deselect all current results',
    'filter.all_categories': 'All categories',

    // 表头
    'table.status': 'Status',
    'table.name': 'Name',
    'table.category': 'Category',
    'table.source': 'Source path',
    'table.target': 'Target (RAM disk)',
    'table.flag': 'Flags',
    'table.flag_tip': 'Left: desired enabled (● on / ○ off); Right: rebuild on boot (⟳ participates)',

    // 通用
    'common.close': 'Close',
    'common.ok': 'OK',
    'common.cancel': 'Cancel',
    'common.confirm': 'Confirm',
    'common.save': 'Save',
    'common.delete': 'Delete',
    'common.add': 'Add',
    'common.copied': 'Copied',
    'common.copy_failed': 'Copy failed',
    'common.copy_manual': 'Please select the text and copy it manually',
    'common.none': 'None',

    // 进度
    'progress.working': 'Working…',

    // 分类标签
    'cat.browser': 'Browser',
    'cat.design': 'Design',
    'cat.dev': 'Dev',
    'cat.system': 'System',
    'cat.media': 'Media',
    'cat.office': 'Office',
    'cat.game': 'Game',
    'cat.other': 'Other',

    // 徽标后缀
    'badge.junction': 'junction',
    'badge.symlink': 'symlink',
    'badge.unlinked': 'unlinked',
    'badge.foreign': 'foreign',
    'badge.unknown': 'unknown',

    // header / stats
    'header.no_root': 'No target root configured — add a RAM disk in Settings first',
    'header.root_ready': 'ready',
    'header.root_unavailable': 'unavailable ({msg})',
    'header.root_prefix': 'Target root {path} {mark}',
    'header.roots_more': ' · {n} target roots',
    'stat.summary': '{total} rules · {enabled} enabled · ✅ {ok} ⚠️ {warn} ❌ {plain}',

    // banners / toasts
    'banner.clean_now': 'Clean now',
    'toast.load_failed': 'Failed to load state',
    'toast.no_pick': 'No rules selected',
    'toast.no_pick_hint': 'Tick the rules you want to {what} using the checkboxes on the left',
    'toast.bad_time_format': 'Time must be HH:MM (e.g. 03:00)',

    // filter summary / empty
    'filter.summary': 'Showing {shown} / {total}',
    'filter.summary_picked': ' · {n} selected',
    'empty.no_match': 'No rules match the current filters',
    'empty.no_rules': 'No rules yet — click “＋ Add rule” at the top left to begin',
    'list.sep': '; ',

    // rows
    'row.drag_tip': 'Drag to reorder (within the same category)',
    'row.expect_on': 'Desired on (participates in “Apply selected”)',
    'row.expect_off': 'Desired off',
    'row.rebuild_on': 'Rebuild target on boot',
    'row.rebuild_off': 'Not rebuilt on boot',
    'row.act.apply': 'Apply',
    'row.act.restore': 'Restore',
    'row.act.relink': 'Relink',
    'row.act.clean': 'Clean…',
    'row.act.edit': 'Edit',

    // detail
    'detail.chip.want_enabled': 'Desired on',
    'detail.chip.rebuild': 'Rebuild on boot',
    'detail.chip.needs_elevation': 'Needs admin',
    'detail.chip.has_acl': 'ACL saved',
    'detail.chip.files': '{n} files · {size}',
    'detail.key.status': 'Status',
    'detail.key.source_template': 'Source template',
    'detail.key.expanded': 'Expanded',
    'detail.key.target': 'Target path',
    'detail.key.link_type': 'Link type',
    'detail.key.notes': 'Notes',
    'detail.key.excludes': 'Clean excludes',
    'detail.key.auto_clean': 'Water-level auto-clean',
    'detail.key.unknown_vars': 'Unresolved variables',
    'detail.key.warning': 'Warning',
    'detail.key.applied_at': 'Last applied',
    'detail.link.symlink': 'Directory symlink (privileged)',
    'detail.link.junction': 'Directory junction (no elevation)',
    'detail.act.apply': 'Apply (move + link)',
    'detail.act.apply_elevated': 'Apply as administrator',
    'detail.act.restore': 'Restore (unlink + move back)',
    'detail.act.relink': 'Relink only',
    'detail.act.locks': 'Check locks',
    'detail.act.clean': 'Clean…',
    'detail.act.copy_source': 'Copy source path',
    'detail.act.reveal': 'Reveal location',
    'detail.act.edit': 'Edit',
    'detail.auto.empty': 'Clear everything when water level hits critical',
    'detail.auto.days': 'Clear entries older than {n} days at critical water level',
    'detail.auto.hours': 'Clear entries older than {n} hours at critical water level',
    'detail.auto.secs': 'Clear entries older than {n} seconds at critical water level',
    'mock.progress_title': 'Processing 3 rules…',
  },
};

let currentLang = 'zh';

/** 系统语言 → 支持的语言；非中文一律回退英文。 */
function detectLang() {
  const nav = (navigator.language || navigator.userLanguage || '').toLowerCase();
  return nav.indexOf('zh') === 0 ? 'zh' : 'en';
}

/** 载入语言：localStorage 优先，其次系统语言。 */
function loadLang() {
  let saved = null;
  try { saved = localStorage.getItem(LOCALE_KEY); } catch (e) { saved = null; }
  currentLang = (saved === 'zh' || saved === 'en') ? saved : detectLang();
}

function getLang() { return currentLang; }

/** 取一条文案；缺失时回退键名（方便一眼看出漏翻）。 */
function t(key) {
  const table = DICT[currentLang] || DICT.zh;
  if (Object.prototype.hasOwnProperty.call(table, key)) return table[key];
  const fallback = DICT.zh;
  return Object.prototype.hasOwnProperty.call(fallback, key) ? fallback[key] : key;
}

/** 取一条文案并做 {name} 插值。 */
function tf(key, args) {
  let s = t(key);
  if (!args) return s;
  Object.keys(args).forEach(function (k) {
    s = s.split('{' + k + '}').join(String(args[k]));
  });
  return s;
}

/** 批量替换静态 DOM 上的 data-i18n / data-i18n-title / data-i18n-placeholder。 */
function applyStatic(root) {
  const scope = root || document;
  scope.querySelectorAll('[data-i18n]').forEach(function (el) {
    el.textContent = t(el.getAttribute('data-i18n'));
  });
  scope.querySelectorAll('[data-i18n-title]').forEach(function (el) {
    el.setAttribute('title', t(el.getAttribute('data-i18n-title')));
  });
  scope.querySelectorAll('[data-i18n-placeholder]').forEach(function (el) {
    el.setAttribute('placeholder', t(el.getAttribute('data-i18n-placeholder')));
  });
  document.documentElement.setAttribute('lang', currentLang === 'zh' ? 'zh-CN' : 'en');
}

/** 切换语言：持久化 + 重刷静态文案 + 广播事件（由 app.js 接力后端与重渲染）。 */
function setLang(lang) {
  if (lang !== 'zh' && lang !== 'en') return;
  currentLang = lang;
  try { localStorage.setItem(LOCALE_KEY, lang); } catch (e) { /* 隐私模式等，忽略 */ }
  applyStatic();
  window.dispatchEvent(new CustomEvent('td-lang-change', { detail: { lang: lang } }));
}

loadLang();