//! 控制台输出辅助：状态行、规则行、操作结果的中文化格式化。

use td_core::engine;
use td_core::store::Rule;

pub(crate) fn print_status_line(i: usize, st: &engine::RuleStatus) {
    println!(
        "{} #{:<4} [{}] {}",
        st.badge,
        st.rule_id,
        td_core::store::category_label(&st.category),
        st.name
    );
    println!("       源：{}", st.source);
    println!("       目标：{}  ({})", st.target, st.link_type);
    println!(
        "       状态：{}  开机重建：{}  期望开启：{}",
        st.label,
        if st.rebuild_on_boot { "是" } else { "否" },
        if st.want_enabled { "是" } else { "否" }
    );
    for w in &st.warnings {
        println!("       ⚠ {w}");
    }
    if i > 0 {
        println!();
    }
}

pub(crate) fn print_rule_row(rule: &Rule) {
    println!(
        "#{:<4} [{}] {}  →  {}",
        rule.id,
        td_core::store::category_label(&rule.category),
        rule.name,
        rule.target_subpath
    );
    println!(
        "       源：{}   链接：{}   开启：{}  开机重建：{}  需提权：{}",
        rule.source_template,
        rule.link_type,
        yn(rule.want_enabled),
        yn(rule.rebuild_on_boot),
        yn(rule.needs_elevation)
    );
    if let Some(n) = &rule.notes {
        println!("       备注：{n}");
    }
    if !rule.excludes.is_empty() {
        println!("       清理排除：{}", rule.excludes.join("、"));
    }
    if !rule.auto_clean_policy.trim().is_empty() {
        println!("       水位自动清理：{}", rule.auto_clean_policy);
    }
}

pub(crate) fn yn(v: bool) -> &'static str {
    if v {
        "是"
    } else {
        "否"
    }
}

pub(crate) fn print_outcome(o: &engine::Outcome) {
    let tag = if o.dry_run {
        "预演"
    } else {
        match o.action {
            "already_ok" | "already_plain" => "跳过",
            "moved_and_linked" | "merged_and_linked" | "created_and_linked" => "完成",
            "fixed_broken_target" => "修复",
            "restored" => "恢复",
            "relinked" => "重建",
            _ => "完成",
        }
    };
    println!(
        "  [{tag}] #{:<4} {:<20} {}",
        o.rule_id, o.name, o.message
    );
    if let Some(s) = o.stats {
        if s.files > 0 || s.bytes > 0 {
            println!(
                "         规模：{}，耗时 {}",
                s.summary(),
                td_core::util::human_dur_ms(o.elapsed_ms)
            );
        }
    }
}