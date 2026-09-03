//! 静态审计回放页(W-22):WAL → **自包含 HTML 时间线**,谈合作用的 protocol receipt。
//!
//! 定位:把「全程审计」卖点从终端文本升级为人能打开、能看懂、能核对的证据页——
//! 授权到收权的每一行:谁授权、哪个意图、闸怎么判、为什么、判完账本多少,以及
//! 每行的完整性链(`prev → 本行链值`,W-21)逐行可见,而不是黑盒一句「已验」。
//!
//! 硬约束:
//! - **零后端零 JS 零外链**:单文件 HTML,纯 CSS,`file://` 离线可开,不引用任何
//!   远程资源(审计页引用远程资源 = 给证据链开外呼通道)。
//! - **fail-closed 先于产出**:先验完整性链([`wanning_core::wal::read_verified`]),
//!   再回放对账两遍(确定性),任何一步不过 → 报错,**输出文件一个字节都不动**
//!   ([`export_audit`] 先写临时文件再原子改名)。
//! - **数据全部 HTML 转义**:memo/merchant/owner 是自由文本,一个 `<script>` 都
//!   不能直出([`escape_html`] + 测试实证)。
//! - **诚实呈现**:已知边界落在页面上——链抓不住「只改最后一行内容」与「整体截尾」,
//!   需外部锚点(所有者侧锚点已落地:`--anchor-sign`,W-23);本页是只读视图,
//!   证据以 WAL 原文为准。
//!
//! 渲染是纯函数(同一份账 + 同一时刻戳 → 字节级同一页面),时间戳由调用方注入,
//! 测试传 `None` 即得确定性输出。配色遵循内置数据可视化规范(状态色只表状态,
//! 徽章一律图标 + 文字,数值列 `tabular-nums`,深浅两套各配各的步)。

use std::path::Path;

use wanning_core::error::CoreError;
use wanning_core::state::WanningState;
use wanning_core::wal::{read_verified, WalChainLink, WalDecision, WalRecord};

use crate::scenario::deny_reason_zh;

// ---------------------------------------------------------------------------
// 报告模型:渲染的输入(可独立断言的结构化事实,HTML 只是它的一个投影)
// ---------------------------------------------------------------------------

/// 一行审计事件(行号 + 完整性链节 + 记录本体)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRow {
    /// 物理行号(1-based),即证据的 WAL 偏移。
    pub line_no: u64,
    /// 读侧独立重算的完整性链节。
    pub link: WalChainLink,
    /// 记录本体。
    pub record: WalRecord,
}

/// 事件计数(报告头部的统计块)。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventCounts {
    pub register: u64,
    pub revoke: u64,
    pub allow: u64,
    pub deny: u64,
}

/// 一份委托的预算台账(来自回放态,不是来自各行的拼接)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelegationSummary {
    pub id: String,
    pub owner: String,
    pub agent: String,
    pub cap_cents: u64,
    pub spent_cents: u64,
    pub remaining_cents: u64,
    pub revoked: bool,
    pub valid_from: u64,
    pub valid_until: u64,
    pub nonce_scope: String,
}

/// 审计报告:回放页的全部事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditReport {
    /// 审计原文路径(展示用)。
    pub wal_display: String,
    /// 逐行事件(顺序 = WAL 物理行序 = 时间序)。
    pub rows: Vec<AuditRow>,
    /// 完整性链尾(读侧独立重算)。
    pub chain_tail: u64,
    /// 回放态状态 hash(两遍一致才到得了这里)。
    pub replay_state_hash: u64,
    /// 各委托预算台账(按委托 id 排序,渲染确定)。
    pub delegations: Vec<DelegationSummary>,
    /// 事件计数。
    pub counts: EventCounts,
    /// 累计放行金额(分):所有放行意图的金额合计。
    pub allow_amount_cents: u64,
    /// 页面生成时刻(Unix 秒);测试传 `None` 以保持渲染确定。
    pub generated_at_unix: Option<u64>,
}

/// 从一份 WAL 构建审计报告。**fail-closed**:链断裂 / 坏行 / 缺文件 / 回放对账
/// 不一致,一律报错,绝不产出半页证据。
pub fn build_report(wal_path: impl AsRef<Path>) -> Result<AuditReport, CoreError> {
    let path = wal_path.as_ref();
    // 第一道:逐行验完整性链(seq = 物理行号,prev = 前行链值)。
    let verified = read_verified(path)?;
    // 第二道:回放重算两遍并与记录对账(语义层),两遍 hash 必一致(确定性)。
    let replay_once = WanningState::replay(path)?;
    let replay_twice = WanningState::replay(path)?;
    let state_hash = replay_once.state_hash();
    if state_hash != replay_twice.state_hash() {
        return Err(CoreError::WalMismatch {
            line: 0,
            message: "同一份账回放两遍状态 hash 不一致(确定性被破坏),拒绝产出回放页".to_string(),
        });
    }

    let mut counts = EventCounts::default();
    let mut allow_amount_cents = 0u64;
    for (_, record) in &verified.records {
        match record {
            WalRecord::RegisterDelegation { .. } => counts.register += 1,
            WalRecord::Revoke { .. } => counts.revoke += 1,
            WalRecord::Decide {
                decision, intent, ..
            } => match decision {
                WalDecision::Allow => {
                    counts.allow += 1;
                    allow_amount_cents = allow_amount_cents
                        .checked_add(intent.amount_cents)
                        .ok_or_else(|| {
                            CoreError::LedgerOverflow("累计放行金额合计溢出(u64)".to_string())
                        })?;
                }
                WalDecision::Deny => counts.deny += 1,
            },
        }
    }

    let gate = replay_once.gate();
    let delegations = gate
        .delegations()
        .map(|d| {
            let spent = gate.spent_cents(&d.id).unwrap_or(0);
            DelegationSummary {
                id: d.id.clone(),
                owner: d.owner.clone(),
                agent: d.agent.clone(),
                cap_cents: d.budget_cap_cents,
                spent_cents: spent,
                remaining_cents: d.budget_cap_cents.saturating_sub(spent),
                revoked: gate.is_revoked(&d.id),
                valid_from: d.valid_from,
                valid_until: d.valid_until,
                nonce_scope: d.nonce_scope.clone(),
            }
        })
        .collect();

    Ok(AuditReport {
        wal_display: path.display().to_string(),
        rows: verified
            .records
            .into_iter()
            .zip(verified.links)
            .map(|((line_no, record), link)| AuditRow {
                line_no,
                link,
                record,
            })
            .collect(),
        chain_tail: verified.tail,
        replay_state_hash: state_hash,
        delegations,
        counts,
        allow_amount_cents,
        generated_at_unix: None,
    })
}

/// 导出回放页:先对账([`build_report`]),后渲染,最后**先写临时文件再原子改名**——
/// 任何一步失败,已有输出文件一个字节都不动,也不留半个临时文件。
pub fn export_audit(
    wal_path: impl AsRef<Path>,
    out_path: impl AsRef<Path>,
    generated_at_unix: Option<u64>,
) -> Result<AuditReport, CoreError> {
    let out_path = out_path.as_ref();
    let mut report = build_report(wal_path)?;
    report.generated_at_unix = generated_at_unix;
    let html = render_html(&report);

    // 临时文件与目标同目录(保证改名在同盘原子),后缀 .tmp。
    let tmp_path = {
        let mut name = out_path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".tmp");
        out_path.with_file_name(name)
    };
    std::fs::write(&tmp_path, html)
        .map_err(|e| CoreError::WalIo(format!("写回放页 {tmp_path:?} 失败: {e}")))?;
    if let Err(e) = std::fs::rename(&tmp_path, out_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CoreError::WalIo(format!(
            "回放页改名 {tmp_path:?} → {out_path:?} 失败(已有输出未被改动): {e}"
        )));
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// 渲染(纯函数)
// ---------------------------------------------------------------------------

/// 渲染自包含 HTML。同一份报告 + 同一 `generated_at_unix` → 同一输出。
pub fn render_html(report: &AuditReport) -> String {
    let mut html = String::with_capacity(16 * 1024 + report.rows.len() * 512);
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>Wanning 审计回放</title>\n");
    html.push_str(STYLE);
    html.push_str("</head>\n<body>\n<main class=\"wrap\">\n");

    html.push_str("<header>\n<h1>Wanning 审计回放</h1>\n");
    html.push_str("<p class=\"sub\">意图层支付闸 · 全程审计时间线 · 只读视图,证据以审计原文为准</p>\n</header>\n");

    render_kpis(report, &mut html);
    render_reconciliation(report, &mut html);
    render_budgets(report, &mut html);
    render_timeline(report, &mut html);
    render_footer(report, &mut html);

    html.push_str("</main>\n</body>\n</html>\n");
    html
}

/// 统计块(四个数,proportional figures;标签用次级墨色)。
fn render_kpis(report: &AuditReport, html: &mut String) {
    html.push_str("<section class=\"kpis\" aria-label=\"统计\">\n");
    let tiles = [
        (report.rows.len().to_string(), "审计行数"),
        (report.counts.allow.to_string(), "放行笔数"),
        (report.counts.deny.to_string(), "拒绝笔数"),
        (format_cents(report.allow_amount_cents), "累计放行金额"),
    ];
    for (value, label) in tiles {
        html.push_str("<div class=\"tile\"><div class=\"v\">");
        html.push_str(&escape_html(&value));
        html.push_str("</div><div class=\"l\">");
        html.push_str(label);
        html.push_str("</div></div>\n");
    }
    html.push_str("</section>\n");
}

/// 对账节:逐行链 + 回放确定性,以及诚实声明本页是什么、不是什么。
fn render_reconciliation(report: &AuditReport, html: &mut String) {
    html.push_str("<section class=\"card\"><h2>对账</h2>\n<ul>\n");
    html.push_str(&format!(
        "<li><strong>完整性链</strong>:{} 行全部通过逐行验证(每行 seq=物理行号、\
         prev=前行链值,读侧独立重算);链尾 <code class=\"hash\">0x{}</code></li>\n",
        report.rows.len(),
        chain_hex(report.chain_tail),
    ));
    html.push_str(&format!(
        "<li><strong>回放对账</strong>:同一份账回放两遍,状态 hash 一致 \
         (<code class=\"hash\">0x{}</code>);预算台账 {} 份委托</li>\n",
        chain_hex(report.replay_state_hash),
        report.delegations.len(),
    ));
    html.push_str(
        "<li><strong>证据声明</strong>:本页是只读视图;每行证据的原文是该 WAL 的\
         对应物理行,核对以原文为准。</li>\n",
    );
    html.push_str(
        "<li><strong>已知边界(诚实声明)</strong>:完整性链抓得住「改历史行 / 删行 / \
         重排 / 复制」;<em>只改最后一行内容</em>与<em>整体截尾</em>本地验不住\
         (无后继行引用),需外部锚点兜底——授权人侧锚点已落地(2026-09-02 W-23):\
         <code>wanning-demo --anchor-sign &lt;wal&gt; --key &lt;key&gt; --out &lt;anchor.json&gt;</code>,\
         锚定后这两类篡改当场现形。</li>\n",
    );
    html.push_str("</ul>\n</section>\n");
}

/// 预算台账:每份委托一行,meter 表达「已用 / 上限」单一比值。
fn render_budgets(report: &AuditReport, html: &mut String) {
    html.push_str("<section class=\"card\"><h2>预算台账</h2>\n");
    if report.delegations.is_empty() {
        html.push_str("<p class=\"empty\">暂无授权记录。</p>\n</section>\n");
        return;
    }
    html.push_str(
        "<div class=\"scroll\"><table>\n<thead><tr><th>委托</th><th>授权</th>\
<th class=\"num\">上限</th><th class=\"num\">已用</th><th class=\"num\">剩余</th>\
<th>有效期(UTC)</th><th>状态</th><th>预算用量</th></tr></thead>\n<tbody>\n",
    );
    for d in &report.delegations {
        // 单一比值 meter:填充比例 = 已用 / 上限(cap≥1 由注册校验保证;
        // saturating 防巨额定值乘 100 溢出,结果再夹到 100)。
        let pct = (d.spent_cents.saturating_mul(100) / d.cap_cents.max(1)).min(100);
        html.push_str("<tr>");
        html.push_str(&format!(
            "<td><code>{}</code></td><td>{} → {}</td>",
            escape_html(&d.id),
            escape_html(&d.owner),
            escape_html(&d.agent),
        ));
        for cents in [d.cap_cents, d.spent_cents, d.remaining_cents] {
            html.push_str(&format!("<td class=\"num\">{}</td>", format_cents(cents)));
        }
        html.push_str(&format!(
            "<td class=\"num\">{} → {}</td><td>{}</td>",
            format_utc(d.valid_from),
            format_utc(d.valid_until),
            if d.revoked { "已撤销" } else { "生效中" },
        ));
        html.push_str(&format!(
            "<td><div class=\"meter\" role=\"img\" aria-label=\"已用 {} / 上限 {}\">\
             <i style=\"width:{}%\"></i></div></td>",
            format_cents(d.spent_cents),
            format_cents(d.cap_cents),
            pct,
        ));
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody>\n</table></div>\n</section>\n");
}

/// 事件时间线:一行一个事件,委托列 + 明细列 + 结果列 + 完整性链列。
fn render_timeline(report: &AuditReport, html: &mut String) {
    html.push_str("<section class=\"card\"><h2>事件时间线</h2>\n");
    if report.rows.is_empty() {
        html.push_str(
            "<p class=\"empty\">本日志当前为空(0 行):还没有任何授权或判定落账。</p>\n\
             </section>\n",
        );
        return;
    }
    html.push_str(
        "<div class=\"scroll\"><table>\n<thead><tr><th class=\"num\">#</th>\
<th class=\"num\">时间(UTC)</th><th>事件</th><th>委托</th><th>明细</th><th>结果</th>\
<th>完整性链</th></tr></thead>\n<tbody>\n",
    );
    for row in &report.rows {
        html.push_str("<tr>");
        html.push_str(&format!(
            "<td class=\"num\">{}</td><td class=\"num\">{}</td>",
            row.line_no,
            format_utc(row.record.ts()),
        ));

        // 事件徽章 + 委托列 + 明细列 + 结果列(按记录种类)。
        let (event_html, delegation_html, detail_html, result_html) = match &row.record {
            WalRecord::RegisterDelegation { delegation, .. } => (
                badge("●", "注册", BadgeKind::Neutral),
                format!("<code>{}</code>", escape_html(&delegation.id)),
                format!(
                    "授权 {} → {}<br><span class=\"dim\">上限 {} · 有效期 {} → {} · \
                     nonce 作用域 {}</span>",
                    escape_html(&delegation.owner),
                    escape_html(&delegation.agent),
                    format_cents(delegation.budget_cap_cents),
                    format_utc(delegation.valid_from),
                    format_utc(delegation.valid_until),
                    escape_html(&delegation.nonce_scope),
                ),
                String::from("—"),
            ),
            WalRecord::Revoke { delegation_id, .. } => (
                badge("⊘", "撤销", BadgeKind::Neutral),
                format!("<code>{}</code>", escape_html(delegation_id)),
                "收权(kill switch):此后该委托的一切意图都被拒绝".to_string(),
                String::from("—"),
            ),
            WalRecord::Decide {
                decision,
                intent,
                reason,
                ..
            } => {
                let (event, verdict_html) = match decision {
                    WalDecision::Allow => (badge("✓", "放行", BadgeKind::Good), "—".to_string()),
                    WalDecision::Deny => {
                        let (reason_zh, reason_raw) = match reason {
                            Some(reason) => (
                                deny_reason_zh(reason).to_string(),
                                serde_reason_label(*reason),
                            ),
                            None => ("未知原因".to_string(), "unknown".to_string()),
                        };
                        (
                            badge("✕", "拒绝", BadgeKind::Critical),
                            format!(
                                "{}<br><span class=\"dim\">reason={}</span>",
                                escape_html(&reason_zh),
                                escape_html(&reason_raw),
                            ),
                        )
                    }
                };
                let memo = if intent.memo.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", escape_html(&intent.memo))
                };
                (
                    event,
                    format!("<code>{}</code>", escape_html(&intent.delegation_id)),
                    format!(
                        "{} · {} · {}<br><span class=\"dim\">nonce {}{}</span>",
                        format_cents(intent.amount_cents),
                        escape_html(&intent.merchant_id),
                        escape_html(category_or_dash(&intent.category)),
                        intent.nonce,
                        memo,
                    ),
                    verdict_html,
                )
            }
        };
        // 放行/拒绝行的账本累计(拒绝不改账本,如实展示决策落地后的累计消费)。
        let ledger_note = match &row.record {
            WalRecord::Decide {
                budget_after_cents, ..
            } => format!(
                "<br><span class=\"dim\">账本累计 {}</span>",
                format_cents(*budget_after_cents)
            ),
            _ => String::new(),
        };
        html.push_str(&format!(
            "<td>{}</td><td>{}</td><td>{}</td><td>{}{}</td>",
            event_html, delegation_html, detail_html, result_html, ledger_note,
        ));

        // 完整性链列:prev → 本行链值(十六进制;title 给十进制原值)。
        html.push_str(&format!(
            "<td class=\"num\"><span class=\"hash\" title=\"prev={}\">{}</span> → \
             <span class=\"hash\" title=\"value={}\">{}</span></td>",
            row.link.prev,
            chain_hex(row.link.prev),
            row.link.value,
            chain_hex(row.link.value),
        ));
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody>\n</table></div>\n</section>\n");
}

/// 页脚:出处 + 生成方式 + 生成时刻。
fn render_footer(report: &AuditReport, html: &mut String) {
    html.push_str("<footer>\n<ul>\n");
    html.push_str(&format!(
        "<li>审计原文:{}</li>\n",
        escape_html(&report.wal_display)
    ));
    html.push_str(
        "<li>生成方式:wanning-demo --export-audit &lt;审计文件&gt; --out &lt;本文件&gt;\
         (离线只读,零网络调用)</li>\n",
    );
    if let Some(ts) = report.generated_at_unix {
        html.push_str(&format!("<li>生成时刻(UTC):{}</li>\n", format_utc(ts)));
    }
    html.push_str("</ul>\n</footer>\n");
}

/// 徽章种类:放行/拒绝是真正的状态(good/critical);注册/撤销是中性事件,
/// **不占用状态色**(状态色只表状态,不表身份)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BadgeKind {
    Good,
    Critical,
    Neutral,
}

/// 状态徽章:图标 + 文字(色彩永不单独承载语义);色彩只上图标与淡底。
fn badge(icon: &str, label: &str, kind: BadgeKind) -> String {
    let class = match kind {
        BadgeKind::Good => "badge good",
        BadgeKind::Critical => "badge critical",
        BadgeKind::Neutral => "badge neutral",
    };
    format!(
        "<span class=\"{class}\"><span class=\"icon\" aria-hidden=\"true\">{icon}</span>{label}</span>",
    )
}

/// reason 的机器可读标签(小写蛇形,与 WAL 原文一致,便于 diff)。
fn serde_reason_label(reason: wanning_core::gate::DenyReason) -> String {
    let json = serde_json::to_string(&reason).unwrap_or_default();
    json.trim_matches('"').to_string()
}

fn category_or_dash(category: &str) -> &str {
    if category.is_empty() {
        "-"
    } else {
        category
    }
}

/// HTML 转义:自由文本(memo/merchant/owner/路径)一律先过这里再进页面。
///
/// `pub`:W-43b 的 `wanning ui` 仪表盘复用同一个转义口径——自由文本进任何
/// Wanning 页面都只有一种转义实现,不允许第二份各写各的。
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// 金额:分 → 「¥整数.两位小数」。纯整数运算,**禁浮点**(这是钱)。
/// `pub`:供 `wanning ui` 仪表盘复用,金额呈现全仓一个口径。
pub fn format_cents(cents: u64) -> String {
    format!("¥{}.{:02}", cents / 100, cents % 100)
}

/// Unix 秒 → 「YYYY-MM-DD HH:MM:SS」(UTC)。零依赖:民用日由 days-from-civil
/// 的逆变换(Hinnant 算法)整数推导,含闰年。`pub`:供 `wanning ui` 仪表盘复用,
/// 时间呈现全仓一个口径。
pub fn format_utc(ts: u64) -> String {
    let days = ts / 86_400;
    let secs_of_day = ts % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// 天数 → (年, 月, 日)(公历;Howard Hinnant 的 civil_from_days)。
fn civil_from_days(days_since_epoch: i64) -> (i64, u64, u64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u64, day as u64)
}

/// u64 → 16 位小写十六进制(链值/哈希的统一呈现)。`pub`:供 `wanning ui`
/// 仪表盘复用,链值呈现全仓一个口径。
pub fn chain_hex(value: u64) -> String {
    format!("{value:016x}")
}

// ---------------------------------------------------------------------------
// 样式:单文件内联,零外链;配色为内置数据可视化规范的浅/深两套各自选步。
// ---------------------------------------------------------------------------

const STYLE: &str = r#"<style>
:root {
  color-scheme: light;
  --surface: #fcfcfb;
  --page: #f9f9f7;
  --ink: #0b0b0b;
  --ink-2: #52514e;
  --muted: #898781;
  --hairline: #e1e0d9;
  --border: rgba(11, 11, 11, 0.10);
  --hover: rgba(11, 11, 11, 0.03);
  --good: #0ca30c;
  --critical: #d03b3b;
  --meter-track: #cde2fb;
  --meter-fill: #2a78d6;
  --mono: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    color-scheme: dark;
    --surface: #1a1a19;
    --page: #0d0d0d;
    --ink: #ffffff;
    --ink-2: #c3c2b7;
    --muted: #898781;
    --hairline: #2c2c2a;
    --border: rgba(255, 255, 255, 0.10);
    --hover: rgba(255, 255, 255, 0.04);
    --good: #0ca30c;
    --critical: #d03b3b;
    --meter-track: #104281;
    --meter-fill: #3987e5;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--page);
  color: var(--ink);
  font: 14px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif;
}
.wrap { max-width: 1180px; margin: 0 auto; padding: 32px 24px 48px; }
h1 { font-size: 22px; margin: 0 0 4px; }
h2 { font-size: 15px; margin: 0 0 12px; }
.sub { color: var(--ink-2); margin: 0 0 24px; font-size: 13px; }
.kpis { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; margin-bottom: 20px; }
.tile { background: var(--surface); border: 1px solid var(--border); border-radius: 10px; padding: 14px 16px; }
.tile .v { font-size: 26px; font-weight: 650; line-height: 1.2; }
.tile .l { color: var(--ink-2); font-size: 12px; margin-top: 2px; }
.card { background: var(--surface); border: 1px solid var(--border); border-radius: 10px; padding: 18px 20px; margin-bottom: 20px; }
.card ul { margin: 0; padding-left: 20px; }
.card li { margin: 4px 0; color: var(--ink-2); }
.card li strong { color: var(--ink); font-weight: 600; }
.scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th { text-align: left; font-weight: 600; color: var(--ink-2); border-bottom: 1px solid var(--hairline); padding: 8px 10px; white-space: nowrap; }
td { border-bottom: 1px solid var(--hairline); padding: 8px 10px; vertical-align: top; }
tbody tr:hover td { background: var(--hover); }
th.num, td.num { font-variant-numeric: tabular-nums; }
code { font-family: var(--mono); font-size: 12px; }
.hash { font-family: var(--mono); font-size: 11.5px; color: var(--ink-2); }
.dim { color: var(--muted); font-size: 12px; }
.badge { display: inline-flex; align-items: center; gap: 6px; font-weight: 600; padding: 2px 8px; border-radius: 999px; white-space: nowrap; }
.badge .icon { font-size: 12px; }
.badge.good { color: var(--ink); background: rgba(12, 163, 12, 0.10); }
.badge.good .icon { color: var(--good); }
.badge.critical { color: var(--ink); background: rgba(208, 59, 59, 0.10); }
.badge.critical .icon { color: var(--critical); }
.badge.neutral { color: var(--ink-2); background: var(--hover); }
.meter { height: 6px; min-width: 120px; border-radius: 3px; background: var(--meter-track); }
.meter > i { display: block; height: 100%; border-radius: 3px; background: var(--meter-fill); }
.empty { color: var(--ink-2); margin: 4px 0; }
footer { color: var(--muted); font-size: 12px; border-top: 1px solid var(--hairline); padding-top: 14px; margin-top: 8px; }
footer ul { margin: 0; padding-left: 18px; }
footer li { margin: 2px 0; }
@media (max-width: 720px) { .kpis { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_format_is_integer_only() {
        assert_eq!(format_cents(0), "¥0.00");
        assert_eq!(format_cents(5), "¥0.05");
        assert_eq!(format_cents(500), "¥5.00");
        assert_eq!(format_cents(1_234_567), "¥12345.67");
    }

    #[test]
    fn utc_format_handles_epoch_leap_years_and_day_boundaries() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20");
        // 两个闰日(世纪闰年 2000 与普通闰年 2024)。
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00");
        // 恰在换日边界(23:59:59 → 次日 00:00:00)。
        assert_eq!(format_utc(86_399), "1970-01-01 23:59:59");
        assert_eq!(format_utc(86_400), "1970-01-02 00:00:00");
    }

    #[test]
    fn escape_html_neutralizes_markup_and_keeps_text_readable() {
        assert_eq!(
            escape_html(r#"<script>alert("x") & '</script>"#),
            "&lt;script&gt;alert(&quot;x&quot;) &amp; &#39;&lt;/script&gt;"
        );
        assert_eq!(escape_html("普通的中文备注"), "普通的中文备注");
        assert_eq!(escape_html(""), "");
    }

    #[test]
    fn chain_hex_is_fixed_width_lowercase() {
        assert_eq!(chain_hex(0), "0000000000000000");
        assert_eq!(chain_hex(0xdead_beef), "00000000deadbeef");
        assert_eq!(chain_hex(u64::MAX), "ffffffffffffffff");
    }
}
