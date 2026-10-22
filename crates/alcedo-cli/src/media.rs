//! 媒体直链的共用件：客户端、请求头、文件名与扩展名。
//!
//! 直链指向的是平台 CDN，不是解析接口：带对 Referer 一般就 200，缺了就 403。
//! 这里的客户端和解析库的连接池是两回事——媒体是大流量长流，只设连接与读
//! 超时、不设总超时（下载 1GB 的视频不该被"总时长"掐死），也不跟跳转：
//! 302 原样交回调用方，浏览器自己会跟，我们也不为一个不认识的目标站掏 DNS。

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};

use alcedo::http::ua;

/// 长流客户端。连接池进程内共享；DNS 走 [`alcedo::http::ssrf::SafeResolver`]，
/// 就算 URL 被构造出来指向内网，也在解析层拦住。
pub(crate) fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build(None).expect("媒体客户端构建失败"))
}

/// 拉这条直链用哪个客户端。
///
/// googlevideo 的地址带着解析时出口的 IP（`ip=` 参数），换个出口去拉就 403，
/// 所以它和解析一样走 `ALCEDO_PROXY`。国内 CDN 的地址不绑 IP，照旧直连，
/// 大文件不必绕代理。
pub(crate) fn client_for(url: &str) -> &'static reqwest::Client {
    static PROXIED: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    if !is_googlevideo(url) {
        return client();
    }
    PROXIED
        .get_or_init(|| {
            let proxy = alcedo::Config::from_env().proxy?;
            build(Some(&proxy))
                .inspect_err(|e| eprintln!("ALCEDO_PROXY 用不了，YouTube 媒体改为直连: {e}"))
                .ok()
        })
        .as_ref()
        .unwrap_or_else(client)
}

/// YouTube 的媒体 CDN。它的直链绑出口 IP、整段拉还会被限速（见 [`crate::ranged`]）。
pub(crate) fn is_googlevideo(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| h.ends_with(".googlevideo.com"))
}

/// 手动跟 3xx 的跳数上限。
const MAX_HOPS: usize = 5;

/// GET，并手动跟跳转。
///
/// 媒体客户端不自动跟跳转（见模块说明），但 googlevideo 的分轨和 HLS 分片会先 302 到
/// 另一台节点，不跟就拿不到内容。这里每一跳都过 SSRF 的地址检查，DNS 那一层照旧由
/// `SafeResolver` 管；同一家 CDN 内的跳转用同一个客户端（同一个出口）。
pub(crate) async fn send_following(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
) -> Result<reqwest::Response, String> {
    let mut url = url::Url::parse(url).map_err(|e| format!("地址无效: {e}"))?;
    for _ in 0..=MAX_HOPS {
        let resp = client
            .get(url.clone())
            .headers(headers.clone())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_redirection() {
            return Ok(resp);
        }
        let Some(location) = resp.headers().get("location").and_then(|v| v.to_str().ok()) else {
            return Ok(resp);
        };
        let next = url
            .join(location)
            .map_err(|e| format!("跳转地址无效: {e}"))?;
        if !alcedo::http::ssrf::url_allowed(&next) {
            return Err("跳转到了不允许的地址".into());
        }
        url = next;
    }
    Err(format!("跳转超过 {MAX_HOPS} 次"))
}

/// PATH 里有没有能用的 ffmpeg。
pub(crate) async fn has_ffmpeg() -> bool {
    tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

fn build(proxy: Option<&str>) -> Result<reqwest::Client, reqwest::Error> {
    alcedo::http::ensure_crypto_provider();
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(8)
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(std::sync::Arc::new(alcedo::http::ssrf::SafeResolver::new(
            true,
        )));
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p)?);
    }
    builder.build()
}

/// 直链请求头：固定桌面 UA（签名接口要求 UA 跨请求一致）+ 按 CDN 域名补
/// Referer，缺了就 403。
pub(crate) fn headers_for(url: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(ua::DESKTOP_FIXED) {
        h.insert("user-agent", v);
    }
    if let Some(referer) = alcedo::parsers::referer_for(url) {
        if let Ok(v) = HeaderValue::from_str(referer) {
            h.insert("referer", v);
        }
    }
    h
}

/// 扩展名怎么定：Content-Type 优先（下载时才知道，最可靠），URL 路径次之，
/// 都没有用 `fallback`。
pub(crate) fn pick_ext(content_type: Option<&str>, url: &str, fallback: &str) -> String {
    if let Some(ct) = content_type {
        let ct = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let ext = match ct.as_str() {
            "video/mp4" => "mp4",
            "video/webm" => "webm",
            "video/quicktime" => "mov",
            "audio/mp4" | "audio/x-m4a" => "m4a",
            "audio/mpeg" => "mp3",
            "audio/ogg" => "ogg",
            "audio/wav" | "audio/x-wav" => "wav",
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            "image/avif" => "avif",
            _ => "",
        };
        if !ext.is_empty() {
            return ext.to_owned();
        }
    }
    // URL 路径上的扩展名：段里必须真的带点，只认点后 2~5 位字母数字，
    // 别把哈希段或 "video" 这种目录名当扩展名
    if let Some(seg) = url::Url::parse(url).ok().and_then(|u| {
        u.path_segments()
            .and_then(|mut s| s.next_back().map(str::to_owned))
    }) {
        if let Some((_, ext)) = seg.rsplit_once('.') {
            if (2..=5).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return ext.to_ascii_lowercase();
            }
        }
    }
    fallback.to_owned()
}

/// 文件名清洗：危险字符换空格、压掉连续空白、掐到 60 个字符。
/// 与 Python 侧 `net.safe_filename` 的行为对齐——两边产物要能对得上。
pub(crate) fn safe_filename(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        })
        .collect();
    let base = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let base: String = base.trim_matches('.').chars().take(60).collect();
    let base = base.trim();
    if base.is_empty() {
        fallback.to_owned()
    } else {
        base.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_prefers_content_type_then_url_then_fallback() {
        assert_eq!(pick_ext(Some("video/mp4"), "https://x/a", "bin"), "mp4");
        assert_eq!(
            pick_ext(Some("image/jpeg; charset=binary"), "https://x/a", "bin"),
            "jpg"
        );
        assert_eq!(
            pick_ext(
                Some("application/octet-stream"),
                "https://x/a.MP4?e=1",
                "bin"
            ),
            "mp4"
        );
        assert_eq!(
            pick_ext(
                Some("application/octet-stream"),
                "https://x/playlist.m3u8",
                "bin"
            ),
            "m3u8"
        );
        assert_eq!(pick_ext(None, "https://x/a", "mp4"), "mp4");
        // 哈希段不是扩展名
        assert_eq!(
            pick_ext(None, "https://x/82b540c91294c68c/2f3a4b5c/video", "mp4"),
            "mp4"
        );
    }

    #[test]
    fn only_googlevideo_needs_the_parse_exit() {
        assert!(is_googlevideo(
            "https://rr5---sn-2oq4f5-cv.googlevideo.com/videoplayback?ip=1.2.3.4"
        ));
        assert!(is_googlevideo(
            "https://RR1---SN-X.GOOGLEVIDEO.COM/videoplayback"
        ));
        assert!(!is_googlevideo("https://googlevideo.com.evil.example/x"));
        assert!(!is_googlevideo("https://v11.365yg.com/a.mp4"));
        assert!(!is_googlevideo("not a url"));
    }

    #[test]
    fn safe_filename_cleans_and_caps() {
        assert_eq!(
            safe_filename("  a/b\\c:d*e?f\"g<h>i|j  ", "x"),
            "a b c d e f g h i j"
        );
        // 掐头去尾的只有 ASCII 点（Windows 不允许文件名以点结尾）；
        // 中文句号是合法字符，和 Python 侧行为保持一致
        assert_eq!(safe_filename("..标题..", "x"), "标题");
        assert_eq!(safe_filename("", "media"), "media");
        assert_eq!(safe_filename("   ", "media"), "media");
        // 60 个字符截断按字符数，不切半个中文
        let long = "抖".repeat(100);
        assert_eq!(safe_filename(&long, "x").chars().count(), 60);
    }
}
