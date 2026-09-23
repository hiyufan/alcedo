//! HTTP 层：连接池、代理、重定向、SSRF、重试。
//!
//! 和 Python 版最大的结构性差别在这里。原来每个解析步骤都 `create_async_client()`
//! 新建一个 `httpx.AsyncClient`，也就是每次都重建连接池：一次抖音解析要发 3–4 个
//! 请求，等于 3–4 次 TCP + TLS 握手全部重来。这里按代理配置各留一个
//! [`reqwest::Client`]，进程内共享，连接、TLS 会话、DNS 缓存全部复用。
//!
//! 重定向自己跟而不是交给 reqwest：短链跳转是解析流程的一部分（好几个平台要读
//! 中间那一跳的 `Location`），而且每一跳都得重新过一遍 SSRF 检查。

pub mod identity;
pub mod resilience;
pub mod ssrf;
pub mod ua;

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, StatusCode};

use crate::error::{from_status, Error, Reason, Result};
use crate::model::Source;

/// 运行期配置。全部来自环境变量，进程启动时读一次。
#[derive(Debug, Clone)]
pub struct Config {
    /// 所有平台的兜底代理
    pub proxy: Option<String>,
    /// 只给国内平台用的代理（境外部署时抖音 / 小红书这些必须走它）
    pub proxy_cn: Option<String>,
    pub connect_timeout: Duration,
    /// 单个请求的上限
    pub request_timeout: Duration,
    /// 一次完整解析的上限
    pub total_timeout: Duration,
    pub max_redirects: usize,
    /// 响应体上限，防着对面返回一个超大页面把内存吃光
    pub max_body_bytes: usize,
    /// DNS 层的内网地址拦截
    pub ssrf_enforce: bool,
    pub bilibili_cookie: Option<String>,
    pub xhs_cookie: Option<String>,
    pub douyin_cookie: Option<String>,
    pub youtube_cookie: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            proxy: None,
            proxy_cn: None,
            connect_timeout: Duration::from_secs(8),
            request_timeout: Duration::from_secs(20),
            total_timeout: Duration::from_secs(45),
            max_redirects: 8,
            max_body_bytes: 16 * 1024 * 1024,
            ssrf_enforce: true,
            bilibili_cookie: None,
            xhs_cookie: None,
            douyin_cookie: None,
            youtube_cookie: None,
        }
    }
}

impl Config {
    /// 从环境变量读。`PV_*` 是新名字，`PARSE_VIDEO_*` 是 Python 版留下的，
    /// 两个都认，迁移期不用改部署脚本。
    pub fn from_env() -> Self {
        let d = Config::default();
        Config {
            proxy: env_any(&["PV_PROXY", "PARSE_VIDEO_PROXY"]),
            proxy_cn: env_any(&["PV_PROXY_CN", "PARSE_VIDEO_PROXY_CN"]),
            connect_timeout: env_secs("PV_CONNECT_TIMEOUT", d.connect_timeout),
            request_timeout: env_secs("PV_REQUEST_TIMEOUT", d.request_timeout),
            total_timeout: env_secs("PV_TOTAL_TIMEOUT", d.total_timeout),
            max_redirects: env_num("PV_MAX_REDIRECTS", d.max_redirects),
            max_body_bytes: env_num("PV_MAX_BODY_BYTES", d.max_body_bytes),
            ssrf_enforce: !matches!(
                env_any(&["PV_SSRF_DNS", "PARSE_VIDEO_SSRF_DNS"]).as_deref(),
                Some("0")
            ),
            bilibili_cookie: env_any(&["PV_BILI_COOKIE", "PARSE_VIDEO_BILI_COOKIE"]),
            xhs_cookie: env_any(&["PV_XHS_COOKIE", "PARSE_VIDEO_XHS_COOKIE"]),
            douyin_cookie: env_any(&["PV_DOUYIN_COOKIE", "PARSE_VIDEO_DOUYIN_COOKIE"]),
            youtube_cookie: env_any(&["PV_YOUTUBE_COOKIE", "PARSE_VIDEO_YOUTUBE_COOKIE"]),
        }
    }

    fn proxy_for(&self, source: Option<Source>) -> Option<&str> {
        if source.is_some_and(Source::is_cn) {
            if let Some(p) = self.proxy_cn.as_deref() {
                return Some(p);
            }
        }
        self.proxy.as_deref()
    }
}

fn env_any(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

fn env_secs(key: &str, default: Duration) -> Duration {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(default, Duration::from_secs)
}

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------------ 连接池

/// 按代理配置缓存 `reqwest::Client`。
///
/// 一个 `Client` 内部就是连接池，clone 只是 Arc 加一，所以真正要避免的是
/// **重复构造**：每次 `Client::builder().build()` 都会新建 TLS 配置和连接池，
/// 把前面攒下的热连接全部作废。
static CLIENTS: RwLock<Vec<(String, Client)>> = RwLock::new(Vec::new());

/// 装 rustls 的加密后端。
///
/// 用 `rustls-no-provider` 就必须自己选一个 provider，否则第一次 TLS 握手会 panic。
/// 选 ring 是为了不把 aws-lc-rs 的 C 工具链要求（NASM / CMake）带进构建。
fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // 已经被宿主程序装过就不用管，`install_default` 返回 Err 即是这种情况
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn client_for(cfg: &Config, proxy: Option<&str>) -> Result<Client> {
    let key = proxy.unwrap_or("").to_owned();
    if let Some(c) = CLIENTS.read().iter().find(|(k, _)| *k == key) {
        return Ok(c.1.clone());
    }
    ensure_crypto_provider();

    let mut builder = Client::builder()
        .connect_timeout(cfg.connect_timeout)
        .timeout(cfg.request_timeout)
        // 重定向自己跟：要读中间那一跳的 Location，也要逐跳做 SSRF 检查
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(ssrf::SafeResolver::new(cfg.ssrf_enforce)))
        // 平台接口几乎全是 HTTPS/2，提前协商省一个 RTT
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_while_idle(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(8)
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true)
        .user_agent(ua::DESKTOP_FIXED);

    if let Some(p) = proxy {
        let proxy = reqwest::Proxy::all(p)
            .map_err(|e| Error::new(Reason::Network, format!("代理地址无效 {p}: {e}")))?;
        builder = builder.proxy(proxy);
    } else {
        // 不配代理时明确禁用系统代理：容器里继承到一个不通的 HTTP_PROXY
        // 会让所有解析莫名其妙超时，排查起来非常费劲。
        builder = builder.no_proxy();
    }

    let client = builder
        .build()
        .map_err(|e| Error::new(Reason::Network, format!("HTTP 客户端构造失败: {e}")))?;

    let mut w = CLIENTS.write();
    if let Some(c) = w.iter().find(|(k, _)| *k == key) {
        return Ok(c.1.clone()); // 并发下别人先建好了，用它的
    }
    w.push((key, client.clone()));
    Ok(client)
}

// ------------------------------------------------------------------ 请求

/// 一次请求的描述。
#[derive(Debug)]
pub struct Req {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    follow: bool,
    /// 跨跳转累积的 cookie。有几个平台（快手）第一跳发 cookie、第二跳才认。
    cookies: Vec<(String, String)>,
    /// 只取前 N 字节就断开，不算错。见 [`Req::head_bytes`]。
    head_bytes: Option<usize>,
}

impl Req {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::GET,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            follow: true,
            cookies: Vec::new(),
            head_bytes: None,
        }
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self {
            method: Method::POST,
            ..Self::get(url)
        }
    }

    pub fn header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn headers<I, K, V>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.headers
            .extend(it.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn referer(self, v: impl Into<String>) -> Self {
        self.header("Referer", v)
    }

    pub fn cookie(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.cookies.push((k.into(), v.into()));
        self
    }

    /// 整段 `a=1; b=2` 形式的 cookie（环境变量里配的就是这个形式）
    pub fn cookie_header(self, raw: impl Into<String>) -> Self {
        self.header("Cookie", raw)
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn json_body(self, value: &serde_json::Value) -> Self {
        let raw = serde_json::to_vec(value).unwrap_or_default();
        self.header("Content-Type", "application/json").body(raw)
    }

    pub fn form_body(self, raw: impl Into<Vec<u8>>) -> Self {
        self.header("Content-Type", "application/x-www-form-urlencoded")
            .body(raw)
    }

    /// 不跟重定向——调用方要自己读 `Location`。
    pub fn no_follow(mut self) -> Self {
        self.follow = false;
        self
    }

    /// 只读响应的前 `n` 字节，读够就断开连接。
    ///
    /// 给"只要 `<head>` 里那几个标签"的场景用：`og:title` / `<title>` 这类元数据
    /// 永远在文档开头，而平台的页面动辄几百 KB。整页下完再扔掉，白白多花的是
    /// 用户等待的时间。
    ///
    /// 和 `max_body_bytes` 不同：那个是防护上限，超了报错；这个是主动截断，
    /// 属于正常结果。
    pub fn head_bytes(mut self, n: usize) -> Self {
        self.head_bytes = Some(n);
        self
    }
}

/// 一次响应。body 已经读完并按 charset 解码。
pub struct Resp {
    pub status: StatusCode,
    /// 跟完重定向之后的最终地址
    pub url: String,
    pub headers: HeaderMap,
    body: Vec<u8>,
}

impl std::fmt::Debug for Resp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只报长度：body 动辄几百 KB，打进日志既没用又会泄露页面内容
        f.debug_struct("Resp")
            .field("status", &self.status)
            .field("url", &self.url)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Resp {
    /// 正文，按 `Content-Type` 里的 charset 解码。
    ///
    /// 六间房、美拍这类老站点还在返回 GBK，直接当 UTF-8 读会得到一串问号，
    /// 标题和作者名全废。
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        let label = self.header("content-type").and_then(|ct| {
            ct.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("charset="))
        });

        match label.map(|l| l.trim_matches('"')) {
            None => String::from_utf8_lossy(&self.body),
            Some(l) => match encoding_rs::Encoding::for_label(l.as_bytes()) {
                Some(enc) if enc != encoding_rs::UTF_8 => {
                    let (decoded, _, _) = enc.decode(&self.body);
                    std::borrow::Cow::Owned(decoded.into_owned())
                }
                _ => String::from_utf8_lossy(&self.body),
            },
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn json(&self) -> Result<serde_json::Value> {
        // 有几家接口会在 JSON 前面塞 BOM 或者 JSONP 包装, 交给调用方处理,
        // 这里只负责最直白的一种
        serde_json::from_slice(&self.body).map_err(|e| {
            Error::parse(format!(
                "接口没有返回合法 JSON: {} | 开头: {}",
                e,
                snippet(&self.body, 120)
            ))
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// `Location`，并按当前地址补全成绝对地址。
    pub fn location(&self) -> Option<String> {
        let raw = self.header("location")?;
        match url::Url::parse(raw) {
            Ok(u) => Some(u.to_string()),
            Err(_) => url::Url::parse(&self.url)
                .ok()?
                .join(raw)
                .ok()
                .map(|u| u.to_string()),
        }
    }

    pub fn error_for_status(&self) -> Result<()> {
        if self.status.is_success() || self.status.is_redirection() {
            Ok(())
        } else {
            Err(from_status(self.status.as_u16()))
        }
    }

    /// 响应里下发的 cookie（`name=value` 列表）。
    pub fn set_cookies(&self) -> Vec<(String, String)> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|raw| {
                let pair = raw.split(';').next()?;
                let (k, v) = pair.split_once('=')?;
                Some((k.trim().to_owned(), v.trim().to_owned()))
            })
            .collect()
    }
}

fn snippet(body: &[u8], n: usize) -> String {
    String::from_utf8_lossy(&body[..body.len().min(n)]).replace(['\n', '\r'], " ")
}

/// 一次解析所共享的 HTTP 句柄。
///
/// UA 在这里定一次，整个解析流程复用（见 [`ua`] 模块的说明）。
#[derive(Debug, Clone)]
pub struct Http {
    client: Client,
    cfg: Arc<Config>,
    user_agent: &'static str,
    source: Option<Source>,
}

impl Http {
    pub fn new(cfg: Arc<Config>, source: Option<Source>) -> Result<Self> {
        let client = client_for(&cfg, cfg.proxy_for(source))?;
        Ok(Self {
            client,
            cfg,
            user_agent: ua::pick(ua::Platform::Ios),
            source,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn source(&self) -> Option<Source> {
        self.source
    }

    /// 本次解析固定使用的 UA。
    pub fn user_agent(&self) -> &'static str {
        self.user_agent
    }

    /// 换一个平台的 UA（B 站、新片场这些要桌面端）。
    pub fn with_ua(mut self, platform: ua::Platform) -> Self {
        self.user_agent = ua::pick(platform);
        self
    }

    pub fn with_fixed_ua(mut self, agent: &'static str) -> Self {
        self.user_agent = agent;
        self
    }

    /// 发请求。自动跟重定向、逐跳查 SSRF、对网络类错误重试一次。
    pub async fn send(&self, req: Req) -> Result<Resp> {
        let mut attempt = 0;
        loop {
            match self.send_once(&req).await {
                Ok(r) => return Ok(r),
                Err(e) if attempt == 0 && e.reason.is_retryable() && req.body.is_none() => {
                    // 只重试幂等请求。POST 重发可能在对面产生第二条记录。
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    tracing::debug!(url = %req.url, error = %e, "请求失败, 重试一次");
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn send_once(&self, req: &Req) -> Result<Resp> {
        let mut current = req.url.clone();
        let mut cookies = req.cookies.clone();
        let mut hops = 0usize;

        loop {
            let parsed = url::Url::parse(&current)
                .map_err(|e| Error::unsupported(format!("链接无法解析 {current}: {e}")))?;
            if !ssrf::url_allowed(&parsed) {
                return Err(Error::new(
                    Reason::Network,
                    format!("拒绝访问非公网地址: {}", parsed.host_str().unwrap_or("?")),
                ));
            }

            let mut builder = self
                .client
                .request(req.method.clone(), parsed.clone())
                .header("User-Agent", self.user_agent)
                .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8");

            let mut header_map = HeaderMap::new();
            for (k, v) in &req.headers {
                if let (Ok(name), Ok(val)) = (k.parse::<HeaderName>(), HeaderValue::from_str(v)) {
                    header_map.insert(name, val);
                }
            }
            if !cookies.is_empty() {
                let joined = cookies
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                // 调用方显式给的 Cookie 头优先，别被跳转攒下来的覆盖掉
                if !header_map.contains_key("cookie") {
                    if let Ok(val) = HeaderValue::from_str(&joined) {
                        header_map.insert("cookie", val);
                    }
                }
            }
            builder = builder.headers(header_map);

            if let Some(b) = &req.body {
                builder = builder.body(b.clone());
            }

            let resp = builder.send().await?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let final_url = resp.url().to_string();

            // 跟下一跳之前先把本跳的 cookie 收好
            for (k, v) in cookies_from(&headers) {
                if let Some(slot) = cookies.iter_mut().find(|(ek, _)| *ek == k) {
                    slot.1 = v;
                } else {
                    cookies.push((k, v));
                }
            }

            if req.follow && status.is_redirection() {
                let next = headers
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|raw| parsed.join(raw).ok());
                if let Some(next) = next {
                    hops += 1;
                    if hops > self.cfg.max_redirects {
                        return Err(Error::new(
                            Reason::Network,
                            format!("重定向超过 {} 跳", self.cfg.max_redirects),
                        ));
                    }
                    current = next.to_string();
                    continue;
                }
            }

            let body = read_capped(resp, self.cfg.max_body_bytes, req.head_bytes).await?;
            return Ok(Resp {
                status,
                url: final_url,
                headers,
                body,
            });
        }
    }

    // ---------------------------------------------------------- 常用简写

    /// GET 并要求 2xx，返回正文。
    pub async fn get_text(&self, url: &str) -> Result<String> {
        let r = self.send(Req::get(url)).await?;
        r.error_for_status()?;
        Ok(r.text().into_owned())
    }

    /// GET 并要求 2xx，返回 JSON。
    pub async fn get_json(&self, url: &str) -> Result<serde_json::Value> {
        let r = self.send(Req::get(url)).await?;
        r.error_for_status()?;
        r.json()
    }

    /// 只要短链跳转后的地址，不下载正文。
    pub async fn resolve_redirect(&self, url: &str) -> Result<String> {
        let r = self.send(Req::get(url).no_follow()).await?;
        r.location().ok_or_else(|| {
            Error::deleted(format!("短链没有返回跳转地址 (HTTP {})", r.status.as_u16()))
        })
    }
}

fn cookies_from(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|raw| {
            let pair = raw.split(';').next()?;
            let (k, v) = pair.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// 读响应体，超过上限就断开。
///
/// 不用 `resp.bytes()` 是因为它会照着 Content-Length 一次性预分配，对面报一个
/// 假的 10GB 就能把进程打爆。
async fn read_capped(
    resp: reqwest::Response,
    max: usize,
    head_bytes: Option<usize>,
) -> Result<Vec<u8>> {
    use futures_util::StreamExt;

    let limit = head_bytes.map_or(max, |n| n.min(max));
    // 只是 Vec 的预分配提示，取不到或超了都不影响正确性
    let hint = resp.content_length().map_or(16 * 1024, |n| {
        usize::try_from(n).unwrap_or(limit).min(limit)
    });
    let mut out = Vec::with_capacity(hint);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if out.len() + chunk.len() > limit {
            if head_bytes.is_some() {
                // 主动截断：补到刚好 limit 就收手，剩下的连接直接丢掉
                let take = limit.saturating_sub(out.len());
                out.extend_from_slice(&chunk[..take]);
                break;
            }
            return Err(Error::new(
                Reason::Parse,
                format!("响应体超过 {max} 字节上限，已中断"),
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn cn_proxy_only_applies_to_cn_sources() {
        let cfg = Config {
            proxy: Some("http://global:1".into()),
            proxy_cn: Some("http://cn:1".into()),
            ..Default::default()
        };
        assert_eq!(cfg.proxy_for(Some(Source::DouYin)), Some("http://cn:1"));
        assert_eq!(
            cfg.proxy_for(Some(Source::YouTube)),
            Some("http://global:1")
        );
        assert_eq!(cfg.proxy_for(None), Some("http://global:1"));
    }

    #[test]
    fn falls_back_to_global_proxy() {
        let cfg = Config {
            proxy: Some("http://g:1".into()),
            ..Default::default()
        };
        assert_eq!(cfg.proxy_for(Some(Source::DouYin)), Some("http://g:1"));
    }

    #[test]
    fn client_pool_does_not_rebuild() {
        // 每次 build() 都会丢掉攒下的热连接, 所以同一套代理配置只能构造一次
        let cfg = Config::default();
        let _a = client_for(&cfg, Some("http://pool-test.invalid:1")).unwrap();
        let before = CLIENTS.read().len();
        let _b = client_for(&cfg, Some("http://pool-test.invalid:1")).unwrap();
        assert_eq!(
            CLIENTS.read().len(),
            before,
            "同一个代理配置不该新建第二个 Client"
        );
    }

    #[tokio::test]
    async fn head_bytes_truncates_instead_of_failing() {
        use futures_util::stream;
        // 用 reqwest 自己的 Response 包一段分块响应
        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> = vec![
            Ok(bytes::Bytes::from_static(b"<html><head><title>OK")),
            Ok(bytes::Bytes::from(vec![b'x'; 8192])),
        ];
        let body = reqwest::Body::wrap_stream(stream::iter(chunks));
        let resp = reqwest::Response::from(http::Response::new(body));

        let out = read_capped(resp, 1024 * 1024, Some(32)).await.unwrap();
        assert_eq!(out.len(), 32, "应当截断到上限, 而不是报错");
        assert!(out.starts_with(b"<html><head><title>OK"));
    }

    #[tokio::test]
    async fn oversized_body_without_head_bytes_is_an_error() {
        use futures_util::stream;
        let chunks: Vec<std::result::Result<bytes::Bytes, std::io::Error>> =
            vec![Ok(bytes::Bytes::from(vec![b'x'; 4096]))];
        let body = reqwest::Body::wrap_stream(stream::iter(chunks));
        let resp = reqwest::Response::from(http::Response::new(body));

        let err = read_capped(resp, 1024, None).await.unwrap_err();
        assert_eq!(err.reason, Reason::Parse);
        assert!(err.detail.contains("上限"));
    }

    #[test]
    fn gbk_body_is_decoded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/html; charset=gbk"),
        );
        // "中文" 的 GBK 编码
        let body = vec![0xD6, 0xD0, 0xCE, 0xC4];
        let r = Resp {
            status: StatusCode::OK,
            url: "https://x/".into(),
            headers,
            body,
        };
        assert_eq!(r.text(), "中文");
    }

    #[test]
    fn location_is_resolved_against_base() {
        let mut headers = HeaderMap::new();
        headers.insert("location", HeaderValue::from_static("/video/123"));
        let r = Resp {
            status: StatusCode::FOUND,
            url: "https://v.douyin.com/abc".into(),
            headers,
            body: Vec::new(),
        };
        assert_eq!(r.location().unwrap(), "https://v.douyin.com/video/123");
    }

    #[test]
    fn set_cookie_parsing() {
        let mut headers = HeaderMap::new();
        headers.append(
            "set-cookie",
            HeaderValue::from_static("ttwid=abc123; Path=/; HttpOnly"),
        );
        headers.append(
            "set-cookie",
            HeaderValue::from_static("did=xyz; Max-Age=60"),
        );
        let r = Resp {
            status: StatusCode::OK,
            url: "https://x/".into(),
            headers,
            body: Vec::new(),
        };
        let cookies = r.set_cookies();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0], ("ttwid".into(), "abc123".into()));
    }
}
