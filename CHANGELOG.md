# CHANGELOG

（按惯例用日期标题，不引入语义化版本号。）

## 2026-10-28（界面：内置模板在列表内直显可勾选 + 「空间统计」回到工具栏）

### 界面 —— 内置模板不再需要「手动添加」这一步

* **列表里直显**：此前内置预设只藏在「添加规则」弹窗顶部的下拉里，要建一条规则得先开弹窗、
  挑预设、再确认 —— 把「内置」做成了半手动。现在 `preset_rules()` 返回、且本机源目录真实
  存在的预设，会直接作为表格行渲染在规则列表末尾，行内带「➕ 未添加」标记与「添加并应用」
  按钮，**全程不开弹窗**。
* **勾选即应用**：「应用选中」的判断由原先的 `requirePicked`（必须选中已有规则）放宽为
  `requireAnyPick` —— 只勾模板也能执行 `add_rule` → `apply_rules`，链接类型 junction、
  期望开启、开机重建，与弹窗预设默认值一致。
* **边界**：模板行不参与「全选当前结果」，也不进 恢复 / 开启 / 关闭 / 删除 / 清理 /
  检测占用 / 仅重建链接（这些仍是纯规则操作）；「只看已开启 / 只看需处理」生效时不展示
  模板行；模板目标根默认取第一个目标根；已存在同 `source_template` 规则的模板不再重复展示。
* 「添加规则」弹窗顶部的预设下拉**保留**（挑模板后顺手改名字 / 分类这条路径仍有价值）。

### 内置预设 —— 补「系统 TEMP」，并确认浏览器缓存已在册

* `presets.json` 新增 `系统 TEMP`（`%WINDIR%\Temp` → `system\windows-temp`）。该目录落在
  `ELEVATED_PREFIX`（`windows\`）内，会被标记 `requires_elevation`、按需走 UAC；note 写明
  「C:\Windows\Temp 常被系统组件占用，被占文件会自动跳过」。
* 浏览器缓存条目（Chrome / Edge 的网页缓存、Code Cache、GPU 缓存，共 6 条）此前已在清单中，
  本轮确认无需改动。展示口径**未变**：仍只列出本机实际存在源目录的条目。

### 界面 —— 「空间统计」回到工具栏

* `#btn-disk` 从「更多 ▾」移出，作为工具栏直接按钮放在「清理…」之后、分隔线之前；「更多」
  现只剩 检测占用 / 仅重建链接 / 执行开机重建 / 恢复全部规则。这是对 `## 2026-10-16（界面：
  工具栏与表格减负）`中「空间统计收进更多」的回退 —— 它是需要经常瞄一眼的读数，不该藏两层。

### 验证

* `node --check` 通过（`app.js` / `i18n.js`）；`grep -rn "menu.disk"` 全仓 0 残留引用，
  `toolbar.disk` 仅出现在 `i18n.js`（zh / en）与 `index.html`。
* `presets.json` 结构自检：有效 JSON、25 条、`target_subpath` 忽略大小写无重复、
  `source_template` 均以 `%…%` 起始、仍含 `%LOCALAPPDATA%\Temp`。
* `index.html?mock=1` 浏览器取证：工具栏直接含「空间统计 / Disk usage」按钮且「更多」里没有它；
  列表底部出现「➕ 未添加」模板行；勾选模板行后摘要出现 "1 selected"，点「应用选中」后规则数
  自增并生成对应规则行、该模板行消失；点模板行自带的「添加并应用」同样生效。
* **构建与测试**（本轮补跑，此前【构建被沙箱挡住、未跑 cargo test】的记录已不成立）：
  `scripts\cargo.cmd build --release --workspace` 通过、无 warning，产物
  `target\release\tempdirector-gui.exe` —— 二进制内可搜到新增预设 `系统 TEMP`、
  `%WINDIR%\Temp`、`system\windows-temp` 原文（`include_str!` 直出）。
  `cargo test --workspace` 全绿：8 个测试目标、186 passed / 0 failed；其中
  `builtin_presets_parse` 覆盖本次新增预设需要的一切（内嵌 JSON 解析、条数下限 ≥20、
  逐条 `validate_preset`、`%VAR%` 模板前缀、名字 / 模板 / 子路径唯一性），
  `preset_rules_filters_by_existence` 与 `preset_override` 集成用例同批通过。
* release 下前端资源经 Tauri（`frontendDist: ui`）压缩内嵌，所以在 exe 里搜
  `toolbar.disk` 这类 UI 文案是 0 命中 —— 属正常，不作为界面未内嵌的判断依据；
  界面行为仍以 `?mock=1` 预览取证为准。

## 2026-10-17（界面：语言切换改分段按钮 + 默认英文 + 切换即整片刷新）

### 切换控件与默认语言

* **默认英文**：首次启动不再跟随系统语言 —— 无 localStorage 记录时默认英文
  （公开发布以英文为默认界面）；手动切换照旧写入 localStorage 并跨重启保持。
  移除 `detectLang()`（`navigator.language` 探测）。
* **分段按钮取代下拉框**：顶栏「中文 | EN」并排两个按钮，当前语言白底高亮、
  半透明描边融入深蓝顶栏（原来的白色 `<select>` 在深色顶栏里很突兀）；
  带 `aria-pressed` 与 focus-visible 轮廓。
* **启动即同步后端语言**：`boot()` 里先 `invoke('set_locale', getLang())` 再拉快照 ——
  状态标签 / 横幅 / 水位消息由后端按当前语言生成，不推送会出现"界面英文、数据中文"。
* **切换即整片刷新**：`td-lang-change` 后 `set_locale → refresh(true)`，取代原来的
  只 `render()` —— 缓存快照里的后端文案还是旧语言，只重渲染会半中半英直到手动刷新。

### 工具

* `scripts/take-screens.cjs`：默认英文后，中文截图改用 `addInitScript` 预设
  localStorage(`tempdirector.locale`)，不再依赖浏览器 locale 探测。

### 验证

* 无头浏览器行为核验（全过）：无记录默认 `en`（首行标签 "Link OK"）→ 点「中文」
  立即整片切中（标签 "链接正常"、按钮 "运维日志"）→ 刷新保持 `zh` → 清 localStorage
  重回 `en`；分段按钮 active 态与 `aria-pressed` 同步。
* 八张截图（中英各四）用新控件重拍；`--check` 扫描英文模式无漏翻（预设下拉名为
  数据直出，与真机一致，截图里下拉收起不可见）。
* README 两版的语言切换说明同步更新（"右上角分段按钮切换，首次启动默认英文"）。

## 2026-10-17（i18n：后端 / CLI / 界面全量中英双语 + 英文截图 + README 双语）

### 后端 —— i18n 基础设施与全量接入

* **`td-core/src/i18n.rs`**：新增 `Locale` + `t()` / `tf()`（`{name}` 插值）与中英双字典，
  覆盖规则、引擎、搬运、占用、开机重建、提权、预设、水位、错误等全链路文案。
* **后端各模块接入**：`engine` / `fsops` / `link` / `boot` / `usecase` / `store` /
  `elevate` / `worker` / `portable` / `water` / `schedule` / `clean` 等（阶段 2 系列提交）。
* **CLI 双语**：全局 `--lang zh|en`（可放任意位置）与环境变量 `TEMPDIRECTOR_LANG`
  （优先级低于 `--lang`）；`help.rs` 双语帮助，`output.rs` 格式化器与 `timefmt.rs`
  时间单位全部键化（阶段 3 系列提交）。

### 界面 —— 语言切换、开机刷新与 mock 双语

* **`ui/i18n.js`**：语言字典与 `data-i18n` 批量替换；首次启动跟随系统语言
  （`navigator.language`，非中文一律回退英文），手动切换写入 localStorage；
  右上角新增「中文 / English」下拉。
* **修 bug**：`applyStatic()` 原先只在 `setLang()` 里调用，`loadLang()` 后不调用 ——
  非中文系统首次启动时静态 HTML 停留在中文默认值，要手动切一次语言才刷新。已在
  `loadLang()` 后补启动调用。
* **mock 假数据接入 i18n**（`?mock=1` 预览 / 无头截图路径）：规则名、状态标签、
  横幅、水位、autostart 消息、进度明细改走 i18n 键；中文取值与此前硬编码逐字一致，
  英文与 `td-core/src/i18n.rs` 措辞对齐；`set_locale` 时按新语言重算状态文案
  （名称是"用户数据"不重算，与真机一致）。新增 `rule.root_fmt` 键替换根下拉硬编码
  全角括号。
* **已知保留项**：预设名（`presets.json` 数据直出）与 ops/事件流水 mock 样本在英文
  模式仍是中文 —— 与真实后端行为一致，未本地化。

### 文档与工具

* **`docs/screenshots/`**：新增英文四张（main-window / add-rule / settings / progress），
  中文四张同源重刷，中英成套。
* **`scripts/take-screens.cjs`**：Playwright 无头截图工具（mock 数据 + `locale: en-US`
  直启英文），`--check` 模式全树扫漏翻（汉字 / 裸键名）。
* **README 中英双语**：`README.md`（英文为主）+ `README_ZH.md`（中文），顶部互挂切换
  链接，各自单语呈现。顺带修了两处 presets.json 路径（实际在 `crates/td-core/`，
  早前文档误写为 `td-gui`）。

### 验证

* `node --check` 通过（`app.js` / `i18n.js`）。
* 无头浏览器两种语言 × 7 个 mock 视图（主窗口 / 弹窗 / 设置 / 日志 / 进度 / 空间 /
  清理）扫描：零裸键名泄漏；英文模式除语言自称「中文」与预设数据外无汉字残留。
* 界面为静态文件 + mock 数据链路，未触碰 Rust 逻辑，无新增编译产物。

## 2026-10-16（界面：详情面板回底部 + 表头点击排序）

### 界面 —— 撤回右侧 drawer，补表头排序

* **详情面板改回表格下方常驻区块**：之前改成右侧滑出 drawer 是想"点行不推挤
  表格"，但实际使用里侧栏抢走横向空间、与表格割裂感更强；改回底部面板
  （`flex: none` + `border-top` + `max-height: 240px`），空时 `hidden` 不占位，
  用 `.hidden` 切换而非 drawer 的 `.open`/transform。撤掉 drawer 专属的 ✕ 关闭
  按钮（Esc 仍可收起）。
* **表头点击排序**（客户端、不写库 —— 持久顺序仍靠拖拽 `sort_order`）：
  状态 / 名称 / 分类 / 源路径 / 目标 / 旗标 六列可点表头排序，同列点击切换
  升降序、新列默认升序，当前列显示 ↑/↓。状态按"好→坏"（✅<⚠️<❌<🚫）排、
  旗标按期望开启优先再按开机重建。`key=null`（初始）保持后端顺序
  （`category, sort_order, id`）。

### 验证

* `node --check` 通过；`cargo test --workspace` 全绿（纯前端改动）；
  `index.html?mock=1` 预览核验（底部详情、表头 ↑/↓ 切换）。

## 2026-10-16（功能：一键恢复全部规则）

### 功能 —— panic 按钮：快回到"没有重定向"的世界

* 场景：卸载软件 / 卸载 RAM 盘 / 出问题要排查 —— 这时要的不是逐条勾选，
  而是**一个按钮**把所有重定向还原。
* 「更多 ▾」菜单新增**红色**「恢复全部规则…」：只认处于链接状态
  （✅/⚠️）的规则 —— plain_dir 本来就是真实目录，没什么可恢复；
  一条都没有时直接提示，不弹确认框。
* 确认弹窗说清楚三件事：影响范围（数据从 RAM 盘搬回原目录、删除联接）、
  后果（"当前重定向"归零、缓存重新写回 SSD）、后续（规则配置保留，
  想彻底停用需关期望或删规则）—— panic 时的确认必须让人看清在确认什么。
* 有受保护位置（需提权）的规则时**整批**走提权进程，只弹一次 UAC ——
  panic 场景少一次选择。

### 验证

* `node --check` 通过；`cargo test --workspace` 全绿（纯前端改动，复用
  已有 `restore_rules` / `run_elevated_job` 命令）；`index.html?mock=1`
  预览核验（红色菜单项、确认弹窗、4 条链接中规则的清单）。

## 2026-10-16（功能：定时低峰期自动清理）

### 功能 —— 例行维护，让缓存别攒到触发水位

* 与水位预授权自动清理**互补**：水位清理是**应急**（RAM 盘快满了才动、
  只处理 critical 卷）；定时清理是**例行** —— 每天到点（默认凌晨 03:00，
  可改可关）把**所有**配了 `auto_clean_policy` 的规则按各自策略清一遍。
* **共用同一套授权模型**：规则级 `auto_clean_policy`（用户显式授权过
  "这种文件可以自动删"），不发明新的删除策略 —— 没配策略的规则绝不动。
* **默认关闭**（破坏性操作必须显式 opt-in）；**每自然日最多一次**
  （`scheduled_clean_last_run` 记本地日期，当天跑过或没到点都不动）。
* 本地时间直接问 OS（`GetLocalTime` 自带年月日时分），绕开时区/DST 数学，
  不引 chrono。执行体注入 `LocalNow`，单测不用真等到凌晨三点。
* **GUI**：设置弹窗新增开关 + 时刻选择（`input[type=time]`）；前端 60s
  定时器与水位巡检同节拍调 `scheduled_clean_poll`；真的执行了才发系统通知
  （删了文件说"完成"、有失败说"有失败"—— 破坏性动作不能悄悄发生）。
* 策略不合法（手改库才会出现）逐条落 warn 事件跳过，不拖垮整轮清理。

### 验证

* `cargo test -p td-core scheduled_clean`：全生命周期单测（默认关闭 →
  未到点 → 到点执行且未授权规则不动 → 当天不重复 → 时刻格式坏跳过并落
  warn）通过；`hhmm_boundaries` 边界（23:59 合法、24:00/12:60 不合法）
  通过；`cargo test --workspace` 全绿；clippy 无告警；`node --check` 通过；
  `index.html?mock=1&settings=1` 预览核验。

## 2026-10-16（功能：规则拖拽排序）

### 功能 —— 表格行拖拽调整顺序，同分类内生效

* `sort_order` 列自建库就有、查询也按它排（`ORDER BY category, sort_order, id`），
  但一直没有写入口 —— 规则顺序实际只能靠插入顺序。本次补上**拖拽排序**。
* **限定同一分类内**：列表按 category 分组展示，跨分类拖拽在 `dragover`
  阶段就被拒绝（后端同样校验）—— 排序键里 category 优先，跨类拖了也会弹回去。
* **后端强制"完整覆盖该分类"**（`reorder_rules`）：`ordered_ids` 必须是该分类的
  全部规则 —— 界面正被筛选/搜索时前端也要从全量快照组装，否则隐藏规则保留旧
  `sort_order`、与重写的新值交错，顺序不可预期。这条不靠调用方自觉，数量对不上
  就拒绝。同时校验：id 存在、无重复、同类。
* `set_rules_order` 的多条 `UPDATE` 包在一个事务里：要么整批生效要么整批不变，
  不能出现"一半规则换了位置"的中间态。
* **前端**：勾选列最左加 `⋮⋮` 把手；只有抓着把手才把行设为 `draggable`
  （行常驻 draggable 会劫持文本选择）；落点按行的上/下半判定插前/插后；
  虚线框高亮合法落点；跨分类 / 拖回自己不响应。

### 验证

* `cargo test -p td-core reorder`：2 条单测（同分类重排生效；跨分类 / 重复 /
  子集 / 不存在 id 全被拒绝且顺序未破坏）通过；`cargo test --workspace` 全绿；
  clippy 无告警；`node --check` 通过；`index.html?mock=1` 预览核验拖拽。

## 2026-10-16（功能：效益统计）

### 功能 —— 三个可解释的数字，回答"这工具到底帮我省了什么"

* **只报可解释、不外推**：当前重定向到 RAM 盘的规模（这些写入本来会落 SSD）、
  历次 `apply` 累计搬运量、历次 `clean` 累计释放量。**不做**"预估节省 SSD 寿命
  X 年"这类外推 —— TLC 擦写次数、写入放大都是假设，估出来的数只会误导。
* 数据来源是 `operations` 表的**结构化列**（`file_count` / `bytes`）而非
  `events.message` 文本（文本聚合不了）。为此补齐缺口：`clean_run` 成功后与
  `apply` 一样落一条操作流水（`kind='clean'`，仅规则目标且确实删了文件时）。
* 口径**宁少算不虚报**：「当前重定向」只统计期望开启**且**上次检测链接正常
  （`link_ok` / `link_ok_empty`）的规则；期望关闭、断链、真实目录都不算。
  累计搬运 / 清理只统计 `phase='done'` 的成功终态。
* **GUI**：运维日志弹窗顶部加三卡统计条（与操作流水同源，放"回顾"语境而非首页）；
  读不到统计时静默移除该条，不挡日志查看。mock 层与后端口径一致地实时计算。
* **CLI**：新子命令 `tempdirector stats [--json]`（审计类，与 `events` / `ops` 为邻）。

### 验证

* `cargo test -p td-core stats`：口径单测 3 条（只计生效规则、空库零值、
  仅按 done 终态且按 kind 聚合）通过；`cargo test --workspace` 全绿（含 doc_sync
  对 lib.rs 分层表 / ARCHITECTURE 模块图的断言）；`cargo clippy --workspace
  --all-targets` 无告警；`node --check` 通过；`index.html?mock=1` 预览核验。

## 2026-10-16（界面：详情面板改为右侧滑出）

### 界面 —— 点行展开详情不再推挤表格，主场景保持稳定

* 详情面板从"表格下方的常驻区块"改为**右侧滑出 drawer**：此前点一行会把表格整体
  上推、视线要重新定位；滑出式布局下表格位置不动 —— 表格才是主场景。
  关闭态平移出屏幕外（`translateX(100%)`，不占位、不拦截点击），打开态滑入
  （`transform` 过渡 180ms）。层级 `z-index` 60：低于弹窗 70，高于表格粘性表头 2。
* 面板右上角加「✕」关闭按钮；**Esc 关闭详情**（弹窗开着时 Esc 只关弹窗，不顺手
  把详情一起收掉）。关闭时只移除 `.open`、**不清空内容**，滑出动画期间仍有内容可看。
* 关闭按钮做成 `.detail-head` 的**最后一个弹性项**（`margin-left: auto` 顶到最右）——
  顺序若在 chips 之前，chips 会被挤到它右边。

### 验证

* `node --check` 通过；`index.html?mock=1` 预览核验（滑入/滑出、✕ 与 Esc 关闭）；
  `cargo test --workspace` 全绿（纯前端改动）。

## 2026-10-16（界面：工具栏与表格减负）

### 界面 —— 一级操作区只留高频动作，两个布尔列合并

* **工具栏 11 → 8 个按钮**：「检测占用 / 空间统计 / 仅重建链接」收进「更多 ▾」下拉
  （都是诊断类低频动作，且单条入口在详情面板里已有）；「导出 / 导入」移除
  （设置弹窗底部本来就有这两个按钮，重复）；「执行开机重建」从顶栏移入同一菜单。
* **表格合并「开启」「开机重建」两列为一列「旗标」**（●=期望开启、⟳=开机重建），
  省一列横向空间；完整含义在详情面板 chips 与悬停 title 中保留。
* 下拉交互：点菜单项、或点任意外部区域即收起；`z-index` 30 < 详情面板 60 < 弹窗 70，
  菜单不会盖住弹窗。

### 验证

* `cargo test --workspace` 全绿（前端改动不涉及 Rust 逻辑）；`node --check` 通过。
* 表格 `thead` 与行内 `td` 均为 8 列（已逐项核对）；用 `index.html?mock=1` 预览核验。

## 2026-10-16（功能：托盘常驻 + 系统通知）

### 功能 —— 窗口关掉也守着 RAM 盘：关闭隐藏到托盘 + 关键事件推系统通知

* **关闭窗口 → 隐藏到托盘**（默认）：此前关闭即退出，水位巡检与预授权自动清理只在
  窗口开着时才跑，等于把刚做的"无人值守"能力废掉一半。现在关闭窗口隐藏到托盘、
  进程继续常驻：首次隐藏弹一次通知说明"仍在后台运行"，退出走托盘菜单。
  设置里可改为"直接退出"（settings 键 `close_to_tray`，默认开）；拿不到库锁时按
  **安全默认（驻留）**，避免一次瞬时锁竞争就把后台能力一起关掉。
* **系统通知（Rust 侧）**：接入 `tauri-plugin-notification`（精确 pin `=2.4.0`），
  走 Rust API 而非前端插件命令 —— 自定义命令不受 ACL 限制、无需 capabilities，
  且触发点都在后端（窗口可能已隐藏）。三类事件推送：
  1. 水位**新升级到 critical**（同根同级别不重复，降级恢复后可再次通知；warn 仍只走
     界面横幅 —— 通知给"窗口关着"的场景用，稀缺才有效）；
  2. **预授权自动清理真的删了文件**（破坏性动作不能悄悄发生）；
  3. **开机重建未完全成功**。
  通知失败一律静默，不影响巡检 / 清理 / 重建的主流程。
* 设置弹窗新增「关闭窗口时」开关（读 `ui_settings`、写 `set_close_to_tray`）；mock 层对齐。
* 通知去重逻辑抽成纯函数 `pick_new_critical`，脱离 Tauri 运行时即可测"通知疲劳"边界。

### 验证

* `cargo test --workspace` 全绿（td-gui 新增 2 个单测：通知去重边界、
  `close_to_tray_setting` 缺省与持久化）；clippy 0 告警；`node --check` 通过。
* 依赖影响：Cargo.lock 新增通知插件链路（tauri-winrt-notification / windows 0.62 系列），
  tauri 仍为 2.11.5，未破坏既有 pin。
* **已知边界**：未打包（开发态）时 Windows 可能不显示 Toast（需安装包提供带 AUMID 的
  开始菜单快捷方式），详见 ARCHITECTURE §11.9；此时以"落库 + 界面横幅"兜底。

## 2026-10-16（功能：水位越线一键清理）

### 功能 —— 水位横幅带「一键清理」入口 + CLI `water sweep` 批量清理

* **`Banner.action`（新字段，`#[serde(default)]`）**：横幅可携带动作标识，水位
  warn/critical 横幅带 `action: "sweep"`——看到"RAM 盘即将写满"时一步直达清理，
  不用在规则列表里逐条找。
* GUI：`renderBanners` 按动作渲按钮，「一键清理」**复用现成的批量清理对话框**
  （`openBatchCleanDialog`，先预演、看到清单再确认删除的两步语义不变）。
* CLI：`water sweep <策略> [--yes]`——对全部规则批量预演/执行，复用 `clean`
  子命令的策略解析（`build_clean_policy`/`describe_policy` 提为 `pub(crate)`），
  输出逐规则预演清单 + 汇总，加 `--yes` 才真删。

### 验证

* `cargo test --workspace` 全绿；clippy 0 告警；`node --check` 通过。
* CLI 冒烟：2 条规则预演 2 文件 → `--yes` 执行 → `clean` 事件逐条落库。

## 2026-10-16（功能：规则级水位预授权自动清理）

### 功能 —— critical 时按预授权策略自动清理，其余情况仍然只告警

* **schema v2→v3**：`rules.auto_clean_policy TEXT NOT NULL DEFAULT ''`
  （空 = 未启用 / `empty` 清空 / `older:<秒>` 只清早于该秒修改的）。
  迁移只对 `user_version < 3` 的老库执行，新库由 schema.sql 直接建全列；
  `v2_database_migrates_to_v3` 手工造 v2 库验证迁移无损。
* **`water.rs` 新增预授权执行**：
  * `parse_auto_policy` / `validate_auto_policy`：策略解析与入口校验
    （空串合法=未启用，仅"非空但解析不了"报错）；
  * `run_auto_clean`：**仅水位 critical** 时触发，只清「配了策略且目标根在
    critical 卷上」的规则；**30 分钟冷却**（settings 键 `water_last_auto_clean_at`，
    水位降不下去时不反复删）；每条走 `clean_run` 门面（规则 excludes 照常保护、
    事件照常落），另落一条 `auto_clean` 汇总事件说明"这次删是水位触发的"。
    由 `water_poll` 末尾调用，GUI 定时器 / CLI `water` / 计划任务都自动获得。
  * **修复**：首巡检直接 warn/critical（装完就写满）此前不落水位事件——
    现在视为从 `ok` 出发，越线即告警，只有首巡检 `ok` 才静默建基线。
* usecase：`RuleForm`/`RuleView` 加 `auto_clean_policy`（`#[serde(default)]`，
  老前端缺字段可反序列化，wire contract 测试钉住）；`validate_form` 拒绝坏策略。
* CLI：`rules add --auto-clean <策略>` 入口校验；`rules list` 规则行展示策略；
  help 补充说明。
* GUI：规则表单加「水位自动清理（预授权）」下拉（未启用 / 清空 / 只清早于
  N 小时修改的，映射 `''` / `empty` / `older:<秒>`）；详情面板友好文案展示；
  mock 层对齐（`mockRule` 带字段、增改持久化、示例数据可见）。

### 验证

* `cargo test --workspace` 全绿（td-core 155：策略解析单测、v2→v3 迁移、
  `water_critical_runs_preauthorized_auto_clean` 端到端——预授权规则被清、
  未授权绝不动、水位 error + clean + auto_clean 三类事件齐、冷却期内不重复）；
  clippy 0 告警；`node --check` 通过。
* CLI 冒烟：`--auto-clean older:3600` 入库并在 `rules list` 展示；
  `--auto-clean older:xx` 被入口校验拦截（exit 1 + 中文报错）。

## 2026-10-16（功能：RAM 盘水位监控）

### 功能 —— 目标根水位分级 + 阈值事件 + 全端展示（只告警，不自动删文件）

* **`usecase/water.rs`（新）**：水位 = 目标根所在卷已用百分比，默认阈值
  85%（warn）/ 95%（critical），settings 键 `water_warn_pct` / `water_critical_pct`
  可覆盖（0~100 之外的值视为未配置回落默认）。
  * `water_levels`：**只读**扫描（快照 / doctor 用，零副作用）；
  * `water_poll`：巡检——与 settings（`water_level_state`）记的上次级别对比，
    **级别变化才落事件**（升 warn→warn 事件、升 critical→error、降级→info 恢复），
    首次只建基线、平稳期零写入，不刷事件流水。
* **snapshot banner**：水位 warn/error 时概览顶部出横幅（写满 = 软件写缓存
  直接失败，必须顶在最显眼处）；只读，事件只归 `water_poll`。
* GUI：`water_poll` 命令 + 前端 60s 定时巡检（发现越线顺带刷新快照让横幅及时
  出现；失败静默下轮再试）；mock 层对齐。
* CLI：`water [--json]` 巡检命令（与 GUI 定时器同源，级别变化才落事件，
  适合计划任务低频跑）；`doctor` 加「RAM 盘水位」小节（只读）。
* **设计边界**：只告警不自动删文件——自动清理涉及破坏性决策，须用户显式触发
  （清理动作已有 `clean` 两步走，预演 + `--yes`）。

### 验证

* `cargo test --workspace` 全绿（td-core 150：新增 classify 边界单测 +
  `water_poll` 状态机端到端（settings 阈值覆盖驱动级别变化，不依赖真盘水位）
  + snapshot 横幅断言）；clippy 0 告警；`node --check` 通过。
* CLI 冒烟：无根空态、加根后 55.1% ok、连续两次巡检零事件，与去抖设计一致。

## 2026-10-16（功能：规则级清理排除）

### 功能 —— 每条规则可配「清理排除」模式，清理时不误删混在缓存里的重要数据

* **`fsops/exclude.rs`（新模块）**：fnmatch 子集的模式匹配（gitignore 的可解释子集）——
  纯名字模式（`sessions`）匹配**任意层级**；含分隔符模式（`logs/old`）从根**锚定**；
  `*` 不跨层、`**` 单独成段跨层、`?` 单字符；`\` 与 `/` 等价。校验：≤16 条、
  单条 ≤120 字符、禁止 `..` 与空模式。
* **设计边界（写进模块文档）**：排除**只影响 `clean`**，不改 apply/restore 的搬运语义
  （建链要求源路径腾空，搬运永远全量）。想让重要数据不进 RAM 盘，应收窄源路径，
  而不是依赖排除。
* `fsops/clean.rs`：`walk_files`/`plan`/`run` 均接收 `&Excludes`，用 `WalkDir::filter_entry`
  在遍历时剪掉命中子树；`CleanPlan`/`CleanReport` 新增 `excluded_skipped` 计数并进 summary。
* store：schema v1→v2 增量迁移（`ALTER TABLE rules ADD COLUMN excludes TEXT NOT NULL
  DEFAULT '[]'`，仅对 current==1 的老库执行，新库由 schema.sql 直接建全列）；坏 JSON
  容错为空数组。`Rule`/`NewRule` 带 `excludes` 字段，insert/update/导出导入链路全通。
* usecase：`RuleView`/`RuleForm`/`CleanPreviewView`/`CleanResultView` 加字段；
  `RuleForm.excludes` 带 `#[serde(default)]`，老前端缺字段可正常反序列化（wire contract
  测试钉住）。`clean` 用例解析规则时构建 `Excludes`（坏模式在入口就报错）。
* CLI：`rules add` 支持 `--exclude <模式>`（**可重复多条**，`parse.rs` 的 `Parsed` 从
  `HashMap<String, Option<String>>` 改为 `HashMap<String, Vec<String>>` 支持重复 flag，
  `value()` 取最后一个保持兼容）；`rules list` 规则行展示排除。**`clean` 子命令改为
  复用 usecase 的 `clean_preview`/`clean_run` 门面**（原先直连 `fsops::clean`，
  规则目标拿不到 excludes，且重复实现了 resolve 与事件落库）。
* GUI：规则表单加排除编辑框（textarea 一行一条），详情面板/清理预演（单个与批量）
  展示排除与 `excluded_skipped` 跳过计数；mock 层对齐真实 API 形状。

### 验证

* `cargo test --workspace --no-fail-fast` 全绿（td-core 147 + doc_sync 2 + td-cli 11
  + td-gui preset 2；exclude 模块 10 个单测、store roundtrip/坏 JSON 容错、
  usecase 端到端"排除子树保留"断言）。
* `node --check` 前端语法通过；`cargo clippy --workspace --all-targets` 0 告警。

## 2026-10-16（架构重构，续）

### 重构 —— `engine.rs`（1144 行）拆为 `engine/` 目录模块

* **`crates/td-core/src/engine.rs` → `engine/{mod,resolve,status,apply,restore,tests}.rs`**：
  - `resolve`：规则 → 源/目标路径（环境变量展开、安全预检、`clean_dir_for_rule`）
  - `status`：体检（`RuleStatus`、批量扫描、写库状态机键 `state_key`）
  - `apply`：应用（搬运/合并 → 建链，失败回滚）
  - `restore`：恢复（拆链搬回）与只重建链接（relink）
  - `mod.rs` 汇总再导出全部公开符号，**`crate::engine::X` 路径不变**，
    GUI/CLI/worker/boot 调用方零改动。
* `boot/acl.rs` 的 `export_apply_roundtrip` 测试改为**自建受保护 DACL 环境**：
  原先依赖 `%TEMP%` 继承链，回灌不带 protected 标志时父目录可继承 ACE 会并入
  回读结果，在本机环境必然失败。现用 `D:P` 探针（Everyone 完全控制，不依赖
  运行账号 SID）切断继承后验证往返，测试不再受宿主目录影响。
* **新增文档同步断言**（`crates/td-core/tests/doc_sync.rs`，2 个测试）：
  lib.rs 顶部分层表必须与 `pub mod` 声明**集合相等**，ARCHITECTURE.md §1
  三圈分工必须**覆盖**全部 `pub mod`。项目无 CI，断言随 `cargo test` 运行，
  新增/重命名/删除模块时忘改文档会直接红掉并指出差异。顺带修复了断言
  发现的既有漂移：lib.rs 分层表补 `error` / `paths` / `portable` 三行，
  ARCHITECTURE 三圈分工补 `error`。

### 验证

* `cargo test --workspace --no-fail-fast`：147 通过 / 0 失败（含 engine 11 个
  状态机/回滚测试逐字随迁、doc_sync 2 个新断言）。
* CLI `version` 冒烟输出不变；`cargo clippy --workspace --all-targets` 0 告警。
* doc_sync 断言经反向验证：临时从文档删模块名会红并报出缺失项。

## 2026-10-16（架构重构）

### 重构 —— td-core 引入用例门面层 `usecase`，GUI/CLI 收敛调用面

* **新增 `td-core::usecase`**（`crates/td-core/src/usecase/`）：按领域拆子模块
  （overview / rules / apply / lifecycle / presets / clean / disk / inspect / autostart /
  elevated / view），GUI、CLI、无界面子模式的**默认入口**；前端 JSON 契约结构
  （`RuleForm` / `UiSnapshot` / `CleanTarget` 等）统一定义在 `view.rs`。
  收益：core 内部模块重命名/拆分不再穿透前端；用例测试脱离 GUI crate 即可运行。
* **`td-gui/src/api.rs` 从 1818 行收敛为 8 行薄再导出**（`pub use td_core::usecase::*`），
  `commands.rs` 的 `api::Xxx` 符号路径不变；原 24 个界面业务单测随逻辑迁至
  `crates/td-core/src/usecase/tests.rs`，`cargo test -p td-core` 即可覆盖。
* `presets.json` 随预设逻辑从 `crates/td-gui/` 迁到 `crates/td-core/`（内嵌进 core），
  GUI 集成测试 `preset_override.rs` 走 `td_gui::api::preset_rules` 的真实链路不变。
* **`td-cli/src/main.rs` 从 1435 行拆为 6 个模块**：`parse.rs`（参数解析 + 新增 4 个单测）、
  `output.rs`（控制台格式化）、`timefmt.rs`（时间参数/相对时长 + 3 个单测随迁）、
  `help.rs`、`cmds/{roots,rules,status,apply,clean,boot,config}.rs`（子命令按域分组）。
  `boot-rebuild` / `export` / `autostart status|disable|run` 迁到 `usecase` 门面；
  `apply`/`restore`（需要 `engine::Outcome` 的规模与耗时明细）、`import`
  （app-tag 警告）保留低层调用并注明原因。CLI 行为与输出逐字不变（`--help` /
  `version` 冒烟核对）。
* `docs/ARCHITECTURE.md`：§1 补 usecase 门面与三圈分层、§2 数据流图加门面行、
  模块表与测试数（141→145）同步 `td-core/src/lib.rs`。

### 验证

* `cargo test --workspace --no-fail-fast`：144 通过、1 失败 —— 失败的是
  `boot::acl::tests::export_apply_roundtrip`，**在重构前的基线上同样失败**
  （`git stash` 对照验证过）：本机 `%TEMP%` 的 ACL 组成导致"导出 SDDL → 回灌 → 回读"
  的字符集合不一致，属环境敏感的既有问题，与本轮改动无关。
* 测试总数 141 → 145（净增 4 个 `parse.rs` 参数解析单测）。
* CLI 冒烟：`tempdirector version` / `--help` / 无参数输出与重构前一致。

## 2026-10-16

### 新增 —— 一键全量重建脚本 + 桌面快捷方式

* 根目录新增 `rebuild.bat`：`cargo clean` → `cargo build --release`（两个 exe 一起出）
  → 校验产物 → （重）建桌面快捷方式。`rebuild.bat noclean` 可跳过 clean 做增量构建。
* 复用 `scripts\cargo.cmd` 补 MSVC / cargo 环境（vcvars64、RUSTUP_HOME / CARGO_HOME 兜底），
  自身保持纯 ASCII（cmd.exe 按 OEM 码页读批处理，中文注释会解析崩溃）。
* 每次重建成功后（重）建 `%USERPROFILE%\Desktop\TempDirector.lnk`，指向新出炉的
  `target\release\tempdirector-gui.exe`，带应用图标、工作目录为 `target\release`。
  `cargo clean` 会清掉整个 `target\`，不刷新的话旧快捷方式会悬空。
* 双击运行时窗口停留等按键（Explorer 经 `cmd /c` 启动时检测），从已有控制台调用则直接退出。

### 验证

* `rebuild.bat noclean`：参数解析、产物校验、失败路径（exit 1 + 提示）均符合预期。
* 运行中 GUI 占用 `target\release` 时全量重建被明确拒绝（提示先托盘退出），
  实测确认过 `cargo clean` 在该状态下会删掉 exe 文件却删不掉目录（os error 32）。
* 桌面快捷方式创建实测通过：目标 / 工作目录 / 图标 / 描述四项属性均已写入 .lnk。
* **未在会话内完成全量 clean 构建验证**：本会话沙箱把 `reg.exe` 列入程序黑名单，
  而 `vcvars64.bat` 靠 `reg.exe` 查注册表定位 Windows SDK，被拦后 LIB 只剩 MSVC
  一侧、链接报 LNK1181 `kernel32.lib`。属环境限制而非脚本问题（10-14 的构建在
  普通终端正常）；脚本失败路径已带该排查提示。

## 2026-10-15

### 新增 —— M5 第一步：常用软件缓存目录预设（内置 + 外置覆盖）

「添加规则」弹窗顶部加了**常用预设**下拉，选中即自动填充名称 / 分类 / 源路径 /
目标子目录并触发实时校验，预设里的坑以警示提示展示。

**内置预设（24 条，真机逐个实测存在性与体积）**
* 分四类：浏览器（Chrome/Edge 的 Cache / Code Cache / GPUCache）、开发
  （npm 12.5G、uv 4.7G、pip、Cargo registry、VS Code 三件套、JetBrains 索引）、
  系统（用户 TEMP、NVIDIA/D3D 着色器缓存、崩溃转储、Steam 网页缓存）、
  办公（微信 radium\web、百度网盘、WPS、飞书）。
* 收录标准是**纯缓存/临时数据**（丢弃后软件自重建），明确排除 Docker Desktop
  的 `%LOCALAPPDATA%\Docker`（里面是 ext4.vhdx 虚拟磁盘本体）与 Zotero 数据目录。
* 下拉只列**本机实际存在源目录**的条目（逐条 `is_dir` 探测），没装的软件不出现。

**抽成配置文件 + 用户外置覆盖层**
* 预设从 Rust 常量表迁到 `crates/td-gui/presets.json`（`include_str!` 编译时内嵌），
  加内置预设只改 JSON、不动 Rust。
* 新增外置层 `%PROGRAMDATA%\TempDirector\presets.json`（便携模式为 `TEMPDIRECTOR_DATA_DIR`
  指向的目录），**装机后扩展无需重编译**。合并语义：`source_template` 相同（忽略大小写）
  的用户条目**覆盖**内置（改某条预设的默认子路径就这么干），其余**追加**。
* 容错：外置文件不存在 = 仅内置；语法坏 = 忽略整个文件；个别条目非法 = 只跳过该条，
  预设列表绝不整体失效。

### 验证

* 119 个测试全绿（td-core 95 + td-gui 22 + 集成 2）。
* 单元测试覆盖预设表完整性（分类合法 / 模板与子路径唯一 / TEMP 兜底）、合并语义、坏条目跳过。
* 集成测试 `crates/td-gui/tests/preset_override.rs` 走 `preset_rules()` 端到端真实链路：
  写外置文件 → `TEMPDIRECTOR_DATA_DIR` 指向临时数据目录 → 断言内置「用户 TEMP」被覆盖、
  且该源只此一条（覆盖而非追加）；另一条验证外置文件语法坏时回落内置。
* **并行坑（已修）**：同一测试二进制内两个 `#[test]` 默认并行，都改同一个进程级环境变量 →
  用 `ENV_LOCK` 串行化；且清理必须在锁内 + 唯一临时目录，否则一个测试的 `remove_dir_all`
  会在另一个持锁读取时误删共享文件、读到内置回落值（连跑 5 次验证稳定）。
* 界面：无头 Edge 实测添加弹窗——选「用户 TEMP」预设后四个字段全部自动填充，
  风险提示与展开路径实时校验正常。

## 2026-10-14

### 新增 —— M3：开机自启 + 按需提权 worker

**开机自启（用户级登录任务）**
* 用任务计划注册 `TempDirector-BootRebuild`：`LogonTrigger` + `InteractiveToken` +
  `LeastPrivilege` + `Priority 4`，**注册/查询/删除都不需要管理员权限**（已实测）。
* 任务动作是 `<GUI 路径> boot-rebuild` —— release 下 GUI 是 `windows_subsystem = "windows"`，
  登录时不会闪控制台窗口；CLI 保留控制台版本供人手动跑。两者共用同一份 `boot::run_boot_rebuild`。
* 便携模式（`TEMPDIRECTOR_DATA_DIR`）下注册时会把 `--data-dir "<目录>"` 写进任务参数：
  任务计划不继承 shell 环境变量，不带上就会"注册时用 A 库、登录时跑 B 库"。
* 重建报告落库（`settings.last_boot_report` / `last_boot_at`）+ 写 `logs/boot-rebuild.log`；
  界面下次启动时在顶部横幅提示「上次开机重建有需要留意的地方」——静默任务失败不会没人知道。

**提权 worker**
* `td-core/worker.rs`：job 协议（只含 rule id，**没有任何路径字段**）+ 执行体 + 结果文件。
  信任锚是 `validate_job_location`：job 必须在 `<数据目录>\jobs\` 之内，否则拒绝执行。
* `td-core/elevate.rs`：查令牌判提权状态；`ShellExecuteExW(runas)` 拉起自己并等退出码；
  用户点 UAC「否」翻译成明确中文提示（`ERROR_CANCELLED` → "提权被取消"）。
* CLI 与 GUI 都支持 `elevated <job>`；GUI 还在 Tauri 之前拦下 `boot-rebuild` / `elevated`
  两个子模式（`submode.rs`），保证登录任务不会拉起一个窗口。
* 界面：需要提权的规则带标记，批量应用时弹窗问「以管理员身份执行 / 只处理不需要权限的 / 取消」；
  详情面板也加了「以管理员身份应用」。

**修复（M3 过程中被测试与实测抓出的问题）**

* `validate_job_location` 忘了剥 `\\?\` 前缀：verbatim 路径与规范路径永远匹配不上，
  会导致 **worker 把自己写的合法 job 全部拒掉**（单元测试直接抓到）。
* `autostart::decode` 解 UTF-16LE 时没跳 BOM，解出来的字符串会以不可见的 U+FEFF 开头。
* `BootReport` / `RootOutcome` 只有 `Serialize`，界面要读回上次报告必须补 `Deserialize`。
* `submode` 里对已经是 `&str` 的变量再调 `.as_str()`，撞上未稳定的 `str_as_str`。
* 结果文件里的时间戳原来是 `unix:1791979173` —— 改成从库里取的本地时间（结果文件是给人看的）。

### 验证

* 104 个单元测试全绿（td-core 88 + td-gui 16）。
* 开机自启真机验证：`autostart enable` → `schtasks /Query /XML` 独立核验命令与参数 →
  `autostart run` 触发 → **目标目录被真实创建**、`logs/boot-rebuild.log` 落盘、
  **没有残留的界面进程**（证明子模式真的在 Tauri 之前被拦下）。
* 提权 worker 真机验证（非提权环境即可覆盖同一条代码路径）：
  正例 job → 退出码 0、结果文件写出、源目录变联接（`fsutil` 核验 `0xa0000003`）、通过链接可读；
  反例（job 放在 `jobs\` 之外）→ 退出码 1、结果文件写明拒绝原因、磁盘未被改动。
* 界面：无头 Edge 截图核验设置页新增的「开机自启」区块。

### 新增 —— M2：Tauri 2 桌面界面

`td-gui`（`tempdirector-gui.exe`）。**内含零链接逻辑** —— 全部走 `td-core`，
这一层只是一层壳，界面本身是静态文件、编译时内嵌。

**分层**
* `api.rs`：业务层，只吃 `&Store`、吐 serde 结构体，**不依赖任何 Tauri 类型**，
  因此能脱离窗口做单元测试（12 个）；`#[tauri::command]` 只是薄转发。
* `commands.rs`：状态锁、错误转字符串、进度事件、目录/文件选择框（走 dialog 插件的 Rust API，
  前端不依赖 npm 包）、在资源管理器里定位。
* `tray.rs`：托盘菜单（显示主窗口 / 立即执行开机重建 / 退出），重建动作丢后台线程，不卡界面。
* `lib.rs`：启动顺序 —— 打开规则库（失败则降级成内存库并在界面顶部横幅报错，不静默退出）
  → `mark_stale_ops` 清理上次中断的流水 → 装托盘。

**界面能力**
* 列表：状态徽标（✅ / ⚠️ / ❌ / 🚫）、按状态给的原因直接标在名称下、源与目标并排、两个开关指示灯。
* 检索筛选：搜索（名称/源路径/目标子目录）、分类、只看已开启、只看需处理、全选当前结果。
* 批量：应用 / 恢复 / 开启 / 关闭 / 检测占用 / 仅重建链接 / 删除。
* 详情面板：源模板与展开后路径、链接类型、警告、上次应用时间与规模 + 单条操作按钮。
* 添加/编辑表单：实时校验源路径（展开结果、缺失变量、是否受保护位置）与目标位置（根是否就绪、是否已有内容）。
* 进度浮层：消费后端 `td-progress` 事件，批量操作不会"点了没反应"。
* 运维日志：事件时间线 + 操作流水两个标签页。
* 空态、错误横幅、Toast 提示齐全；`?mock=1` 提供内存假数据便于单独预览与截图核验。

**工程取舍**
* 前端不引 npm / 打包器：`withGlobalTauri` + `window.__TAURI__.core.invoke`（带
  `__TAURI_INTERNALS__` 回退）。CSP 用 `style-src 'self'`，所以**不写内联 style 属性、不注入 `<style>`**，
  动态样式只走 CSSOM；弹窗只接受 DOM 节点、不用 `innerHTML`。
* 图标由 `scripts/make_icon.py` 纯 PIL 绘制生成（多档 PNG + 16→256 全帧 ICO），可重跑。
* `Cargo.toml` 精确 pin 了整个 tauri 家族版本（见 README 说明）。

### 修复（M2 过程中被实测抓出的问题）

* **tauri 家族版本漂移导致 27 个编译错误**：只 pin `tauri` 本体不够，`tauri-codegen/macros/utils/runtime`
  是范围依赖会各自升级，宏展开引用了本体不存在的错误变体。用 `cargo update --precise` 对齐到已验证组合。
* `MutexGuard<Store>` 传给 `&Store` 参数需要在调用点显式 `&*lock_store(...)`（16 处）。
* 徽标文案错误：真实目录显示成「❌ 联接」，自相矛盾 → 改为按状态给后缀（联接 / 符号链接 / 未链接 / 外来）。
* 弹窗 `body` 之前支持字符串走 `innerHTML`，存在注入面 → 收敛为只接受 DOM 节点。
* 设置页「添加目标根」行里两个按钮被 flex 撑成一大条 → 只让路径输入框拉满。

### 验证

* 87 个单元测试全绿（td-core 75 + td-gui 12）。
* 界面渲染：无头 Edge 逐页截图核验（列表 / 进度 / 表单 / 设置 / 运维日志），
  本机 Edge 无头模式需要 `--no-sandbox` 与 `--user-data-dir`。
* **真机启动验证**：用 CLI 造出 ✅1 / ⚠️1 / ❌1 混合状态的真实库，启动 `tempdirector-gui.exe`，
  截取桌面确认窗口正常打开、内嵌前端加载、样式生效（CSP 未拦截）、IPC 通（显示的是真实库数据）。

### 新增 —— M1：链接引擎内核 + CLI

`td-core`（库）+ `tempdirector`（CLI，同时是未来的提权 worker 与开机重建执行体）。

**链接引擎**
* 重解析点识别：读 `FSCTL_GET_REPARSE_POINT` 取 tag，区分 Junction（`0xa0000003`）/
  目录符号链接（`0xa000000c`）/ 外来重解析点（只报告不处理）。
* 建链默认走 **目录 Junction**（免管理员权限），仅在 UNC 目标时才退化为目录符号链接。
* 建链后**自检回读** tag 与目标路径，API 报成功但实际不可用的情形会当场暴露。
* 拆链只删链接条目本身，返回原目标路径；对真实目录一律拒绝。

**安全搬运**
* 同卷走 `rename`（原子、保留全部元数据）；跨卷走 复制 → 核对 → 删源。
* 核对不变式：文件数 + 总字节数（可选逐文件 SHA-256）；核对通过前不删源。
* 长路径全程 `\\?\`；复制前查可用空间；只读文件删除前清属性。
* 合并模式：目标非空时按"同路径取修改时间较新者"合并，并在合并后断言源侧每个条目都在目标中存在。

**占用检测**
* 两层设计：先用 `CreateFileW(share=0)` 独占打开扫出被占用文件（廉价、无数量限制），
  再用 Restart Manager 只对那批文件查出**是哪个进程/服务**占着，供 UI 提示"请关闭 Chrome"。
* RM 调用固定丢到独立线程并显式 `CoInitializeEx(MTA)`，不污染调用方的 COM 公寓。
* 瞬时占用（杀软实时扫描）有重试退避，避免误报。

**编排层**
* `apply` 决策树覆盖九种状态：幂等、断链修复、源不存在直接建链、搬运、合并、外来重解析点拒绝等。
* 失败一定回滚（搬回 / 重建空目录 / 把链接建回去），回滚也失败则留 `needs_manual` 流水。
* `restore` 支持目标有内容（搬回）与目标为空（拆链 + 建空目录）两种路径。

**规则库（SQLite，schema v1）**
* `roots` / `rules` / `operations` / `events` / `settings`；`user_version` 做迁移版本号。
* 规则存**未展开**的路径模板，运行时按当前用户展开；`source_norm` 做去重键。
* `want_enabled`（用户期望态）与 `last_state`（实时真相）分开存。
* 非终态的操作流水即"上次中断待修复"清单，启动时 `mark_stale_ops` 清理。

**开机守护**
* 轮询等待卷就绪（存在 + 能查容量 + 能写入），超时报明确错误而不是静默失败。
* 按目标层级从浅到深批量建目录；DACL（SDDL）导出与回灌已实现并有往返测试。
* 软件更新把链接覆盖成真实目录时**不自动搬运**，只报警告。

**配置迁移**
* JSON 导入导出，目标根用 `root_ref` 引用；支持 `{RAM_ROOT}` 占位符 + `--ram-root` 重绑。
* 导入幂等（按展开后的源路径去重），单条失败不中断整批。

**CLI**
`doctor` / `roots` / `rules` / `status` / `locks` / `apply` / `restore` / `relink` /
`boot-rebuild` / `volume` / `export` / `import` / `events` / `ops`，均支持 `--json` 与 `--dry-run`（适用者）。

### 修复（开发过程中被测试与实测抓出的问题）

* `junction::create` 参数顺序是 `(target, junction)`，最初传反导致 `os error 183`。
* 裸盘符目标根（`R:`）经 `PathBuf::join` 会丢掉分隔符得到 `R:xxx` → 统一在入口处规范化成 `R:\`。
* 目录树统计把根目录本身算进"子目录数"，源侧与目标侧口径不一致 → 跳过 depth 0。
* `LIKE ... ESCAPE '\'` 模式下搜索关键词里的反斜杠未转义，导致按路径搜索永远匹配不上。
* 非 raw 字符串里的 `\t` / `\r` 被当转义字符（测试语法通过但语义错误）→ 统一改转义写法。
* `relink` 的检查顺序会给出误导性错误（"目标不存在"而真正问题是"源目录里有数据"）→ 调整为先做安全判定。
* 开机重建报告的"新建目录"与"本就正常"同时计数，语义不清 → 拆成 新建目录 / 链接自动恢复 / 链接重建 / 本就正常。

### 验证

* 75 个单元测试全绿。
* 真机端到端：同卷重定向（71ms）、跨卷重定向（25ms）、模拟 RAM 盘重启 → 开机重建 → 写入成功、
  恢复（有内容 / 无内容两种）、导入导出往返、操作流水与事件审计。
* 用 `cmd dir /AL` 与 `fsutil reparsepoint query` 做独立于本程序的交叉验证（确认为 Mount Point 重解析点）。

### 已知缺口（下一阶段）

* GUI / 托盘 / 桌面通知（M2–M3）。
* 按需提权的一次性 worker（M3，设计已定，见 `docs/ARCHITECTURE.md` §7）。
* Windows 服务 + 登录事件触发重建 + 命名管道 IPC + 巡检自愈（M4）。
* 内置预设规则库、NSIS 安装包（M5）。
* `apply` 成功后自动导出源目录 DACL 存入 `acl_sddl`（机制已在，接线待做）。
