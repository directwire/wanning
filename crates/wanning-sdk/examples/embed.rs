//! Wanning 嵌入 SDK 演示:宿主程序五步接入(全离线,零网络零真实消费)。
//!
//! 运行:`cargo run -p wanning-sdk --example embed`
//!
//! 走的就是四卖点语义(预算内放行 → 累计 → 超额拒 → kill switch → 撤销后拒),
//! 但每一步都从 SDK 回执拿真实值(判定/nonce/审计行号),不硬编码。

use wanning_core::gate::GateDecision;
use wanning_sdk::{Delegation, SpendRequest, Wanning};

/// 2100-01-01 00:00:00 UTC(样例委托的失效时刻;演示用,不写死「现在+N」)。
const VALID_UNTIL_2100: u64 = 4_102_444_800;

fn main() {
    let wal =
        std::env::temp_dir().join(format!("wanning-sdk-example-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&wal); // 演示可重入

    println!("== Wanning 嵌入 SDK 演示(全离线;零网络零真实消费)==");
    println!("审计 WAL: {}", wal.display());
    println!();

    // ① 开闸:唯一入口,必带 WAL(开机即回放续放;这里是从零开张)。
    let mut gate = Wanning::open(&wal).expect("开闸");

    // ② 注册委托:用户 → agent 的一次授权(¥10 总预算,单位分)。
    gate.authorize(Delegation::new(
        "d1",
        "boss",
        "claude-code",
        1000,
        0,
        VALID_UNTIL_2100,
        "agent:embed-demo",
    ))
    .expect("注册");
    println!("[行 1] 注册委托 d1(上限 1000 分,nonce 作用域 agent:embed-demo)");

    // ③ 判定:委托 id 宿主给,nonce 闸注入,判定与拒绝都落审计。
    let v = gate
        .decide(
            "d1",
            SpendRequest::new(300, "jd:shop-1", "grocery", "第一笔"),
        )
        .expect("判定");
    println!(
        "[行 {}] nonce={} → {}(判后累计消费 {} 分)",
        v.wal_line,
        v.nonce,
        verdict_zh(&v.decision),
        spent_after_zh(&v.decision)
    );

    let v = gate
        .decide(
            "d1",
            SpendRequest::new(600, "jd:shop-1", "grocery", "第二笔"),
        )
        .expect("判定");
    println!(
        "[行 {}] nonce={} → {}(判后累计消费 {} 分)",
        v.wal_line,
        v.nonce,
        verdict_zh(&v.decision),
        spent_after_zh(&v.decision)
    );

    let v = gate
        .decide(
            "d1",
            SpendRequest::new(900, "jd:shop-1", "grocery", "超额意图"),
        )
        .expect("判定");
    println!(
        "[行 {}] nonce={} → {}(账本未动 nonce 不耗,剩余 {} 分)",
        v.wal_line,
        v.nonce,
        verdict_zh(&v.decision),
        gate.remaining_cents("d1").expect("委托在")
    );

    // ④ kill switch:授权者动作,撤销后永不允许。
    gate.revoke("d1").expect("撤销");
    println!(
        "[行 {}] 撤销 d1(kill switch)",
        gate.last_wal_line().expect("必有行")
    );
    let v = gate
        .decide(
            "d1",
            SpendRequest::new(100, "jd:shop-1", "grocery", "撤销后再来"),
        )
        .expect("判定");
    println!(
        "[行 {}] nonce={} → {}",
        v.wal_line,
        v.nonce,
        verdict_zh(&v.decision)
    );

    // ⑤ 审计自证:验链 + 回放对账,三条独立口径全对上才发回执。
    let report = gate.self_check().expect("自证通过");
    println!();
    println!(
        "自证:审计 {} 行,链尾 {:#x},state_hash {:#x}(实时 == 读侧重算 == 回放)",
        report.wal_line_count, report.chain_tail, report.state_hash
    );
    for line in gate.audit_tail(5).expect("读审计尾") {
        println!(
            "  [行 {}] {} ts={}",
            line.line_no,
            line.record.kind(),
            line.record.ts()
        );
    }

    drop(gate);
    let _ = std::fs::remove_file(&wal);
}

/// 判定的中文一句话(演示输出用)。
fn verdict_zh(decision: &GateDecision) -> String {
    match decision {
        GateDecision::Allow { .. } => "放行".to_string(),
        GateDecision::Deny { reason } => format!("拒绝({reason})"),
    }
}

/// 放行携带的判后累计消费(拒绝时显示「-」)。
fn spent_after_zh(decision: &GateDecision) -> String {
    match decision {
        GateDecision::Allow { budget_after_cents } => budget_after_cents.to_string(),
        GateDecision::Deny { .. } => "-".to_string(),
    }
}
