//! 文件夹清理：清空 / 按修改时间批量删除。给缓存目录瘦身的工具。
//!
//! ## 与 `mover::delete_tree` 的三点不同（都是刻意的）
//!
//! * **只删文件，永远保留根目录**：清理语义是"腾空"，不是"抹掉这个目录"。
//!   软件多半还指着这个目录，删了根目录它未必会自己建回来。
//! * **占用/失败跳过而不是中止**：缓存目录里永远有几个文件被进程握着，
//!   清理要"能删多少删多少"，把删不掉的如实汇报，而不是整单失败。
//! * **删完自底向上剪空目录**：只删文件会剩一地空壳；但**只剪因删除而变空的**，
//!   有东西的目录一律保留。
//!
//! ## 时间口径
//!
//! 核心**只认 Unix 秒**，不做任何时区/日历判断 —— "昨天 0 点"、"1 小时前"、
//! "某个时间段"都由前端按用户本地时区算成秒传进来，展示时也用前端 `Date` 还原成
//! 本地时间。这样核心不引时区依赖，也不会两边各算一次产生偏差。
//!
//! ## 铁律（与全工程一致）
//!
//! * 不跟随重解析点：树内的链接（OneDrive 占位符、别人建的联接）**既不删也不进**，
//!   计入 `links_skipped` 如实汇报。根目录本身是链接则直接拒绝 ——
//!   那是 TempDirector 自己管的链接，清理它得走"规则指向的真实目录"那条路。
//! * 长路径全程 `\\?\` verbatim；只读文件先清属性再删。
//!
//! ## 排除（[`Excludes`]）
//!
//! 规则可带排除模式（`sessions`、`logs/old` 这类），枚举与剪空目录都跳过
//! 命中的子树，计入 `excluded_skipped` 如实汇报。语义与限制见
//! [`exclude`](super::exclude) 模块文档 —— 排除**只影响清理**，不改搬运。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{io_ctx, TdError, TdResult};
use crate::fsops::exclude::Excludes;
use crate::fsops::mover::Progress;
use crate::util;

/// 清理策略。时间一律用 Unix 秒（前端按本地时区算好传进来）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CleanPolicy {
    /// 清空：删除目录下全部文件。
    Empty,
    /// 删除修改时间**早于** `cutoff` 的文件（"昨天及更早"、"1 小时前"都是它的实例）。
    Before { cutoff: i64 },
    /// 删除修改时间落在 [`from`, `to`) 内的文件（"指定时间段"，左闭右开）。
    Between { from: i64, to: i64 },
}

impl CleanPolicy {
    /// 校验策略参数，挡掉前端传来的明显非法值（负数、起止倒置）。
    pub fn validate(&self) -> TdResult<()> {
        match self {
            CleanPolicy::Empty => Ok(()),
            CleanPolicy::Before { cutoff } => {
                if *cutoff <= 0 {
                    return Err(TdError::InvalidPath("清理时间点非法".into()));
                }
                Ok(())
            }
            CleanPolicy::Between { from, to } => {
                if *from <= 0 || *to <= *from {
                    return Err(TdError::InvalidPath(
                        "时间段非法：起点须为正、且早于终点".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn matches(&self, mtime: i64) -> bool {
        match self {
            CleanPolicy::Empty => true,
            CleanPolicy::Before { cutoff } => mtime < *cutoff,
            CleanPolicy::Between { from, to } => mtime >= *from && mtime < *to,
        }
    }
}

/// 当前 Unix 秒（供 GUI 层给"1 小时前"这类相对量做基准，避免前端时钟与系统不一致时无从校准）。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 预演结果：将要删什么、删多少。执行前必须先给用户看这个。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanPlan {
    pub files: u64,
    pub bytes: u64,
    /// 树内链接数：不删、不进入，仅汇报。
    pub links_skipped: u64,
    /// 被排除模式跳过的条目数（目录命中即整棵子树跳过）。
    pub excluded_skipped: u64,
    /// 样本（最多 20 条），给人确认用。mtime 为 Unix 秒，前端还原本地时间。
    pub sample: Vec<CleanSampleEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanSampleEntry {
    pub path: String,
    pub bytes: u64,
    pub mtime: i64,
}

impl CleanPlan {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "没有符合条件的文件".to_string();
        }
        format!("{} 个文件 / {}", self.files, util::human_bytes(self.bytes))
    }
}

/// 执行结果。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanReport {
    pub deleted_files: u64,
    pub deleted_bytes: u64,
    pub dirs_pruned: u64,
    /// 被进程占用 / 权限不足而删不掉的，已跳过并如实汇报。
    pub skipped: u64,
    pub skip_examples: Vec<String>,
    /// 树内链接数（刻意不碰）。
    pub links_skipped: u64,
    /// 被排除模式跳过的条目数。
    pub excluded_skipped: u64,
}

impl CleanReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "已删除 {} 个文件 / {}",
            self.deleted_files,
            util::human_bytes(self.deleted_bytes)
        );
        if self.dirs_pruned > 0 {
            s += &format!("，剪除 {} 个空目录", self.dirs_pruned);
        }
        if self.links_skipped > 0 {
            s += &format!("；{} 个链接未触碰", self.links_skipped);
        }
        if self.excluded_skipped > 0 {
            s += &format!("；{} 个条目按排除规则保留", self.excluded_skipped);
        }
        if self.skipped > 0 {
            s += &format!(
                "；{} 个文件被占用或无权限已跳过（例如：{}）",
                self.skipped,
                self.skip_examples.join("；")
            );
        }
        s
    }
}

/// 一条候选文件。预演与执行共用 [`walk_files`] + 同一 `matches` 判断，
/// 保证"看到的"与"删掉的"绝不两套口径。
struct Candidate {
    path: PathBuf,
    bytes: u64,
    mtime: i64,
}

/// 枚举根目录下**全部普通文件**（递归、不跟随链接），带 mtime 与大小。
/// 链接根目录直接拒绝（那是链接，应清理它指向的真实目录）。
/// 命中排除模式的目录整棵子树不进入，计数如实汇报。
fn walk_files(
    dir: &Path,
    excludes: &Excludes,
    links_skipped: &mut u64,
    excluded_skipped: &mut u64,
) -> TdResult<Vec<Candidate>> {
    let root_md = fs::symlink_metadata(dir).map_err(|e| io_ctx(e, dir, "读取目录"))?;
    if root_md.file_type().is_symlink() {
        return Err(TdError::SafetyBlocked(format!(
            "该目录本身是链接（联接/符号链接），请清理它指向的真实目录：{}",
            dir.display()
        )));
    }
    if !root_md.is_dir() {
        return Err(TdError::InvalidPath(format!("不是目录：{}", dir.display())));
    }

    let vroot = util::to_verbatim(dir);
    let mut out = Vec::new();
    for entry in WalkDir::new(&vroot)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let rel = rel_path(&vroot, e.path());
            if !excludes.is_empty() && excludes.matches(&rel) {
                *excluded_skipped += 1;
                return false;
            }
            true
        })
    {
        let entry = match entry {
            Ok(e) => e,
            // 单个条目读不到（权限/竞态删除）不该让整棵树的枚举报废
            Err(_) => continue,
        };
        if entry.depth() == 0 {
            continue;
        }
        let ft = entry.file_type();
        if ft.is_symlink() {
            *links_skipped += 1;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            // 拿不到 mtime：按"最新"处理，让它躲过时间条件（宁可漏删不误删）；
            // Empty 仍会删它。
            .unwrap_or(i64::MAX);
        out.push(Candidate {
            path: util::simplify(entry.path()),
            bytes: md.len(),
            mtime,
        });
    }
    Ok(out)
}

/// 子条目相对根目录的路径（统一 `/` 分隔，供排除模式匹配）。
fn rel_path(root: &Path, child: &Path) -> String {
    child
        .strip_prefix(root)
        .unwrap_or(child)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 预演：只算不删。
pub fn plan(dir: &Path, policy: &CleanPolicy, excludes: &Excludes) -> TdResult<CleanPlan> {
    policy.validate()?;
    let mut links = 0u64;
    let mut excluded = 0u64;
    let files = walk_files(dir, excludes, &mut links, &mut excluded)?;
    let picked: Vec<&Candidate> = files.iter().filter(|f| policy.matches(f.mtime)).collect();
    Ok(CleanPlan {
        files: picked.len() as u64,
        bytes: picked.iter().map(|f| f.bytes).sum(),
        links_skipped: links,
        excluded_skipped: excluded,
        sample: picked
            .iter()
            .take(20)
            .map(|f| CleanSampleEntry {
                path: f.path.display().to_string(),
                bytes: f.bytes,
                mtime: f.mtime,
            })
            .collect(),
    })
}

/// 执行清理。`dry_run` 时不产生任何写入，只回报将要删除的量。
pub fn run(
    dir: &Path,
    policy: &CleanPolicy,
    excludes: &Excludes,
    dry_run: bool,
    sink: &mut dyn FnMut(Progress),
) -> TdResult<CleanReport> {
    policy.validate()?;
    let mut links = 0u64;
    let mut excluded = 0u64;
    let files = walk_files(dir, excludes, &mut links, &mut excluded)?;
    let picked: Vec<&Candidate> = files.iter().filter(|f| policy.matches(f.mtime)).collect();
    let planned_bytes: u64 = picked.iter().map(|f| f.bytes).sum();
    sink(Progress::Enumerated {
        files: picked.len() as u64,
        bytes: planned_bytes,
    });

    let mut report = CleanReport {
        links_skipped: links,
        excluded_skipped: excluded,
        ..Default::default()
    };
    if dry_run {
        report.deleted_files = picked.len() as u64;
        report.deleted_bytes = planned_bytes;
        return Ok(report);
    }

    for f in &picked {
        let vpath = util::to_verbatim(&f.path);
        let outcome = match fs::remove_file(&vpath) {
            Ok(()) => Ok(()),
            // 只读文件（error 5）清掉属性再试一次
            Err(e) if e.raw_os_error() == Some(5) => {
                clear_readonly(&f.path);
                fs::remove_file(&vpath)
            }
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => {
                report.deleted_files += 1;
                report.deleted_bytes += f.bytes;
                if report.deleted_files % 200 == 0 {
                    sink(Progress::Deleting {
                        files: report.deleted_files,
                    });
                }
            }
            Err(_) => {
                report.skipped += 1;
                if report.skip_examples.len() < 3 {
                    report.skip_examples.push(
                        f.path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| f.path.display().to_string()),
                    );
                }
            }
        }
    }

    // 自底向上剪空目录（只剪因本次删除而真的空了的；链接目录与被排除的目录不碰）。
    report.dirs_pruned = prune_empty_dirs(dir, excludes)?;
    sink(Progress::Done {
        files: report.deleted_files,
        bytes: report.deleted_bytes,
    });
    Ok(report)
}

// 仅 Windows：此处在 Unix 上会有 world-writable 顾虑，本工具不支持 Unix。
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly(path: &Path) {
    if let Ok(md) = fs::symlink_metadata(path) {
        let mut perm = md.permissions();
        if perm.readonly() {
            perm.set_readonly(false);
            let _ = fs::set_permissions(path, perm);
        }
    }
}

/// 自底向上删除空子目录（不含根）。以**实时**状态判断是否为空，
/// 天然只剪"删完文件后真的空了"的目录；反复扫描直到没有新的可剪（处理嵌套空壳）。
/// 被排除模式命中的目录（及其子树）一律保留。
fn prune_empty_dirs(dir: &Path, excludes: &Excludes) -> TdResult<u64> {
    let vroot = util::to_verbatim(dir);
    let mut pruned = 0u64;
    loop {
        let mut removed_this_pass = false;
        let mut dirs: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(&vroot)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || excludes.is_empty()
                    || !excludes.matches(&rel_path(&vroot, e.path()))
            })
        {
            match entry {
                Ok(e) => {
                    if e.depth() > 0 && e.file_type().is_dir() {
                        dirs.push(util::simplify(e.path()));
                    }
                }
                Err(_) => continue,
            }
        }
        // 深的在前：子目录先剪，父目录下一轮才可能变空。
        dirs.sort_by_key(|a| std::cmp::Reverse(a.components().count()));
        for d in dirs {
            let vpath = util::to_verbatim(&d);
            let is_empty = match fs::read_dir(&vpath) {
                Ok(mut rd) => rd.next().is_none(),
                Err(_) => false,
            };
            if is_empty && fs::remove_dir(&vpath).is_ok() {
                pruned += 1;
                removed_this_pass = true;
            }
        }
        if !removed_this_pass {
            return Ok(pruned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;

    fn fixture(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("td_clean_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn put(dir: &Path, rel: &str, content: &[u8], mtime_unix: i64) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        filetime::set_file_mtime(&p, FileTime::from_unix_time(mtime_unix, 0)).unwrap();
    }

    /// 固定时间轴：以 now 为基准造出 2 天前 / 2 小时前 / 1 分钟前 三档。
    struct Axis {
        now: i64,
        two_days_ago: i64,
        two_hours_ago: i64,
        one_minute_ago: i64,
    }
    fn axis() -> Axis {
        let now = now_secs();
        Axis {
            now,
            two_days_ago: now - 2 * 86400,
            two_hours_ago: now - 7200,
            one_minute_ago: now - 60,
        }
    }

    #[test]
    fn empty_policy_deletes_all_but_keeps_root() {
        let base = fixture("empty");
        let t = axis();
        put(&base, "a.bin", b"11111", t.one_minute_ago);
        put(&base, "sub/b.bin", b"222", t.two_days_ago);

        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &Excludes::default(), false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 2);
        assert_eq!(rep.deleted_bytes, 8);
        assert!(base.is_dir(), "根目录必须保留");
        assert!(!base.join("sub").exists(), "变空的子目录应被剪掉");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn before_policy_only_hits_older_files() {
        let base = fixture("before");
        let t = axis();
        put(&base, "old.bin", b"aa", t.two_days_ago);
        put(&base, "mid.bin", b"bbb", t.two_hours_ago);
        put(&base, "new.bin", b"c", t.one_minute_ago);

        let cutoff = t.now - 86400; // "昨天及更早" ≈ 24h 前
        let p = plan(&base, &CleanPolicy::Before { cutoff }, &Excludes::default()).unwrap();
        assert_eq!(p.files, 1, "只有 2 天前的文件命中");
        assert_eq!(p.bytes, 2);

        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Before { cutoff }, &Excludes::default(), false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 1);
        assert!(base.join("mid.bin").exists());
        assert!(base.join("new.bin").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn between_policy_is_left_closed_right_open() {
        let base = fixture("between");
        let t = axis();
        put(&base, "out_old.bin", b"1", t.two_days_ago);
        put(&base, "edge_from.bin", b"22", t.two_hours_ago);
        put(&base, "inside.bin", b"333", t.two_hours_ago + 600);
        put(&base, "edge_to.bin", b"4444", t.one_minute_ago);

        let p = CleanPolicy::Between {
            from: t.two_hours_ago,
            to: t.one_minute_ago,
        };
        assert_eq!(plan(&base, &p, &Excludes::default()).unwrap().files, 2, "from 闭、to 开");
        let mut sink = |_| {};
        let rep = run(&base, &p, &Excludes::default(), false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 2);
        assert!(base.join("out_old.bin").exists(), "时间段之前的不受影响");
        assert!(base.join("edge_to.bin").exists(), "to 边界不删");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dry_run_changes_nothing() {
        let base = fixture("dry");
        let t = axis();
        put(&base, "a.bin", b"xxxx", t.two_days_ago);
        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &Excludes::default(), true, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 1, "预演也要报出将要删的量");
        assert_eq!(rep.deleted_bytes, 4);
        assert!(base.join("a.bin").exists(), "预演不得动磁盘");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn plan_summary_and_sample() {
        let base = fixture("summary");
        let p = plan(&base, &CleanPolicy::Empty, &Excludes::default()).unwrap();
        assert!(p.is_empty());
        assert_eq!(p.summary(), "没有符合条件的文件");

        let t = axis();
        put(&base, "x/y.bin", b"12345678", t.two_days_ago);
        let p = plan(&base, &CleanPolicy::Empty, &Excludes::default()).unwrap();
        assert_eq!(p.files, 1);
        assert_eq!(p.sample.len(), 1);
        assert_eq!(p.sample[0].mtime, t.two_days_ago, "样本带 mtime 供前端还原");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn nonempty_dirs_survive() {
        let base = fixture("keepdirs");
        let t = axis();
        put(&base, "mixed/gone.bin", b"1", t.two_days_ago);
        put(&base, "mixed/stays.bin", b"2", t.now);
        let mut sink = |_| {};
        run(&base, &CleanPolicy::Before { cutoff: t.now - 3600 }, &Excludes::default(), false, &mut sink)
            .unwrap();
        assert!(base.join("mixed").is_dir(), "还有文件的目录不能剪");
        assert!(base.join("mixed/stays.bin").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn nested_empty_dirs_pruned_transitively() {
        let base = fixture("nested");
        let t = axis();
        put(&base, "a/b/c/deep.bin", b"1", t.two_days_ago);
        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &Excludes::default(), false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 1);
        assert_eq!(rep.dirs_pruned, 3, "a/b/c 三层空壳都应被剪");
        assert!(base.is_dir(), "根目录保留");
        assert!(!base.join("a").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_reparse_root_and_missing_dir() {
        let base = fixture("refuse");
        assert!(plan(&base.join("nope"), &CleanPolicy::Empty, &Excludes::default()).is_err());

        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        crate::link::create::create_dir_link(&link, &real, crate::link::LinkType::Junction)
            .unwrap();
        let err = plan(&link, &CleanPolicy::Empty, &Excludes::default()).unwrap_err();
        assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
        assert!(real.exists(), "拒绝时绝不能碰链接指向的真实目录");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn readonly_files_are_cleared_and_deleted() {
        let base = fixture("readonly");
        put(&base, "ro.bin", b"abcd", now_secs() - 999999);
        let p = base.join("ro.bin");
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm).unwrap();

        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &Excludes::default(), false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 1, "只读文件清掉属性后应删除");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn policy_validate_rejects_bad_times() {
        assert!(CleanPolicy::Empty.validate().is_ok());
        assert!(CleanPolicy::Before { cutoff: 100 }.validate().is_ok());
        assert!(CleanPolicy::Before { cutoff: 0 }.validate().is_err());
        assert!(CleanPolicy::Before { cutoff: -5 }.validate().is_err());
        assert!(
            CleanPolicy::Between { from: 100, to: 200 }.validate().is_ok(),
            "from<to 合法"
        );
        assert!(CleanPolicy::Between { from: 200, to: 200 }.validate().is_err());
        assert!(CleanPolicy::Between { from: 300, to: 200 }.validate().is_err());
        // 非法策略在 plan/run 入口就该被挡下
        assert!(plan(Path::new("."), &CleanPolicy::Before { cutoff: 0 }, &Excludes::default()).is_err());
    }

    fn mk_excludes(pats: &[&str]) -> Excludes {
        let v: Vec<String> = pats.iter().map(|s| s.to_string()).collect();
        Excludes::new(&v).unwrap()
    }

    #[test]
    fn excluded_subtree_survives_empty_clean() {
        let base = fixture("excl_empty");
        let t = axis();
        put(&base, "cache/a.bin", b"111", t.now);
        put(&base, "sessions/db.sqlite", b"2222", t.now);
        put(&base, "deep/sessions/x.bin", b"33", t.now);

        let ex = mk_excludes(&["sessions"]);
        let p = plan(&base, &CleanPolicy::Empty, &ex).unwrap();
        assert_eq!(p.files, 1, "只有 cache/a.bin 会被清");
        assert_eq!(p.excluded_skipped, 2, "两个 sessions 目录都被剪掉");

        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &ex, false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 1);
        assert!(base.join("sessions/db.sqlite").exists(), "排除子树必须原样保留");
        assert!(base.join("deep/sessions/x.bin").exists(), "任意层级的同名目录都受保护");
        assert!(!base.join("cache").exists(), "未排除的正常清理");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn anchored_exclude_prunes_only_outside() {
        let base = fixture("excl_anchor");
        let t = axis();
        put(&base, "logs/old/keep.bin", b"1", t.now);
        put(&base, "logs/fresh.bin", b"22", t.now);
        put(&base, "deep/logs/old/kill.bin", b"333", t.now);

        // 锚定：只保护根下第一层 logs/old，深层同名不保护
        let ex = mk_excludes(&["logs/old"]);
        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &ex, false, &mut sink).unwrap();
        assert!(base.join("logs/old/keep.bin").exists(), "锚定子树保留");
        assert!(!base.join("logs/fresh.bin").exists(), "同目录其他文件正常清");
        assert!(!base.join("deep/logs/old").exists(), "深层同名不在此模式保护范围");
        assert!(rep.excluded_skipped >= 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn excluded_empty_dir_is_not_pruned() {
        let base = fixture("excl_prune");
        let t = axis();
        put(&base, "keep/inner/dead.bin", b"1", t.two_days_ago);

        let ex = mk_excludes(&["keep"]);
        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &ex, false, &mut sink).unwrap();
        assert_eq!(rep.deleted_files, 0, "排除子树里的文件不删");
        assert_eq!(rep.dirs_pruned, 0, "排除子树的空目录也不剪");
        assert!(base.join("keep/inner").is_dir(), "目录结构原样保留");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn exclude_reported_in_summary() {
        let base = fixture("excl_summary");
        let t = axis();
        put(&base, "sessions/a.bin", b"11", t.now);
        let ex = mk_excludes(&["sessions"]);
        let mut sink = |_| {};
        let rep = run(&base, &CleanPolicy::Empty, &ex, false, &mut sink).unwrap();
        assert!(rep.summary().contains("按排除规则保留"), "{}", rep.summary());
        let _ = fs::remove_dir_all(&base);
    }
}
