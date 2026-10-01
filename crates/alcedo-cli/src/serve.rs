//! `alcedo serve` —— 常驻 HTTP 服务。
//!
//! 给别的语言的程序用：Python / Node 的网站调本机接口就能用上 alcedo，
//! 而不必每次起一个 `alcedo` 进程。后者每次都要重新做 DNS + TLS 握手
//! （实测首次解析比热连接慢 160~390ms，YouTube 能慢出 800ms），常驻进程里
//! 连接池、游客身份、结果缓存全都能复用。
//!
//! ```text
//! GET /parse?url=<链接或分享文案>      解析
//! GET /parse?source=douyin&id=<作品ID> 已知平台和 ID 时直接解析
//! GET /parse?media=1                   响应里附上各直链的签名代理地址
//! GET /media?url=<地址>&e=<过期>&s=<签名>
//!                                      签名媒体代理：浏览器 <video>/<img>
//!                                      直接用，不用管 CDN 的 Referer
//! GET /health                          存活检查（缓存条数、中继保温、按平台×原因的解析统计）
//! ```
//!
//! 成功返回 200 + `VideoInfo` 的 JSON；失败返回对应状态码 +
//! `{"reason": "...", "message": "...", "detail": "..."}`，`reason` 取值见
//! [`alcedo::Reason`]。
//!
//! `/media` 故意**不**检查 Bearer 令牌——浏览器放不了自定义请求头，签名本身
//! 就是这一层的鉴权；而能拿到签名的前提是调过（令牌保护的）/parse。签名密钥
//! 优先取 `ALCEDO_MEDIA_SECRET`，没配就进程启动时随机生成（重启后旧签名
//! 全部失效；要跨重启有效就显式配一个）。

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use alcedo::{Client, Reason, Source, VideoInfo};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::media;

/// 默认只听本机：这是给同机上的网站调的，不是公网服务。
const DEFAULT_LISTEN: &str = "127.0.0.1:7878";

/// 默认预热的平台：国内最常用的三家。
const DEFAULT_PREWARM: &[Source] = &[Source::DouYin, Source::BiliBili, Source::RedBook];

/// 保温间隔。连接池空闲 300 秒就回收（见 `alcedo::http`），赶在那之前
/// 再碰一次，低流量时也不会让某个用户撞上冷连接。
const KEEP_WARM_EVERY: Duration = Duration::from_secs(240);

/// 中继保温间隔。边缘节点侧的空闲超时在 70~130 秒之间（跨洋链路实测），
/// 60 秒一个探针保证连接永远在热的那一侧。探针是「带令牌、不带 url」的
/// 空请求，边缘函数直接 400，不对任何平台产生出站流量——冷握手实测
/// 1.3s，热连接 0.18s，一个空来回换掉这 1.1s 很划算。
const RELAY_KEEPALIVE_EVERY: Duration = Duration::from_secs(60);

/// /media 的并发转发槽。媒体转发是长流，一个浏览器拖着 `<video>` 能开
/// 好几十条 Range 请求，不设闸的话几个播放器就能把出口占满。
const MAX_MEDIA_STREAMS: usize = 16;

/// 服务配置，来自命令行参数和环境变量。
#[derive(Debug)]
pub(crate) struct Options {
    listen: SocketAddr,
    prewarm: Vec<Source>,
    token: Option<String>,
}

impl Options {
    /// 解析 `serve` 之后的参数。参数优先，其次环境变量，最后默认值。
    pub(crate) fn from_args(args: &[String]) -> Result<Self, String> {
        let mut listen = std::env::var("ALCEDO_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.into());
        let mut prewarm = std::env::var("ALCEDO_PREWARM").ok();

        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--listen" => listen = it.next().ok_or("--listen 后面要跟地址")?.clone(),
                "--prewarm" => {
                    prewarm = Some(it.next().ok_or("--prewarm 后面要跟平台列表")?.clone())
                }
                other => return Err(format!("不认识的参数: {other}")),
            }
        }

        let listen = listen
            .parse()
            .map_err(|e| format!("监听地址无效 {listen}: {e}"))?;
        let prewarm = match prewarm.as_deref().map(str::trim) {
            None => DEFAULT_PREWARM.to_vec(),
            Some("" | "none") => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Source::from_str(s).map_err(|_| format!("认不出这个平台: {s}")))
                .collect::<Result<_, _>>()?,
        };
        let token = std::env::var("ALCEDO_SERVE_TOKEN")
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());

        Ok(Self {
            listen,
            prewarm,
            token,
        })
    }
}

struct AppState {
    client: Client,
    token: Option<String>,
    stats: Stats,
    /// /media 的转发槽
    media_streams: Arc<Semaphore>,
}

/// 按平台 × 原因累计解析结果，挂在 `/health` 上。
///
/// 平台改版时哪个解析器的 `parse` 类错误突然涨、哪个平台整体被风控，一眼
/// 可见——这正是排障时最想知道的两件事。进程内存里的计数器，重启归零：
/// 是排障用的仪表，不是审计日志。
#[derive(Default)]
struct Stats {
    /// (平台, 原因) -> 次数。原因 `ok` 表示成功。
    counts: Mutex<BTreeMap<&'static str, BTreeMap<&'static str, u64>>>,
}

impl Stats {
    fn record(&self, source: &'static str, reason: &'static str) {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(source)
            .or_default()
            .entry(reason)
            .and_modify(|c| *c += 1)
            .or_insert(1);
    }

    fn snapshot(&self) -> BTreeMap<&'static str, BTreeMap<&'static str, u64>> {
        self.counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// 跑服务，直到收到 Ctrl-C。
pub(crate) async fn run(opts: Options) -> Result<(), String> {
    let client = Client::new().map_err(|e| format!("初始化失败: {e}"))?;

    if !opts.prewarm.is_empty() {
        let c = client.clone();
        let sources = opts.prewarm.clone();
        tokio::spawn(async move {
            loop {
                c.prewarm(&sources).await;
                tokio::time::sleep(KEEP_WARM_EVERY).await;
            }
        });
    }

    // 中继保温和预热是两回事：走中继时平台那一跳发生在边缘函数里，本地
    // 根本没有平台连接池，唯一要焐的是到边缘节点那条连接——所以配了中继
    // 就自动开，不依赖 ALCEDO_PREWARM。
    let relay_keepalive = client.config().relay_cn.is_some();
    if relay_keepalive {
        let c = client.clone();
        tokio::spawn(async move {
            loop {
                c.ping_relay().await;
                tokio::time::sleep(RELAY_KEEPALIVE_EVERY).await;
            }
        });
    }

    let state = Arc::new(AppState {
        client,
        token: opts.token.clone(),
        stats: Stats::default(),
        media_streams: Arc::new(Semaphore::new(MAX_MEDIA_STREAMS)),
    });
    let app = Router::new()
        .route("/parse", get(parse))
        .route("/media", get(media))
        .route("/health", get(health))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(opts.listen)
        .await
        .map_err(|e| format!("监听 {} 失败: {e}", opts.listen))?;
    let names: Vec<&str> = opts.prewarm.iter().map(|s| s.as_str()).collect();
    eprintln!(
        "alcedo serve 已启动: http://{}  预热: {}  中继保温: {}  鉴权: {}",
        opts.listen,
        if names.is_empty() {
            "无".to_owned()
        } else {
            names.join(",")
        },
        if relay_keepalive { "60s" } else { "无" },
        if opts.token.is_some() { "开" } else { "关" },
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| format!("服务异常退出: {e}"))
}

#[derive(Deserialize)]
struct ParseQuery {
    url: Option<String>,
    source: Option<String>,
    id: Option<String>,
    /// 1 = 响应里附上各直链对应的签名代理地址（`media` 字段，见 `/media`）
    media: Option<u8>,
}

async fn parse(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ParseQuery>,
) -> Response {
    if let Some(resp) = check_token(state.token.as_deref(), &headers) {
        return resp;
    }

    let started = Instant::now();
    let (what, label, result) = match (q.url, q.source, q.id) {
        (Some(url), _, _) => {
            // 统计的来源标注：成功时结果里有权威的 source，失败时只能先从
            // 链接猜一个；猜不出（整段文案、不认识的站点）记作 "-"
            let label = alcedo::util::extract_url(&url)
                .and_then(alcedo::registry::detect)
                .map_or("-", Source::as_str);
            let r = state.client.parse(&url).await;
            (url, label, r)
        }
        (None, Some(src), Some(id)) => match Source::from_str(&src) {
            Ok(s) => {
                let r = state.client.parse_id(s, &id).await;
                (format!("{src}:{id}"), s.as_str(), r)
            }
            Err(_) => {
                return error_response(Reason::Unsupported, &format!("认不出这个平台: {src}"));
            }
        },
        _ => {
            return error_response(Reason::Unsupported, "要给 url，或者同时给 source 和 id");
        }
    };

    let ms = started.elapsed().as_millis();
    match result {
        Ok(info) => {
            let src = info.source.map_or("?", Source::as_str);
            state.stats.record(src, "ok");
            eprintln!("parse ok   {src:<10} {ms:>5}ms  {}", shorten(&what));
            let mut payload = serde_json::to_value(&info).expect("VideoInfo 序列化不会失败");
            if q.media == Some(1) {
                payload["media"] = serde_json::to_value(media_map(&info)).unwrap_or_default();
            }
            Json(payload).into_response()
        }
        Err(e) => {
            state.stats.record(label, e.reason.as_str());
            eprintln!(
                "parse fail {:<10} {ms:>5}ms  {}  {}",
                e.reason,
                shorten(&what),
                e.detail
            );
            error_body(e.reason, &e.to_string(), &e.detail)
        }
    }
}

async fn health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(resp) = check_token(state.token.as_deref(), &headers) {
        return resp;
    }
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "cache_entries": alcedo::result_cache().len(),
        "relay_keepalive": state.client.config().relay_cn.is_some(),
        "stats": state.stats.snapshot(),
    }))
    .into_response()
}

// ---------------------------------------------------------------- 签名媒体代理

/// 签名的有效期。要盖住各平台直链自身的寿命（B 站 2 小时是最长的常规档），
/// 又不能长到让泄露的签名变成永久通行证。
const MEDIA_TTL_SECS: u64 = 6 * 3600;

/// 签名密钥。`ALCEDO_MEDIA_SECRET` 优先；没配就进程启动时从系统熵生成一个
/// 随机的——重启后旧签名全部失效，要跨重启有效就显式配一个。
fn media_secret() -> &'static [u8] {
    static SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    SECRET
        .get_or_init(|| match std::env::var("ALCEDO_MEDIA_SECRET") {
            Ok(s) if !s.trim().is_empty() => s.trim().as_bytes().to_vec(),
            _ => {
                // RandomState 每次构造都从操作系统取熵（SipHash 的密钥），拼 4 段
                // 64 位凑 32 字节。密钥生命周期只有进程本身，不值得为此多拉依赖
                use std::hash::{BuildHasher, Hasher};
                (0..4u64)
                    .flat_map(|i| {
                        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
                        h.write_u64(i);
                        h.finish().to_le_bytes()
                    })
                    .collect()
            }
        })
        .as_slice()
}

/// `url` + 过期时刻的 HMAC-SHA256，取前 32 个十六进制位（对齐 Python 侧 net.sign）。
fn media_sign(url: &str, exp: u64) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(media_secret()).expect("HMAC 接受任意长度密钥");
    mac.update(url.as_bytes());
    mac.update(b"\n");
    mac.update(exp.to_string().as_bytes());
    let digest = mac.finalize().into_bytes();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..32].to_owned()
}

/// 给一条直链生成 `/media` 代理地址（相对路径，同源直接拼）。
fn signed_media_path(url: &str) -> String {
    let exp = unix_now() + MEDIA_TTL_SECS;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", url)
        .append_pair("e", &exp.to_string())
        .append_pair("s", &media_sign(url, exp))
        .finish();
    format!("/media?{query}")
}

/// `?media=1` 时响应里附带的 `media` 字段：直链 → 签名代理地址。
/// 浏览器拿代理地址就能播 / 能下载，CDN 的 Referer 与跳转都不用上层操心。
fn media_map(info: &VideoInfo) -> BTreeMap<String, String> {
    let mut raw: Vec<&str> = vec![info.video_url.as_str(), info.cover_url.as_str()];
    for img in &info.images {
        raw.push(img.url.as_str());
        raw.push(img.live_photo_url.as_str());
    }
    for f in &info.formats {
        raw.push(f.url.as_str());
        raw.push(f.video_url.as_str());
        raw.push(f.audio_url.as_str());
    }
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|u| !u.is_empty() && seen.insert(*u))
        .map(|u| (u.to_owned(), signed_media_path(u)))
        .collect()
}

#[derive(Deserialize)]
struct MediaQuery {
    url: String,
    /// 过期时刻（Unix 秒），参与签名
    e: Option<u64>,
    /// 签名
    s: Option<String>,
}

/// 签名媒体代理：只转发自己解析出来的地址。
///
/// 签名对（url + 过期时刻）验证、逐条过 SSRF 的协议与主机名检查、按 CDN
/// 域名补 Referer，`Range` 原样透传（播放器拖进度条全靠它），响应头白名单
/// 透传。3xx 原样交回给浏览器自己跟——我们不为一个不认识的目标站掏 DNS。
async fn media(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MediaQuery>,
) -> Response {
    let exp = q.e.unwrap_or(0);
    if exp <= unix_now() {
        return forbidden("签名已过期，重新解析再拿");
    }
    if !constant_time_eq(
        media_sign(&q.url, exp).as_bytes(),
        q.s.as_deref().unwrap_or("").as_bytes(),
    ) {
        return forbidden("签名不对，这个地址不是解析结果里给的");
    }
    let parsed = match url::Url::parse(&q.url) {
        Ok(u) => u,
        Err(_) => return error_response(Reason::Unsupported, "地址无效"),
    };
    if !alcedo::http::ssrf::url_allowed(&parsed) {
        return error_response(Reason::Unsupported, "不允许的地址");
    }

    let mut upstream = media::client()
        .get(parsed)
        .headers(media::headers_for(&q.url));
    if let Some(range) = headers.get("range").and_then(|v| v.to_str().ok()) {
        upstream = upstream.header("range", range);
    }
    let resp = match upstream.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"reason": "network", "message": format!("拉取失败: {e}")})),
            )
                .into_response()
        }
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = HeaderMap::new();
    for key in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "etag",
        "last-modified",
        "location",
    ] {
        if let Some(v) = resp.headers().get(key) {
            out.insert(key, v.clone());
        }
    }
    if status.is_success() && !out.contains_key("accept-ranges") {
        out.insert("accept-ranges", HeaderValue::from_static("bytes"));
    }
    out.insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=3600"),
    );

    // 满了就明确拒绝，别让第 17 条流把前 16 条挤成慢动作
    let permit = match state.media_streams.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"reason": "blocked", "message": "转发通道满了，稍后再试"})),
            )
                .into_response()
        }
    };
    // 令牌搬进响应体里：流断它才走，不能在 handler 返回时提前释放
    let body = Body::from_stream(resp.bytes_stream().map(move |chunk| {
        let _permit = &permit;
        chunk
    }));
    (status, out, body).into_response()
}

fn forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"reason": "forbidden", "message": message})),
    )
        .into_response()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 配了 `ALCEDO_SERVE_TOKEN` 就要求 `Authorization: Bearer <token>`。
fn check_token(expected: Option<&str>, headers: &HeaderMap) -> Option<Response> {
    let expected = expected?;
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if given.is_some_and(|g| constant_time_eq(g.as_bytes(), expected.as_bytes())) {
        return None;
    }
    Some(
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"reason": "unauthorized", "message": "缺少或错误的令牌"})),
        )
            .into_response(),
    )
}

/// 比较令牌时不因为前缀对上就提前返回，免得按响应时间逐字节猜出令牌。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error_response(reason: Reason, detail: &str) -> Response {
    let message = alcedo::Error::new(reason, detail).to_string();
    error_body(reason, &message, detail)
}

fn error_body(reason: Reason, message: &str, detail: &str) -> Response {
    (
        status_for(reason),
        Json(json!({
            "reason": reason.as_str(),
            "message": message,
            "detail": detail,
        })),
    )
        .into_response()
}

/// 原因 -> HTTP 状态码。调用方主要看 `reason`，状态码是给日志和监控看的。
fn status_for(reason: Reason) -> StatusCode {
    match reason {
        Reason::Unsupported => StatusCode::BAD_REQUEST,
        Reason::Deleted => StatusCode::NOT_FOUND,
        Reason::Login | Reason::Restricted => StatusCode::FORBIDDEN,
        Reason::Empty => StatusCode::UNPROCESSABLE_ENTITY,
        Reason::Timeout => StatusCode::GATEWAY_TIMEOUT,
        // 被上游风控、连不上、页面变了：都是"上游出了问题"
        Reason::Blocked | Reason::Network | Reason::Parse => StatusCode::BAD_GATEWAY,
    }
}

/// 日志里别把整段分享文案打出来。
fn shorten(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn options_parse_flags() {
        let o = Options::from_args(&args(&[
            "--listen",
            "0.0.0.0:9000",
            "--prewarm",
            "douyin, bilibili",
        ]))
        .unwrap();
        assert_eq!(o.listen.port(), 9000);
        assert_eq!(o.prewarm, vec![Source::DouYin, Source::BiliBili]);

        let o = Options::from_args(&args(&["--prewarm", "none"])).unwrap();
        assert!(o.prewarm.is_empty());

        assert!(Options::from_args(&args(&["--prewarm", "nope"])).is_err());
        assert!(Options::from_args(&args(&["--listen", "bad"])).is_err());
        assert!(Options::from_args(&args(&["--what"])).is_err());
    }

    #[test]
    fn token_is_enforced() {
        let mut h = HeaderMap::new();
        assert!(check_token(None, &h).is_none(), "没配令牌就不校验");
        assert!(check_token(Some("s3cret"), &h).is_some());
        h.insert("authorization", "Bearer wrong!".parse().unwrap());
        assert!(check_token(Some("s3cret"), &h).is_some());
        h.insert("authorization", "Bearer s3cret".parse().unwrap());
        assert!(check_token(Some("s3cret"), &h).is_none());
    }

    #[test]
    fn long_inputs_are_shortened_on_char_boundaries() {
        let s = "抖".repeat(100);
        let out = shorten(&s);
        assert_eq!(out.chars().count(), 81);
        assert!(out.ends_with('…'));
        assert_eq!(shorten(" a "), "a");
    }

    #[test]
    fn stats_counts_by_source_and_reason() {
        let s = Stats::default();
        s.record("douyin", "ok");
        s.record("douyin", "ok");
        s.record("douyin", "deleted");
        s.record("-", "unsupported");
        let snap = s.snapshot();
        assert_eq!(snap["douyin"]["ok"], 2);
        assert_eq!(snap["douyin"]["deleted"], 1);
        assert_eq!(snap["-"]["unsupported"], 1);
        assert!(!snap.contains_key("bilibili"), "没出现的平台不该有条目");
    }

    #[test]
    fn media_signing_round_trip_and_tamper() {
        let url = "https://v11.365yg.com/a.mp4?x=1";
        let exp = unix_now() + 60;
        let sig = media_sign(url, exp);
        assert_eq!(sig.len(), 32);
        // url / exp 任何一边被动过都通不过
        assert_ne!(media_sign("https://v11.365yg.com/a.mp4?x=2", exp), sig);
        assert_ne!(media_sign(url, exp + 1), sig);

        // 签名地址拆回来的参数要能原样验过——前端只管整条拿来用
        let path = signed_media_path(url);
        assert!(path.starts_with("/media?url="), "{path}");
        let pairs: Vec<(String, String)> = url::Url::parse(&format!("https://serve{path}"))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let u = &pairs.iter().find(|(k, _)| k == "url").unwrap().1;
        let e = &pairs.iter().find(|(k, _)| k == "e").unwrap().1;
        let s = &pairs.iter().find(|(k, _)| k == "s").unwrap().1;
        assert_eq!(u, url);
        assert_eq!(
            media_sign(u, e.parse().unwrap()),
            *s,
            "round-trip 必须能验过"
        );
    }

    #[test]
    fn media_map_dedups_and_skips_empty() {
        let info = VideoInfo {
            video_url: "https://cdn/a.mp4".into(),
            cover_url: "https://cdn/cover.jpg".into(),
            formats: vec![alcedo::Format {
                url: "https://cdn/a.mp4".into(),
                ..alcedo::Format::direct("720p", "", 720)
            }],
            ..Default::default()
        };
        let m = media_map(&info);
        assert_eq!(m.len(), 2, "重复与空地址不该出现");
        assert!(m.contains_key("https://cdn/a.mp4"));
        assert!(m.contains_key("https://cdn/cover.jpg"));
    }
}
