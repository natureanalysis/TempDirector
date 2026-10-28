//! engine 的集成级测试：真实文件系统 + 内存库，覆盖 apply / restore / relink 的
//! 状态机路径与回滚语义。

use super::*;
use crate::error::TdError;
use crate::fsops::mover::Progress;
use crate::fsops::verify::VerifyMode;
use crate::link::detect::{LinkState, LinkType, TargetState};
use crate::store::{NewRule, Store};
use std::fs;
use std::path::PathBuf;

struct Env {
    base: PathBuf,
    store: Store,
    rule_id: i64,
    source: PathBuf,
    target: PathBuf,
}

fn noop(_: Progress) {}

/// 构造一个「RAM 盘就是本地某个目录」的测试环境。
fn setup(tag: &str) -> Env {
    let base = std::env::temp_dir().join(format!("td_engine_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let source = base.join("cache");
    let ram = base.join("ram");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&ram).unwrap();
    fs::write(source.join("a.txt"), b"aaa").unwrap();
    fs::create_dir_all(source.join("sub")).unwrap();
    fs::write(source.join("sub").join("b.bin"), vec![9u8; 2048]).unwrap();

    let store = Store::open_in_memory().unwrap();
    let root_id = store
        .insert_root("RAM", &ram.to_string_lossy(), true, 90000)
        .unwrap();
    let rule_id = store
        .insert_rule(&NewRule {
            name: "测试缓存".into(),
            category: "dev".into(),
            source_template: source.to_string_lossy().into_owned(),
            target_root_id: root_id,
            target_subpath: "dev\\test-cache".into(),
            link_type: "junction".into(),
            want_enabled: true,
            rebuild_on_boot: true,
            notes: None,
            excludes: Vec::new(),
            auto_clean_policy: String::new(),
            auto_clean_limit_bytes: 0,
        })
        .unwrap();
    let target = ram.join("dev").join("test-cache");

    Env {
        base,
        store,
        rule_id,
        source,
        target,
    }
}

#[test]
fn apply_then_status_then_restore() {
    let env = setup("roundtrip");
    let rule = env.store.get_rule(env.rule_id).unwrap();

    let st = rule_status(&env.store, &rule).unwrap();
    assert!(matches!(st.state, LinkState::PlainDir), "{:?}", st.state);

    // dry-run 不做任何改动
    let dry = apply_rule(
        &env.store,
        &rule,
        &ApplyOptions {
            dry_run: true,
            ..Default::default()
        },
        &mut noop,
    )
    .unwrap();
    assert!(dry.dry_run);
    assert!(env.source.join("a.txt").exists(), "dry-run 不应动文件");

    // 正式应用
    let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(out.action, "moved_and_linked");
    assert_eq!(out.stats.unwrap().files, 2);

    let st = rule_status(&env.store, &rule).unwrap();
    assert_eq!(st.badge, "✅");
    assert!(matches!(
        st.state,
        LinkState::LinkOk {
            target_state: TargetState::HasContent,
            ..
        }
    ));
    assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
    assert_eq!(
        fs::read(env.target.join("sub").join("b.bin")).unwrap().len(),
        2048
    );

    // 幂等
    let again = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(again.action, "already_ok");

    // 恢复
    let back = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(back.action, "restored");
    let st = rule_status(&env.store, &rule).unwrap();
    assert!(matches!(st.state, LinkState::PlainDir), "{:?}", st.state);
    assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
    assert!(!env.target.exists(), "恢复后目标内容应被搬走");

    // 再恢复一次：幂等
    let again = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(again.action, "already_plain");

    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn apply_recovers_after_ram_loss() {
    let env = setup("ramloss");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();

    // 模拟 RAM 盘重启：目标目录整个消失
    fs::remove_dir_all(&env.target).unwrap();
    let st = rule_status(&env.store, &rule).unwrap();
    assert!(matches!(st.state, LinkState::LinkBroken { .. }), "{:?}", st.state);
    assert_eq!(st.badge, "⚠️");

    let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(out.action, "fixed_broken_target");
    let st = rule_status(&env.store, &rule).unwrap();
    assert!(matches!(
        st.state,
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            ..
        }
    ));

    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn merge_into_non_empty_target() {
    let env = setup("merge");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    fs::create_dir_all(&env.target).unwrap();
    fs::write(env.target.join("old.txt"), b"old").unwrap();

    // 默认拒绝
    let err = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap_err();
    assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");

    // 允许合并
    let out = apply_rule(
        &env.store,
        &rule,
        &ApplyOptions {
            allow_merge: true,
            ..Default::default()
        },
        &mut noop,
    )
    .unwrap();
    assert_eq!(out.action, "merged_and_linked");
    assert!(env.target.join("old.txt").exists(), "原目标文件应保留");
    assert_eq!(fs::read(env.source.join("a.txt")).unwrap(), b"aaa");
    assert!(matches!(
        rule_status(&env.store, &rule).unwrap().state,
        LinkState::LinkOk { .. }
    ));

    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn restore_empty_target_creates_plain_dir() {
    let env = setup("restoreempty");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    fs::remove_file(env.target.join("a.txt")).unwrap();
    fs::remove_file(env.target.join("sub").join("b.bin")).unwrap();
    fs::remove_dir(env.target.join("sub")).unwrap();

    let out = restore_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(out.action, "restored");
    assert!(env.source.is_dir());
    assert!(matches!(
        rule_status(&env.store, &rule).unwrap().state,
        LinkState::PlainDir
    ));
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn source_missing_creates_empty_link() {
    let env = setup("missing");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    fs::remove_dir_all(&env.source).unwrap();

    let out = apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).unwrap();
    assert_eq!(out.action, "created_and_linked");
    assert!(matches!(
        rule_status(&env.store, &rule).unwrap().state,
        LinkState::LinkOk {
            target_state: TargetState::Empty,
            ..
        }
    ));
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn relink_refuses_contentful_plain_dir() {
    let env = setup("relink");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    // 目标就绪，但源是含内容的真实目录 → 必须拒绝，绝不能把内容盖掉
    fs::create_dir_all(&env.target).unwrap();
    let err = relink_rule(&env.store, &rule).unwrap_err();
    assert!(matches!(err, TdError::SafetyBlocked(_)), "{err}");
    assert!(env.source.join("a.txt").exists());
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn relink_requires_existing_target() {
    let env = setup("relinknotarget");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    // 源是空壳目录 + 目标不存在 → 报"目标不存在"
    fs::remove_file(env.source.join("a.txt")).unwrap();
    fs::remove_file(env.source.join("sub").join("b.bin")).unwrap();
    fs::remove_dir(env.source.join("sub")).unwrap();
    let err = relink_rule(&env.store, &rule).unwrap_err();
    assert!(matches!(err, TdError::InvalidPath(_)), "{err}");
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn relink_works_on_empty_source_shell() {
    let env = setup("relinkok");
    let rule = env.store.get_rule(env.rule_id).unwrap();
    // 源被软件更新清空成空目录，目标已就绪 → 应该能直接重链
    fs::create_dir_all(&env.target).unwrap();
    fs::remove_file(env.source.join("a.txt")).unwrap();
    fs::remove_file(env.source.join("sub").join("b.bin")).unwrap();
    fs::remove_dir(env.source.join("sub")).unwrap();

    let out = relink_rule(&env.store, &rule).unwrap();
    assert_eq!(out.action, "relinked");
    assert!(matches!(
        rule_status(&env.store, &rule).unwrap().state,
        LinkState::LinkOk { .. }
    ));
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn rejected_apply_does_not_touch_anything() {
    let env = setup("reject");
    // 把目标根指向一个不存在的盘
    env.store
        .update_root(1, "RAM", "Z:\\NopeNotExist", true, 90000)
        .unwrap();
    let rule = env.store.get_rule(env.rule_id).unwrap();
    assert!(apply_rule(&env.store, &rule, &ApplyOptions::default(), &mut noop).is_err());
    assert!(env.source.join("a.txt").exists());
    let _ = fs::remove_dir_all(&env.base);
}

#[test]
fn state_keys_are_stable() {
    assert_eq!(state_key(&LinkState::PlainDir), "plain_dir");
    assert_eq!(state_key(&LinkState::SourceMissing), "source_missing");
    assert_eq!(
        state_key(&LinkState::LinkOk {
            target: PathBuf::from("R:\\x"),
            link_type: LinkType::Junction,
            target_state: TargetState::Empty,
            target_mismatch: false,
        }),
        "link_ok_empty"
    );
}

#[test]
fn clean_dir_follows_link_state() {
    let env = setup("clean_dir");
    let rule = env.store.get_rule(env.rule_id).unwrap();

    // 未应用：源是普通目录 → 清理命中源
    assert_eq!(clean_dir_for_rule(&env.store, &rule).unwrap(), env.source);

    // 应用后：源变成指向 RAM 盘目标的联接 → 清理应命中真实内容所在的目标
    let opts = ApplyOptions {
        dry_run: false,
        verify: VerifyMode::CountAndSize,
        skip_lock_check: true,
        allow_merge: false,
    };
    apply_rule(&env.store, &rule, &opts, &mut noop).unwrap();
    assert_eq!(clean_dir_for_rule(&env.store, &rule).unwrap(), env.target);

    let _ = fs::remove_dir_all(&env.base);
}