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
    CLIENT.get_or_init(|| {
        alcedo::http::ensure_crypto_provider();
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(8)
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(std::sync::Arc::new(alcedo::http::ssrf::SafeResolver::new(
                true,
            )))
            .build()
            .expect("媒体客户端构建失败")
    })
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
