//! 外部签名器：把"每季度会变的东西"挡在仓库外面。
//!
//! ## 为什么不把 a_bogus 直接写进来
//!
//! 抖音的 `a_bogus` 是 SM3 管线 + VM 混淆 + 熵字节的产物，而且**大约每季度
//! 轮换一次算法**。把它硬编进 Rust 的后果很具体：每三个月要重新逆向一遍、
//! 重新发一个版本，否则抖音解析直接死。对一个没有全职维护者的项目来说，
//! 这是全仓库维护成本最高的一块，而它偏偏又最容易失修。
//!
//! 所以这里采用 yt-dlp 应对 PO Token 时的同一个思路——**提供者框架**：
//! 签名算法放在外部程序或服务里，算法变了换那个，不用碰也不用重新编译本仓库。
//!
//! ## 怎么配
//!
//! ```text
//! ALCEDO_SIGNER_DOUYIN=cmd:/opt/alcedo/douyin-signer      # 起一个子进程，stdin/stdout 走 JSON
//! ALCEDO_SIGNER_DOUYIN=http://127.0.0.1:9000/sign     # 调一个常驻服务（推荐，省掉进程启动）
//! ```
//!
//! 约定的协议（两种传输一样）：
//!
//! ```jsonc
//! // 请求
//! { "platform": "douyin", "url": "https://...", "query": "aweme_ids=%5B123%5D",
//!   "user_agent": "Mozilla/5.0 ...", "body": "" }
//! // 响应：三个字段都可选，按需合并进请求
//! { "query": { "a_bogus": "..." }, "headers": { "X-Foo": "..." }, "cookies": { "msToken": "..." } }
//! ```
//!
//! 没配就是没配——解析器走免签名那条路，**不会**因此报错。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::http::{Http, Req};
use crate::model::Source;

/// 签名器返回的东西。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Signature {
    /// 要追加到 query string 上的参数
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    /// 要加的请求头
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 要加的 cookie
    #[serde(default)]
    pub cookies: BTreeMap<String, String>,
}

impl Signature {
    /// 什么都没签出来。
    pub fn is_empty(&self) -> bool {
        self.query.is_empty() && self.headers.is_empty() && self.cookies.is_empty()
    }

    /// 把签名结果贴到请求上。
    pub fn apply(&self, mut req: Req) -> Req {
        for (k, v) in &self.headers {
            req = req.header(k.clone(), v.clone());
        }
        for (k, v) in &self.cookies {
            req = req.cookie(k.clone(), v.clone());
        }
        req
    }

    /// 把 query 参数拼到地址后面。
    pub fn apply_url(&self, url: &str) -> String {
        if self.query.is_empty() {
            return url.to_owned();
        }
        let joined = self
            .query
            .iter()
            .map(|(k, v)| {
                let ev =
                    percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC);
                format!("{k}={ev}")
            })
            .collect::<Vec<_>>()
            .join("&");
        let sep = if url.contains('?') { '&' } else { '?' };
        format!("{url}{sep}{joined}")
    }
}

#[derive(Debug, Serialize)]
struct SignRequest<'a> {
    platform: &'a str,
    url: &'a str,
    query: &'a str,
    user_agent: &'a str,
    body: &'a str,
}

/// 配好的签名器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signer {
    /// 没配
    None,
    /// 子进程：stdin 收请求 JSON，stdout 吐响应 JSON
    Command(String),
    /// HTTP 服务
    Http(String),
}

impl Signer {
    /// 读这个平台配的签名器。
    pub fn for_source(source: Source) -> Self {
        let key = format!("ALCEDO_SIGNER_{}", source.as_str().to_ascii_uppercase());
        let Ok(raw) = std::env::var(&key) else {
            return Self::None;
        };
        Self::parse(raw.trim())
    }

    fn parse(raw: &str) -> Self {
        if raw.is_empty() {
            Self::None
        } else if let Some(cmd) = raw.strip_prefix("cmd:") {
            Self::Command(cmd.to_owned())
        } else if raw.starts_with("http://") || raw.starts_with("https://") {
            Self::Http(raw.to_owned())
        } else {
            // 没写前缀时按命令处理，比直接忽略更符合直觉
            Self::Command(raw.to_owned())
        }
    }

    /// 是否配了签名器。
    pub const fn is_configured(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// 要一份签名。**任何失败都返回空签名而不是报错**——签名器是增强项，
    /// 它挂了不该把本来能走通的免签名路径也一起带走。
    pub async fn sign(
        &self,
        http: &Http,
        source: Source,
        url: &str,
        query: &str,
        body: &str,
    ) -> Signature {
        let req = SignRequest {
            platform: source.as_str(),
            url,
            query,
            user_agent: http.user_agent(),
            body,
        };

        let result = match self {
            Self::None => return Signature::default(),
            Self::Command(cmd) => sign_via_command(cmd, &req).await,
            Self::Http(endpoint) => sign_via_http(http, endpoint, &req).await,
        };

        match result {
            Ok(sig) => sig,
            Err(e) => {
                tracing::warn!(source = source.as_str(), error = %e, "外部签名器不可用，改走免签名路径");
                Signature::default()
            }
        }
    }
}

async fn sign_via_command(cmd: &str, req: &SignRequest<'_>) -> Result<Signature, String> {
    use tokio::io::AsyncWriteExt;

    let payload = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    let mut parts = cmd.split_whitespace();
    let program = parts.next().ok_or("签名命令是空的")?;

    let mut child = tokio::process::Command::new(program)
        .args(parts)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("起不来 {program}: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload).await.map_err(|e| e.to_string())?;
        // 必须关掉 stdin，否则对面会一直等输入
        drop(stdin);
    }

    // 签名器卡住时不能拖着解析一起卡
    let out = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .map_err(|_| "签名器超时（5 秒）".to_owned())?
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(format!("签名器退出码 {:?}", out.status.code()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("签名器输出不是合法 JSON: {e}"))
}

async fn sign_via_http(
    http: &Http,
    endpoint: &str,
    req: &SignRequest<'_>,
) -> Result<Signature, String> {
    let value = serde_json::to_value(req).map_err(|e| e.to_string())?;
    let resp = http
        .send(Req::post(endpoint).json_body(&value))
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status.is_success() {
        return Err(format!("签名服务返回 HTTP {}", resp.status.as_u16()));
    }
    serde_json::from_slice(resp.bytes()).map_err(|e| format!("签名服务输出不是合法 JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_config_form() {
        assert_eq!(
            Signer::parse("cmd:/opt/signer"),
            Signer::Command("/opt/signer".into())
        );
        assert_eq!(
            Signer::parse("http://127.0.0.1:9000/sign"),
            Signer::Http("http://127.0.0.1:9000/sign".into())
        );
        // 没写前缀按命令处理
        assert_eq!(
            Signer::parse("/usr/bin/signer"),
            Signer::Command("/usr/bin/signer".into())
        );
        assert_eq!(Signer::parse(""), Signer::None);
        assert!(!Signer::None.is_configured());
    }

    #[test]
    fn query_params_are_appended_and_encoded() {
        let mut sig = Signature::default();
        sig.query.insert("a_bogus".into(), "aB+/=xyz".into());

        let with_q = sig.apply_url("https://x/api?id=1");
        assert!(with_q.starts_with("https://x/api?id=1&a_bogus="));
        assert!(
            !with_q.contains('+'),
            "特殊字符必须转义, 否则签名会被服务端解错"
        );

        let no_q = sig.apply_url("https://x/api");
        assert!(no_q.contains("?a_bogus="));
    }

    #[test]
    fn empty_signature_leaves_url_alone() {
        let sig = Signature::default();
        assert!(sig.is_empty());
        assert_eq!(sig.apply_url("https://x/api?id=1"), "https://x/api?id=1");
    }

    #[tokio::test]
    async fn a_broken_signer_degrades_instead_of_failing() {
        // 签名器是增强项：它挂了必须退回免签名路径，不能把解析一起拖死
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(Source::DouYin),
        )
        .unwrap();
        let signer = Signer::Command("这个程序根本不存在".into());
        let sig = signer
            .sign(&http, Source::DouYin, "https://x", "", "")
            .await;
        assert!(sig.is_empty(), "签名器不可用时应当给空签名, 而不是报错");
    }

    #[tokio::test]
    async fn none_signer_is_a_noop() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(Source::DouYin),
        )
        .unwrap();
        let sig = Signer::None
            .sign(&http, Source::DouYin, "https://x", "", "")
            .await;
        assert!(sig.is_empty());
    }

    #[test]
    fn signature_deserialises_from_the_documented_shape() {
        let raw =
            r#"{"query":{"a_bogus":"XYZ"},"headers":{"X-Foo":"1"},"cookies":{"msToken":"m"}}"#;
        let sig: Signature = serde_json::from_str(raw).unwrap();
        assert_eq!(sig.query["a_bogus"], "XYZ");
        assert_eq!(sig.headers["X-Foo"], "1");
        assert_eq!(sig.cookies["msToken"], "m");
        // 只给一部分字段也要能解
        let partial: Signature = serde_json::from_str(r#"{"query":{"k":"v"}}"#).unwrap();
        assert!(partial.headers.is_empty());
    }
}
