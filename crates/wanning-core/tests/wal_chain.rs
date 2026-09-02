//! WAL 完整性链(先红后绿,W-21)。
//!
//! 洞:审计日志的「防篡改」此前只靠**语义对账**——回放重算判定与记录是否一致。
//! 语义对账抓不住这些改动:
//! ① 历史行里改一个**不参与判定**的字段(如 memo/merchant)——重算照旧一致;
//! ② **删掉中间一行**(拒绝行不影响后续状态)——重算照旧一致;
//! ③ **复制/重排拒绝行**(拒绝不耗 nonce 不动账本)——重算照旧一致。
//! 也就是说,读审计的人看到的证据可以被无声改写,而闸的回放给不出任何报错
//! ——demo 输出里「任何篡改都会被对账抓住」因此是过度声明。
//!
//! 修法:落盘行加完整性链——每行带 `seq`(物理行号)与 `prev`(上一行的链值),
//! 链值 = FNV-1a64(prev ‖ seq ‖ 该行内容的规范 JSON)。读回逐行验:
//! `seq` 必须等于物理行号(删行/重排/复制当场现形),`prev` 必须等于按前文重算的
//! 链值(改历史行而不重算后续整条链,下一行的 prev 就对不上)→ fail-closed。
//!
//! **已知边界**(记入模块文档与 master-plan,不在本文件假装能测):
//! 链抓不住「只改最后一行内容」「整体截尾」——最后一行没有后继行引用它。
//! 那需要外部锚点(老板侧签名/远端锚点),列为账户开通后的 TODO。

use std::sync::Arc;

use wanning_core::clock::MockClock;
use wanning_core::delegation::Delegation;
use wanning_core::state::WanningState;
use wanning_core::wal::{raw_lines, read_records, Wal};

fn tmp_path(tag: &str) -> std::path::PathBuf {
    // 名字带进程内原子序号:用例并行起跑,只靠「纳秒+pid」可能同 tick 撞名,
    // 两个用例抢同一把单写者锁,输的一方 WalLocked 起不来(W-21 顺带修)。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join("wanning-wal-chain-tests");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix 纪元")
        .as_nanos();
    dir.join(format!(
        "{tag}-{unix_nanos}-{}-{}.jsonl",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::process::id()
    ))
}

fn delegation() -> Delegation {
    Delegation::new(
        "d1",
        "老板",
        "claude-code",
        1_000,
        1_000,
        2_000,
        "agent:claude-code",
    )
}

/// 单会话(同一 state,不重开)建样本,行序确定:
/// `lines` = 每行的意图描述,依次执行(首行恒为注册)。
/// 返回 WAL 路径;调用方负责 drop 后再动文件(state 持锁,必须先释放)。
///
/// 注意必须单会话建满:`with_wal` 不回放旧账(W-17 决策),重开后 decide 打在
/// 空闸上会记成 UnknownDelegation 拒绝行,行序就乱了。
fn sample_wal_with(tag: &str, decides: &[(u64, u64, &str)]) -> std::path::PathBuf {
    let path = tmp_path(tag);
    let mut state = WanningState::with_wal(Arc::new(MockClock::new(1_500)), &path).expect("开 WAL");
    state.register_delegation(delegation()).expect("注册");
    for (nonce, amount, memo) in decides {
        state
            .decide(&wanning_core::intent::SpendIntent::new(
                "d1",
                *nonce,
                *amount,
                "jd:shop-1",
                "grocery",
                *memo,
            ))
            .expect("decide 落审计");
    }
    drop(state);
    path
}

/// 三行样本:注册 → 放行(¥1)→ 超额拒(¥90)。
fn sample_wal(tag: &str) -> std::path::PathBuf {
    sample_wal_with(tag, &[(1, 100, "原始备注"), (2, 9_000, "超额意图")])
}

/// 把第 `line_no`(1-based)行里的 intent.memo 改成 `memo` 后整文件重写。
///
/// 兼容两种落盘形态(包裹前 memo 在行根;包裹后在 `rec` 下),红绿两态都改得到
/// 同一个字段——改的是**不参与判定的字段**,语义对账(重算)本来抓不住它。
fn rewrite_with_memo(path: &std::path::Path, line_no: usize, memo: &str) {
    let mut lines = raw_lines(path).expect("读 WAL");
    let idx = line_no - 1;
    let mut value: serde_json::Value = serde_json::from_str(&lines[idx]).expect("行是 JSON");
    if value.get("rec").is_some() {
        value["rec"]["intent"]["memo"] = serde_json::Value::String(memo.to_string());
    } else {
        value["intent"]["memo"] = serde_json::Value::String(memo.to_string());
    }
    lines[idx] = value.to_string();
    std::fs::write(path, lines.join("\n") + "\n").expect("重写 WAL");
}

#[test]
fn in_place_edit_of_a_past_line_is_caught() {
    let path = sample_wal("memo-edit");
    rewrite_with_memo(&path, 2, "被改写的备注");

    let err = WanningState::replay(&path).expect_err("改历史行必须 fail-closed(回放)");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
    let err = read_records(&path).expect_err("改历史行必须 fail-closed(读回)");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
}

#[test]
fn removing_a_middle_line_is_caught() {
    // 四行:注册 → 放行 → 超额拒 → 再放行;删掉中间的拒绝行(第 3 行)。
    // 拒绝行不影响任何后续状态,语义对账(重算)抓不住删行。
    let path = sample_wal_with(
        "delete-line",
        &[
            (1, 100, "原始备注"),
            (2, 9_000, "超额意图"),
            (3, 100, "再放行"),
        ],
    );

    let mut lines = raw_lines(&path).expect("读 WAL");
    assert_eq!(lines.len(), 4, "样本应为四行: {lines:?}");
    lines.remove(2); // 删第 3 行(拒绝行)
    std::fs::write(&path, lines.join("\n") + "\n").expect("重写 WAL");

    let err = WanningState::replay(&path).expect_err("删中间行必须 fail-closed");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
}

#[test]
fn duplicating_a_deny_line_is_caught() {
    // 拒绝不耗 nonce、不动账本:复制一行拒绝,语义对账(重算)抓不住。
    let path = tmp_path("duplicate-line");
    let mut state = WanningState::with_wal(Arc::new(MockClock::new(1_500)), &path).expect("开 WAL");
    state.register_delegation(delegation()).expect("注册");
    state
        .decide(&wanning_core::intent::SpendIntent::new(
            "d1",
            1,
            9_000,
            "jd:shop-1",
            "grocery",
            "超额意图",
        ))
        .expect("超额拒");
    drop(state);

    let lines = raw_lines(&path).expect("读 WAL");
    assert_eq!(lines.len(), 2, "样本应为两行: {lines:?}");
    let mut lines = lines;
    lines.push(lines[1].clone()); // 复制拒绝行
    std::fs::write(&path, lines.join("\n") + "\n").expect("重写 WAL");

    let err = WanningState::replay(&path).expect_err("复制行必须 fail-closed");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
}

#[test]
fn swapping_two_deny_lines_is_caught() {
    // 重排两行拒绝(都不耗 nonce 不动账本):语义对账(重算)抓不住。
    let path = tmp_path("swap-lines");
    let mut state = WanningState::with_wal(Arc::new(MockClock::new(1_500)), &path).expect("开 WAL");
    state.register_delegation(delegation()).expect("注册");
    state
        .decide(&wanning_core::intent::SpendIntent::new(
            "d1",
            1,
            9_000,
            "jd:shop-1",
            "grocery",
            "超额意图一",
        ))
        .expect("超额拒一");
    state
        .decide(&wanning_core::intent::SpendIntent::new(
            "d1",
            2,
            9_000,
            "jd:shop-2",
            "grocery",
            "超额意图二",
        ))
        .expect("超额拒二");
    drop(state);

    let mut lines = raw_lines(&path).expect("读 WAL");
    assert_eq!(lines.len(), 3, "样本应为三行: {lines:?}");
    lines.swap(1, 2); // 对调两行拒绝
    std::fs::write(&path, lines.join("\n") + "\n").expect("重写 WAL");

    let err = WanningState::replay(&path).expect_err("重排行必须 fail-closed");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
}

#[test]
fn wal_open_fails_closed_on_tampered_history() {
    // 打开(追加模式)= 先验完整历史:带病审计绝不续写。
    let path = sample_wal("open-tampered");
    rewrite_with_memo(&path, 2, "被改写的备注");

    let err = Wal::open(&path).expect_err("历史被改必须拒开");
    assert!(
        format!("{err}").contains("完整性链"),
        "报错要点名完整性链: {err}"
    );
}

#[test]
fn old_format_line_is_rejected_with_clear_message() {
    // 完整性链是落盘格式的一部分(2026-09-02 W-21 引入):
    // 旧格式(无 seq/prev 包裹)读不懂,报错要说明白,而不是一句泛泛的解析失败。
    let path = tmp_path("old-format");
    std::fs::write(
        &path,
        "{\"kind\":\"revoke\",\"ts\":1500,\"delegation_id\":\"d1\"}\n",
    )
    .expect("写旧格式行");

    let err = read_records(&path).expect_err("旧格式必须 fail-closed");
    let message = format!("{err}");
    assert!(
        message.contains("完整性链") && message.contains("W-21"),
        "报错要点名完整性链并指向 W-21: {message}"
    );
}
