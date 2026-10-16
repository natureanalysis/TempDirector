//! 运行时多语言文案（中 / 英）。
//!
//! 设计原则：**文案在产生它的那一层按当前语言解析**，而不是把某一语言的原文
//! 透传到上层再翻译。调用方只持有稳定的键（如 `"err.io"`），当前语言由进程内
//! 全局状态 [`set_locale`] 决定：
//!
//! * GUI：前端切换语言后经 `set_locale` 命令写回这里，随后重新拉取数据；
//! * CLI：`--lang` / `TEMPDIRECTOR_LANG` / 系统区域设置在启动时写入。
//!
//! 词典用 `match` 实现：零依赖、编译器可查缺键、无需任何初始化。
//! 文案里的占位符写作 `{name}`，由 [`tf`] 负责插值。

use std::sync::atomic::{AtomicU8, Ordering};

/// 支持的界面语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    Zh,
    En,
}

impl Locale {
    /// 稳定的语言标签（写配置 / 传前端用）。
    pub const fn tag(self) -> &'static str {
        match self {
            Locale::Zh => "zh",
            Locale::En => "en",
        }
    }

    /// 从标签 / 系统区域串解析；无法识别返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("zh") {
            Some(Locale::Zh)
        } else if s.starts_with("en") {
            Some(Locale::En)
        } else {
            None
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::Zh
    }
}

const ZH: u8 = 0;
const EN: u8 = 1;

static CURRENT: AtomicU8 = AtomicU8::new(ZH);

/// 设置当前语言。
pub fn set_locale(locale: Locale) {
    CURRENT.store(
        match locale {
            Locale::Zh => ZH,
            Locale::En => EN,
        },
        Ordering::Relaxed,
    );
}

/// 读取当前语言。
pub fn locale() -> Locale {
    if CURRENT.load(Ordering::Relaxed) == EN {
        Locale::En
    } else {
        Locale::Zh
    }
}

/// 查一条静态文案，返回当前语言版本。未收录的键返回空串。
pub fn t(key: &str) -> &'static str {
    match locale() {
        Locale::Zh => zh(key),
        Locale::En => en(key),
    }
}

/// 查一条带占位符的文案并插值。占位符写作 `{name}`，`args` 给出 `("name", 值)`。
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(key).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

fn zh(key: &str) -> &'static str {
    match key {
        "err.io" => "IO 错误：{e}",
        "err.db" => "数据库错误：{e}",
        "err.json" => "JSON 处理错误：{e}",
        "err.invalid_path" => "路径非法：{e}",
        "err.safety" => "安全检查未通过：{e}",
        "err.busy" => "被占用：{e}",
        "err.no_space" => "目标卷空间不足：需要 {need}，可用 {free}",
        "err.verify" => "数据核对失败：{e}",
        "err.elevation" => "需要管理员权限：{e}",
        "err.not_found" => "找不到记录：{e}",
        "io.failed" => "{action}失败：{err}{hint}（路径：{path}）",
        "hint.busy" => "（文件正被其他程序占用，请先关闭相关软件）",
        "hint.denied" => "（访问被拒绝，该路径需要管理员权限）",
        "hint.not_exist" => "（路径不存在）",
        "hint.no_space" => "（磁盘空间不足）",
        "hint.too_long" => "（路径过长，已启用 \\\\?\\ 前缀仍失败请缩短目录层级）",
        "hint.not_reparse" => "（不是重解析点）",
        "hint.none" => "",
        "link.type.junction" => "目录联接",
        "link.type.symlink" => "目录符号链接",
        "link.state.source_missing" => "源路径不存在",
        "link.state.plain_dir" => "未链接（真实目录）",
        "link.state.plain_file" => "路径是文件，规则配置有误",
        "link.state.target_missing" => "目标丢失",
        "link.state.target_empty" => "目标为空（RAM 盘刚重启）",
        "link.state.target_ok" => "正常",
        "link.state.mismatch" => "，且指向与登记不符",
        "link.state.link_ok" => "{type} → {target}（{state}{extra}）",
        "link.state.link_broken" => "{type} 断裂，目标不可达：{target}",
        "link.state.foreign" => "外来重解析点（tag {tag}），非本工具管理",
        "link.state.access_denied" => "访问被拒绝（需要管理员权限）",
        "link.state.io_error" => "检测出错：{message}",
        "verify.mode.count" => "文件数 + 总字节数",
        "verify.mode.sha256" => "逐文件 SHA-256",
        "inspect.state.link_ok_empty" => "链接已就绪，目标为空（开机重建后的正常形态）",
        "inspect.state.link_ok" => "链接正常",
        "banner.boot_problem" => "上次开机重建（{at}）有需要留意的地方：{summary}",
        "banner.no_roots" => "还没有配置目标根（RAM 盘），先在「设置」里添加一个，例如 R:",
        "banner.all_roots_down" => "所有目标根都不可用 —— RAM 盘可能没有创建或没有挂载，此时无法建立链接",
        "banner.needs_attention" => "有 {n} 条规则需要处理（断链 / 真实目录 / 外来重解析点）",
        "banner.open_ops" => "有 {n} 条上次中断的操作未收尾，请查看「运维」面板确认后处理",
        "banner.root_error" => "目标根 #{id} {path} 异常：{message}",
        "banner.disk_critical" => "RAM 盘即将写满：{label}（{path}）已用 {pct}%，仅剩 {free} —— 建议清理或缩小规则",
        "banner.disk_warn" => "RAM 盘水位偏高：{label}（{path}）已用 {pct}%，剩余 {free}",
        "cat.browser" => "浏览器",
        "cat.design" => "设计",
        "cat.dev" => "开发",
        "cat.system" => "系统",
        "cat.media" => "影音",
        "cat.office" => "办公",
        "cat.game" => "游戏",
        "cat.other" => "其他",
        "move.method.rename" => "同卷重命名",
        "move.method.copy" => "跨卷复制",
        "autostart.unregistered" => "未注册（登录时不会自动重建）",
        "autostart.registered" => "已注册",
        "autostart.registered_cmd" => "已注册：{cmd}",
        "autostart.disabled" => "已取消开机自启",
        "autostart.not_registered" => "该任务本来就没有注册",
        "autostart.triggered" => "已触发一次开机重建任务",
        "water.message.ok" => "已用 {pct}%，剩余 {free}",
        "water.message.zero" => "卷容量为 0，无法判断水位",
        "water.message.unavailable" => "卷不可用（未挂载？），无法读取水位",
        "disk.role.ram" => "RAM 盘",
        "disk.role.target" => "目标盘",
        "disk.role.source" => "源盘",
        "worker.boot_rebuild" => "开机重建",
        "volume.not_created" => "卷已就绪，但目标根目录还没建出来：{path}（RAM 盘重启后的正常现象，运行一次开机重建即可）",
        "volume.missing" => "目标卷不存在：{path}",
        "volume.not_dir" => "目标路径不是目录：{path}",
        "volume.ready" => "就绪，可用空间 {free}",
        "volume.readonly" => "不可写入：{e}",
        "volume.no_space_info" => "无法查询容量：{e}",
        "volume.wait_timeout" => "等待目标卷就绪超时（已等 {secs} 秒）：{path}。RAM 盘未创建、未格式化，或创建它的程序还没启动。",
        "volume.create_failed" => "创建目标根目录失败（{path}）：{e}。若是 RAM 盘尚未挂载，请先创建 RAM 盘。",
        "io.action.elev_request" => "请求管理员权限",
        "io.action.restore_acl" => "回灌目录权限",
        "io.action.read_dir" => "读取目录",
        "io.action.read_link_target" => "读取链接目标",
        "io.action.check_target" => "检查目标目录",
        "io.action.create_symlink" => "创建目录符号链接",
        "io.action.read_link_status" => "读取链接状态（可能链接已不存在）",
        "io.action.delete_link" => "删除链接",
        "io.action.create_dir" => "创建目录",
        "io.action.clean_empty_source" => "清理空源目录",
        "io.action.read_source" => "读取源目录",
        "io.action.create_parent" => "创建目标父目录",
        "io.action.clean_empty_target" => "清理空目标目录",
        "io.action.rename" => "同卷重命名",
        "io.action.create_target" => "创建目标目录",
        "io.action.create_subdir" => "创建子目录",
        "io.action.copy_file" => "复制文件",
        "io.action.read_delete_dir" => "读取待删除目录",
        "io.action.read_hash_file" => "读取待哈希文件",
        "io.action.query_free_space" => "查询可用空间",
        "io.action.query_volume" => "查询卷容量",
        "io.action.merge_file" => "合并文件",
        "clean.stats.empty" => "没有符合条件的文件",
        "clean.stats.count" => "{n} 个文件 / {size}",
        "clean.report.deleted" => "已删除 {n} 个文件 / {size}",
        "clean.report.pruned" => "，剪除 {n} 个空目录",
        "clean.report.links" => "；{n} 个链接未触碰",
        "clean.report.excluded" => "；{n} 个条目按排除规则保留",
        "clean.report.skipped" => "；{n} 个文件被占用或无权限已跳过（例如：{examples}）",
        "blocker.service" => "{app}（服务，PID {pid}）",
        "blocker.process" => "{app}（PID {pid}）",
        "lock.clear" => "未发现占用（已扫描 {n} 个文件）",
        "lock.unknown" => "无法确定具体进程",
        "lock.locked" => "{n} 个文件被占用{trunc}，占用者：{who}",
        "lock.truncated" => "（已截断）",
        "lock.denied" => "；另有 {n} 个文件无权限访问，需要以管理员身份重试",
        "tree.summary" => "{files} 个文件 / {size} / {dirs} 个子目录{links}",
        "tree.links" => " / {n} 个链接",
        "boot.one_line" => "目标卷 {ready}/{total} 就绪；规则 {rules} 条：新建目录 {dirs}，链接自动恢复 {recovered}，链接重建 {repaired}，本就正常 {ok}，告警 {warn}，失败 {failed}（耗时 {ms}）",
        "apply.already_ok" => "链接已就绪且缓存有内容，无需操作",
        "apply.already_ok_empty" => "链接已就绪（目标为空属正常：RAM 盘重启后缓存会自然重建）",
        "apply.mismatch" => "链接实际指向 {actual}，与登记的目标 {target} 不一致。请先执行「恢复」再重新应用。",
        "apply.foreign" => "源路径是外来重解析点（tag {tag}，可能是云盘占位符），本工具不会触碰它：{path}",
        "apply.is_file" => "源路径是文件而不是目录，请修正规则：{path}",
        "apply.access_denied" => "读取源目录被拒绝，请以管理员身份重试：{path}",
        "apply.dry_rebuild_target" => "将重建丢失的目标目录",
        "apply.target_rebuilt" => "目标目录 {target} 已重建，链接恢复可用",
        "apply.target_rebuilt_done" => "已重建目标目录 {target}，链接恢复可用（原缓存内容随 RAM 盘重启丢失，属预期）",
        "apply.dry_create_link" => "将创建目标目录并直接建立链接",
        "apply.linked_no_source" => "源目录原本不存在，已直接建立链接（目标目录{created}）",
        "apply.created" => "已创建",
        "apply.exists" => "已存在",
        "apply.linked_no_source_done" => "源目录不存在，已直接建立链接；目标目录就绪：{target}",
        "apply.contradiction" => "链接状态自相矛盾（链接可用但目标状态为缺失）：{path}",
        "apply.target_non_empty" => "目标目录已有内容：{path}。加 --merge 可以把源目录合并进去（同路径文件取修改时间较新者）后建链。",
        "apply.source_busy" => "源目录正被占用，已中止（未做任何改动）：{summary}",
        "apply.dry_merge" => "将合并进已有目标目录后建链",
        "apply.dry_move" => "将搬运到目标目录后建链",
        "apply.merged" => "已合并 {n} 个文件进目标目录（覆盖 {over} 个较旧文件，跳过 {skip} 个不较新的）",
        "apply.rollback_ok" => "{e}；回滚：{rb}",
        "apply.rollback_log" => "{name}：{e}（已回滚：{rb}）",
        "apply.rollback_fail" => "{e}；回滚失败：{rb}",
        "apply.rollback_fail_log" => "{name}：{e}；且回滚失败：{rb}。请人工检查 {src} 与 {tgt}",
        "apply.applied_log" => "{name}：{note}",
        "apply.done_note" => "{note}；已建链 {src} → {tgt}",
        "apply.rb_merged" => "已把源目录重建为空目录（合并进 RAM 盘的内容保留）",
        "apply.rb_source_exists" => "源目录仍存在，无需处理",
        "apply.rb_target_empty" => "目标目录为空，无需搬回",
        "apply.rb_source_exists2" => "源目录已存在，跳过搬回",
        "apply.rb_moved_back" => "已把 {summary} 搬回源目录",
        "restore.already_plain" => "本来就是真实目录，无需恢复",
        "restore.source_missing" => "源路径不存在，无需恢复",
        "restore.foreign" => "源路径是外来重解析点（tag {tag}），本工具不会触碰它",
        "restore.is_file" => "源路径是文件，无法恢复：{path}",
        "restore.dry_move_back" => "将拆掉链接并把 RAM 盘中的内容搬回原位",
        "restore.dry_empty" => "将拆掉链接并重建空的原目录",
        "restore.rollback_ok" => "{e}；已把链接建回，状态未变",
        "restore.rollback_log" => "{name}：{e}（已把链接建回）",
        "restore.rollback_fail" => "{e}；且链接未能建回：{rb}",
        "restore.rollback_fail_log" => "{name}：{e}；且链接未能建回：{rb}。RAM 盘内容仍在 {tgt}，原路径 {src}",
        "restore.restored_log" => "{name}：{plan}（{summary}）",
        "restore.done_moved" => "已恢复：{summary} 已搬回 {src}",
        "restore.done_empty" => "已拆掉链接并重建空目录：{src}",
        "restore.relink_already" => "链接已存在，无需重建",
        "restore.relink_blocked" => "源是含内容的真实目录（{path}），不能直接覆盖成链接。请改用 apply（会先搬运/合并）。",
        "restore.relink_no_target" => "目标目录不存在，无法只重建链接：{path}",
        "restore.relink_log" => "{name}：链接已重建 → {target}",
        "restore.relink_done" => "链接已重建：{src} → {target}",
        "safety.volume_root" => "拒绝把整个卷根作为缓存目录：{path}",
        "safety.forbidden_exact" => "该路径属于系统关键目录，禁止重定向：{path}",
        "safety.forbidden_prefix" => "该路径位于系统保留区（{prefix}），禁止重定向：{path}",
        "safety.elevated" => "该路径位于受保护位置，移动/建链需要管理员权限：{path}",
        "safety.cloud" => "该路径疑似在云同步目录内：云盘客户端会用重解析点做占位符，与目录链接冲突，强烈不建议重定向",
        "safety.temp" => "重定向整个 TEMP 目录影响面较大（安装程序、待重启的更新会依赖它），建议逐软件配置",
        "safety.long_path" => "路径较长，部分老程序（未声明 longPathAware）仍可能受 260 字符限制影响",
        "safety.both_empty" => "源路径与目标路径都不能为空",
        "safety.must_absolute" => "源路径与目标路径必须是绝对路径（支持 %VAR% 模板，但展开后必须绝对）",
        "safety.same" => "源路径与目标路径相同：{s}",
        "safety.target_in_source" => "目标目录位于源目录内部（{t} 在 {s} 内），会形成自环",
        "safety.source_in_target" => "源目录位于目标目录内部（{s} 在 {t} 内），会形成自环",
        "db.root_not_in_lib" => "规则库中没有 id={id} 的目标根",
        "db.root_path_empty" => "目标根路径不能为空",
        "db.root_not_found" => "没有 id={id} 的目标根",
        "db.root_in_use" => "还有 {used} 条规则在引用这个目标根，请先删除或改绑这些规则",
        "db.rule_not_found" => "没有 id={id} 的规则",
        "db.unresolved_vars" => "源路径模板里有无法解析的环境变量：{vars}",
        "db.must_be_absolute" => "源路径必须是绝对路径（可用 %VAR% 模板）",
        "db.duplicate_rule" => "已存在指向同一路径的规则：{norm}",
        "db.duplicate_rule_other" => "另一条规则（#{id}）已经指向该路径",
        "form.name_empty" => "名称不能为空",
        "form.path_empty" => "路径不能为空，例如 R:",
        "form.rule_name_empty" => "规则名称不能为空",
        "form.source_empty" => "源路径不能为空",
        "form.unknown_category" => "未知分类 {cat}，可用：{list}",
        "form.subpath_empty" => "目标子目录不能为空（它是 RAM 盘下的相对路径，例如 browsers\\chrome）",
        "form.bad_link_type" => "链接类型只能是 junction 或 symlink_dir",
        "form.duplicate_id" => "顺序列表里 id={id} 出现了不止一次",
        "form.cross_category" => "只能在同一分类内调整顺序：{name}（#{id}）属于 {cat}，其它属于 {other}",
        "form.incomplete_order" => "顺序列表必须覆盖分类 {category} 的全部规则（{given}/{total}）",
        "form.reordered_log" => "调整了分类 {category} 的 {n} 条规则顺序",
        "form.acl_no_source" => "源目录不存在，无法导出权限：{path}",
        "form.acl_captured_log" => "{name}：已保存目录权限模板",
        _ => "",
    }
}

fn en(key: &str) -> &'static str {
    match key {
        "err.io" => "I/O error: {e}",
        "err.db" => "Database error: {e}",
        "err.json" => "JSON error: {e}",
        "err.invalid_path" => "Invalid path: {e}",
        "err.safety" => "Safety check failed: {e}",
        "err.busy" => "In use: {e}",
        "err.no_space" => "Not enough space on target volume: need {need}, free {free}",
        "err.verify" => "Data verification failed: {e}",
        "err.elevation" => "Administrator privileges required: {e}",
        "err.not_found" => "Record not found: {e}",
        "io.failed" => "{action} failed: {err}{hint} (path: {path})",
        "hint.busy" => " (the file is held by another program; close it first)",
        "hint.denied" => " (access denied; this path needs administrator privileges)",
        "hint.not_exist" => " (path does not exist)",
        "hint.no_space" => " (not enough disk space)",
        "hint.too_long" => " (path too long; even with the \\\\?\\ prefix, shorten the directory depth)",
        "hint.not_reparse" => " (not a reparse point)",
        "hint.none" => "",
        "link.type.junction" => "Directory junction",
        "link.type.symlink" => "Directory symbolic link",
        "link.state.source_missing" => "Source path does not exist",
        "link.state.plain_dir" => "Not linked (real directory)",
        "link.state.plain_file" => "Path is a file; rule is misconfigured",
        "link.state.target_missing" => "Target missing",
        "link.state.target_empty" => "Target empty (RAM disk just restarted)",
        "link.state.target_ok" => "OK",
        "link.state.mismatch" => "; target differs from registered",
        "link.state.link_ok" => "{type} → {target} ({state}{extra})",
        "link.state.link_broken" => "{type} broken, target unreachable: {target}",
        "link.state.foreign" => "Foreign reparse point (tag {tag}), not managed by this tool",
        "link.state.access_denied" => "Access denied (administrator privileges required)",
        "link.state.io_error" => "Detection error: {message}",
        "verify.mode.count" => "File count + total bytes",
        "verify.mode.sha256" => "Per-file SHA-256",
        "inspect.state.link_ok_empty" => "Link ready, target empty (normal after boot rebuild)",
        "inspect.state.link_ok" => "Link OK",
        "banner.boot_problem" => "Last boot rebuild ({at}) has issues to review: {summary}",
        "banner.no_roots" => "No target root (RAM disk) configured; add one in \"Settings\", e.g. R:",
        "banner.all_roots_down" => "All target roots unavailable — RAM disk may not be created or mounted; cannot create links",
        "banner.needs_attention" => "{n} rules need attention (broken links / real directories / foreign reparse points)",
        "banner.open_ops" => "{n} interrupted operations pending; check the \"Ops\" panel to review",
        "banner.root_error" => "Target root #{id} {path} abnormal: {message}",
        "banner.disk_critical" => "RAM disk nearly full: {label} ({path}) {pct}% used, only {free} free — clean up or shrink rules",
        "banner.disk_warn" => "RAM disk water level high: {label} ({path}) {pct}% used, {free} free",
        "cat.browser" => "Browser",
        "cat.design" => "Design",
        "cat.dev" => "Development",
        "cat.system" => "System",
        "cat.media" => "Media",
        "cat.office" => "Office",
        "cat.game" => "Game",
        "cat.other" => "Other",
        "move.method.rename" => "Same-volume rename",
        "move.method.copy" => "Cross-volume copy",
        "autostart.unregistered" => "Not registered (won't auto-rebuild on login)",
        "autostart.registered" => "Registered",
        "autostart.registered_cmd" => "Registered: {cmd}",
        "autostart.disabled" => "Autostart disabled",
        "autostart.not_registered" => "Task was not registered",
        "autostart.triggered" => "Triggered a boot rebuild task",
        "water.message.ok" => "{pct}% used, {free} free",
        "water.message.zero" => "Volume capacity is 0, cannot determine water level",
        "water.message.unavailable" => "Volume unavailable (not mounted?), cannot read water level",
        "disk.role.ram" => "RAM disk",
        "disk.role.target" => "Target disk",
        "disk.role.source" => "Source disk",
        "worker.boot_rebuild" => "Boot rebuild",
        "volume.not_created" => "Volume ready but target root not yet created: {path} (normal after RAM disk restart; run boot rebuild once)",
        "volume.missing" => "Target volume does not exist: {path}",
        "volume.not_dir" => "Target path is not a directory: {path}",
        "volume.ready" => "Ready, free space {free}",
        "volume.readonly" => "Not writable: {e}",
        "volume.no_space_info" => "Cannot query capacity: {e}",
        "volume.wait_timeout" => "Timed out waiting for target volume ({secs}s): {path}. RAM disk not created, not formatted, or its creator not started.",
        "volume.create_failed" => "Failed to create target root directory ({path}): {e}. If the RAM disk is not mounted, create it first.",
        "io.action.elev_request" => "Request administrator privileges",
        "io.action.restore_acl" => "Restore directory permissions",
        "io.action.read_dir" => "Read directory",
        "io.action.read_link_target" => "Read link target",
        "io.action.check_target" => "Check target directory",
        "io.action.create_symlink" => "Create directory symbolic link",
        "io.action.read_link_status" => "Read link status (link may not exist)",
        "io.action.delete_link" => "Delete link",
        "io.action.create_dir" => "Create directory",
        "io.action.clean_empty_source" => "Clean empty source directory",
        "io.action.read_source" => "Read source directory",
        "io.action.create_parent" => "Create target parent directory",
        "io.action.clean_empty_target" => "Clean empty target directory",
        "io.action.rename" => "Same-volume rename",
        "io.action.create_target" => "Create target directory",
        "io.action.create_subdir" => "Create subdirectory",
        "io.action.copy_file" => "Copy file",
        "io.action.read_delete_dir" => "Read directory to delete",
        "io.action.read_hash_file" => "Read file to hash",
        "io.action.query_free_space" => "Query free space",
        "io.action.query_volume" => "Query volume capacity",
        "io.action.merge_file" => "Merge file",
        "clean.stats.empty" => "No matching files",
        "clean.stats.count" => "{n} files / {size}",
        "clean.report.deleted" => "Deleted {n} files / {size}",
        "clean.report.pruned" => ", pruned {n} empty directories",
        "clean.report.links" => "; {n} links untouched",
        "clean.report.excluded" => "; {n} entries kept by exclude rules",
        "clean.report.skipped" => "; {n} files skipped (in use or no permission, e.g.: {examples})",
        "blocker.service" => "{app} (service, PID {pid})",
        "blocker.process" => "{app} (PID {pid})",
        "lock.clear" => "No locks found (scanned {n} files)",
        "lock.unknown" => "Cannot identify process",
        "lock.locked" => "{n} files locked{trunc}, held by: {who}",
        "lock.truncated" => " (truncated)",
        "lock.denied" => "; {n} files denied access, retry as administrator",
        "tree.summary" => "{files} files / {size} / {dirs} subdirs{links}",
        "tree.links" => " / {n} links",
        "boot.one_line" => "Volumes {ready}/{total} ready; rules {rules}: dirs created {dirs}, links recovered {recovered}, links repaired {repaired}, already ok {ok}, warnings {warn}, failed {failed} ({ms})",
        "apply.already_ok" => "Link ready with cached content; nothing to do",
        "apply.already_ok_empty" => "Link ready (empty target is normal: cache rebuilds after RAM disk restart)",
        "apply.mismatch" => "Link actually points to {actual}, differs from registered target {target}. Run \"Restore\" first, then re-apply.",
        "apply.foreign" => "Source is a foreign reparse point (tag {tag}, possibly a cloud placeholder); this tool won't touch it: {path}",
        "apply.is_file" => "Source path is a file, not a directory; fix the rule: {path}",
        "apply.access_denied" => "Source directory access denied; retry as administrator: {path}",
        "apply.dry_rebuild_target" => "Would rebuild the missing target directory",
        "apply.target_rebuilt" => "Target directory {target} rebuilt; link restored",
        "apply.target_rebuilt_done" => "Rebuilt target directory {target}; link restored (original cache lost with RAM disk restart, expected)",
        "apply.dry_create_link" => "Would create target directory and link directly",
        "apply.linked_no_source" => "Source directory did not exist; linked directly (target {created})",
        "apply.created" => "created",
        "apply.exists" => "already exists",
        "apply.linked_no_source_done" => "Source directory did not exist; linked directly; target ready: {target}",
        "apply.contradiction" => "Link state contradicts itself (link usable but target state missing): {path}",
        "apply.target_non_empty" => "Target directory not empty: {path}. Add --merge to merge the source into it (same-path files: keep newer mtime) then link.",
        "apply.source_busy" => "Source directory in use; aborted (no changes made): {summary}",
        "apply.dry_merge" => "Would merge into existing target directory then link",
        "apply.dry_move" => "Would move to target directory then link",
        "apply.merged" => "Merged {n} files into target (overwrote {over} older files, skipped {skip} not-newer)",
        "apply.rollback_ok" => "{e}; rolled back: {rb}",
        "apply.rollback_log" => "{name}: {e} (rolled back: {rb})",
        "apply.rollback_fail" => "{e}; rollback failed: {rb}",
        "apply.rollback_fail_log" => "{name}: {e}; rollback also failed: {rb}. Manually check {src} and {tgt}",
        "apply.applied_log" => "{name}: {note}",
        "apply.done_note" => "{note}; linked {src} → {tgt}",
        "apply.rb_merged" => "Rebuilt source as empty directory (merged content kept in RAM disk)",
        "apply.rb_source_exists" => "Source directory still exists; nothing to do",
        "apply.rb_target_empty" => "Target directory empty; nothing to move back",
        "apply.rb_source_exists2" => "Source directory already exists; skipped move-back",
        "apply.rb_moved_back" => "Moved {summary} back to source directory",
        "restore.already_plain" => "Already a real directory; nothing to restore",
        "restore.source_missing" => "Source path does not exist; nothing to restore",
        "restore.foreign" => "Source is a foreign reparse point (tag {tag}); this tool won't touch it",
        "restore.is_file" => "Source path is a file; cannot restore: {path}",
        "restore.dry_move_back" => "Would remove link and move RAM disk content back",
        "restore.dry_empty" => "Would remove link and rebuild empty source directory",
        "restore.rollback_ok" => "{e}; link rebuilt, state unchanged",
        "restore.rollback_log" => "{name}: {e} (link rebuilt)",
        "restore.rollback_fail" => "{e}; link could not be rebuilt: {rb}",
        "restore.rollback_fail_log" => "{name}: {e}; link could not be rebuilt: {rb}. RAM disk content still in {tgt}, original path {src}",
        "restore.restored_log" => "{name}: {plan} ({summary})",
        "restore.done_moved" => "Restored: {summary} moved back to {src}",
        "restore.done_empty" => "Removed link and rebuilt empty directory: {src}",
        "restore.relink_already" => "Link already exists; nothing to rebuild",
        "restore.relink_blocked" => "Source is a real directory with content ({path}); cannot overwrite with a link. Use apply (will move/merge first).",
        "restore.relink_no_target" => "Target directory does not exist; cannot rebuild link: {path}",
        "restore.relink_log" => "{name}: link rebuilt → {target}",
        "restore.relink_done" => "Link rebuilt: {src} → {target}",
        "safety.volume_root" => "Cannot use an entire volume root as cache directory: {path}",
        "safety.forbidden_exact" => "This path is a critical system directory; redirection forbidden: {path}",
        "safety.forbidden_prefix" => "This path is in a system-reserved area ({prefix}); redirection forbidden: {path}",
        "safety.elevated" => "This path is in a protected location; moving/linking needs administrator privileges: {path}",
        "safety.cloud" => "This path may be inside a cloud-sync directory: cloud clients use reparse points as placeholders, which conflict with directory links; redirection strongly discouraged",
        "safety.temp" => "Redirecting the entire TEMP directory has broad impact (installers and pending updates depend on it); configure per-software instead",
        "safety.long_path" => "Path is long; some legacy programs (not longPathAware) may still hit the 260-character limit",
        "safety.both_empty" => "Source and target paths cannot both be empty",
        "safety.must_absolute" => "Source and target paths must be absolute (supports %VAR% templates, but expanded form must be absolute)",
        "safety.same" => "Source and target paths are the same: {s}",
        "safety.target_in_source" => "Target is inside source ({t} within {s}); would form a loop",
        "safety.source_in_target" => "Source is inside target ({s} within {t}); would form a loop",
        "db.root_not_in_lib" => "No target root with id={id} in the rule library",
        "db.root_path_empty" => "Target root path cannot be empty",
        "db.root_not_found" => "No target root with id={id}",
        "db.root_in_use" => "{used} rules still reference this target root; delete or rebind them first",
        "db.rule_not_found" => "No rule with id={id}",
        "db.unresolved_vars" => "Source path template has unresolved environment variables: {vars}",
        "db.must_be_absolute" => "Source path must be absolute (can use %VAR% templates)",
        "db.duplicate_rule" => "A rule already points to the same path: {norm}",
        "db.duplicate_rule_other" => "Another rule (#{id}) already points to this path",
        "form.name_empty" => "Name cannot be empty",
        "form.path_empty" => "Path cannot be empty, e.g. R:",
        "form.rule_name_empty" => "Rule name cannot be empty",
        "form.source_empty" => "Source path cannot be empty",
        "form.unknown_category" => "Unknown category {cat}; valid: {list}",
        "form.subpath_empty" => "Target subdirectory cannot be empty (it is a relative path under the RAM disk, e.g. browsers\\chrome)",
        "form.bad_link_type" => "Link type must be junction or symlink_dir",
        "form.duplicate_id" => "id={id} appears more than once in the order list",
        "form.cross_category" => "Can only reorder within the same category: {name} (#{id}) is {cat}, others are {other}",
        "form.incomplete_order" => "Order list must cover all rules in category {category} ({given}/{total})",
        "form.reordered_log" => "Reordered {n} rules in category {category}",
        "form.acl_no_source" => "Source directory does not exist; cannot export permissions: {path}",
        "form.acl_captured_log" => "{name}: saved directory permission template",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_tags() {
        assert_eq!(Locale::parse("zh-CN"), Some(Locale::Zh));
        assert_eq!(Locale::parse("zh"), Some(Locale::Zh));
        assert_eq!(Locale::parse("en-US"), Some(Locale::En));
        assert_eq!(Locale::parse("EN"), Some(Locale::En));
        assert_eq!(Locale::parse("fr"), None);
    }

    #[test]
    fn switch_changes_text() {
        set_locale(Locale::Zh);
        assert_eq!(t("err.busy"), "被占用：{e}");
        set_locale(Locale::En);
        assert_eq!(t("err.busy"), "In use: {e}");
        set_locale(Locale::Zh);
    }

    #[test]
    fn interpolates_placeholders() {
        set_locale(Locale::Zh);
        let s = tf("io.failed", &[("action", "搬运"), ("err", "boom"), ("hint", ""), ("path", "C:\\x")]);
        assert_eq!(s, "搬运失败：boom（路径：C:\\x）");
        set_locale(Locale::En);
        let s = tf("io.failed", &[("action", "move"), ("err", "boom"), ("hint", ""), ("path", "C:\\x")]);
        assert_eq!(s, "move failed: boom (path: C:\\x)");
        set_locale(Locale::Zh);
    }
}