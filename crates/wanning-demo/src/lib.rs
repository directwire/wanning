//! wanning-demo 的库面:真实消费 fail-closed 护栏([`guard`])、离线场景([`scenario`])、
//! 全链 mock 闭环场景([`full_loop`],W-29)、渠道 adapter(京东 [`jd`] / 支付宝
//! [`alipay`] / 微信 [`wechat`];共用类型在 [`channel`],传输在 [`http`])、渠道签名
//! 管线([`signing`],W-28,报文层零网络)、静态审计回放页([`audit_html`])、老板侧
//! 审计锚点命令([`anchor_cmd`],W-23,HMAC v1)与第三方可验锚点 v2([`anchor_v2`],
//! W-31,ed25519)、本地 JSON mock server([`mock_server`],场景运行时与集成测试共用)。
//!
//! bin(`main.rs`)只做参数解析与终端展示;一切可测逻辑都在这里,
//! 「设/不设 env 两路」「离线闭环」「导出回放页」「签/验锚点」都有测试实证。

pub mod alipay;
pub mod anchor_cmd;
pub mod anchor_v2;
pub mod audit_html;
pub mod channel;
pub mod decision;
pub mod full_loop;
pub mod guard;
pub mod http;
pub mod jd;
pub mod meituan;
pub mod mock_server;
pub mod scenario;
pub mod signing;
pub mod wechat;
