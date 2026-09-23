//! Facebook。
//!
//! Facebook 没有可用的公开接口，页面本身也是登录墙 + 高度混淆。能稳定拿到东西的
//! 只有两条路：
//!
//! 1. 用爬虫 UA 请求 `mbasic` / `m.` 站点，页面里会直接出现 `playable_url`
//! 2. 分享卡片的 OG 标签
//!
//! 两条都拿不到就老实报"需要登录"，别假装还有别的办法。

use crate::error::{Error, Result};
use crate::http::{Http, Req};
use crate::model::VideoInfo;
use crate::parsers::meta;
use crate::util;

/// 用这个 UA 时 Facebook 会返回给爬虫看的那份页面，OG 标签齐全。
const CRAWLER_UA: &str =
    "Mozilla/5.0 (compatible; facebookexternalhit/1.1; +http://www.facebook.com/externalhit_uatext.php)";

/// 解析一条Facebook分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // fb.watch / fb.com 都是短链
    let host = util::host_of(url).unwrap_or_default();
    let url = if host == "fb.watch" || host.ends_with("fb.com") {
        http.resolve_redirect(url)
            .await
            .unwrap_or_else(|_| url.to_owned())
    } else {
        url.to_owned()
    };

    let resp = http
        .send(
            Req::get(&url)
                .header("User-Agent", CRAWLER_UA)
                .header("Accept", "text/html,*/*")
                .header("Accept-Language", "en-US,en;q=0.9"),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    // 页面里的 playable_url 比 OG 的更高清
    if let Some(u) = playable_url(&html) {
        let mut info = meta::from_og(&html).unwrap_or_default();
        info.video_url = u;
        info.images.clear();
        if info.title.is_empty() {
            info.title = util::html_title(&html);
        }
        return Ok(info);
    }

    if let Some(info) = meta::from_og(&html) {
        return Ok(info);
    }

    Err(if html.contains("login") || html.contains("Log in") {
        Error::login("Facebook 要求这个出口登录才能看这条内容")
    } else {
        Error::restricted("Facebook 没有对外给出这条内容")
    })
}

/// 页面 JS 里嵌的播放地址，优先高清那个。
///
/// 这些字段藏在一大坨转义过的 JSON 里，所以是按字面量搜，而不是解析结构。
fn playable_url(html: &str) -> Option<String> {
    for key in ["playable_url_quality_hd", "playable_url_hd", "playable_url"] {
        for pattern in [format!("\"{key}\":\""), format!("{key}\\\":\\\"")] {
            if let Some(at) = html.find(&pattern) {
                let start = at + pattern.len();
                let rest = &html[start..];
                // 值到下一个未转义的引号为止
                let end = rest.find("\",").or_else(|| rest.find('"'))?;
                let raw = &rest[..end];
                let cleaned = unescape_json_url(raw);
                if cleaned.starts_with("http") {
                    return Some(cleaned);
                }
            }
        }
    }
    None
}

/// 这些地址被转义了两轮：`\/` 要还原成 `/`，`%` 之类要还原成字符。
fn unescape_json_url(raw: &str) -> String {
    let s = raw
        .replace("\\\\", "\\")
        .replace("\\/", "/")
        .replace("\\u0025", "%");
    meta::decode_entities(&s)
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn prefers_hd_playable_url() {
        let html = r#"{"playable_url":"https:\/\/video.fb\/sd.mp4","playable_url_quality_hd":"https:\/\/video.fb\/hd.mp4"}"#;
        assert_eq!(playable_url(html).unwrap(), "https://video.fb/hd.mp4");
    }

    #[test]
    fn falls_back_to_sd() {
        let html = r#"{"playable_url":"https:\/\/video.fb\/sd.mp4"}"#;
        assert_eq!(playable_url(html).unwrap(), "https://video.fb/sd.mp4");
    }

    #[test]
    fn no_playable_url_returns_none() {
        assert!(playable_url("<html>nothing here</html>").is_none());
        // 值不是 http 开头的要当成没找到
        assert!(playable_url(r#"{"playable_url":"null"}"#).is_none());
    }

    #[test]
    fn unescapes_double_escaped_urls() {
        assert_eq!(
            unescape_json_url(r"https:\/\/video.fb\/a.mp4?x=1%"),
            "https://video.fb/a.mp4?x=1%"
        );
    }
}
