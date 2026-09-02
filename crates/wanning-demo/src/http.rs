//! 共享 HTTP 传输层:全部渠道 adapter(京东 / 支付宝 / 微信)共用(可注入,测试打本地 mock)。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    pub status: Option<u16>,
    pub timeout: bool,
    pub message: String,
}

impl fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.timeout {
            write!(f, "超时: {message}", message = self.message)
        } else {
            match self.status {
                Some(status) => write!(f, "HTTP {status}: {message}", message = self.message),
                None => write!(f, "连接失败: {message}", message = self.message),
            }
        }
    }
}

/// JSON over HTTP 的 POST 抽象。TODO(账户开通后):若网关要求 GET+签名参数,再扩方法。
pub trait ApiTransport: fmt::Debug + Send + Sync {
    fn post_json(
        &self,
        url: &str,
        body: &str,
        headers: &[(String, String)],
    ) -> Result<String, HttpFailure>;
}

/// ureq 实现(生产;今晚被护栏挡住,不会指向真端点)。
#[derive(Debug)]
pub struct UreqApiTransport;

impl ApiTransport for UreqApiTransport {
    fn post_json(
        &self,
        url: &str,
        body: &str,
        headers: &[(String, String)],
    ) -> Result<String, HttpFailure> {
        let mut request = ureq::post(url)
            .timeout(std::time::Duration::from_secs(30))
            .set("Content-Type", "application/json");
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let response = request.send_string(body).map_err(|e| {
            let status = match &e {
                ureq::Error::Status(code, _) => Some(*code),
                ureq::Error::Transport(_) => None,
            };
            HttpFailure {
                status,
                timeout: false,
                message: e.to_string(),
            }
        })?;
        let status = response.status();
        response.into_string().map_err(|e| HttpFailure {
            status: Some(status),
            timeout: false,
            message: format!("读响应体失败: {e}"),
        })
    }
}
