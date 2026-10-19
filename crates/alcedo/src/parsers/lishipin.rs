//! 梨视频。
//!
//! `videoStatus.jsp` 给的 `srcUrl` 里带着一串时间戳，必须替换成 `cont-<id>` 才能播。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::VideoInfo;
use crate::util;

/// 解析一条梨视频分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let id = cont_id(url).ok_or_else(|| Error::unsupported("链接里没有梨视频的作品 ID"))?;
    parse_id(http, id).await
}

/// 作品 ID：分享出来是 `/detail_<id>`，首页和列表里是 `/video_<id>`，接口地址里是 `contId=<id>`。
fn cont_id(url: &str) -> Option<&str> {
    ["/detail_", "/video_", "contId="]
        .iter()
        .find_map(|marker| {
            let rest = &url[url.find(marker)? + marker.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            (end > 0).then(|| &rest[..end])
        })
}

/// 已知梨视频作品 ID 时直接解析。
pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    // mrd 是个随机数，梨视频拿它防缓存；用时间戳即可
    let mrd = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let detail_page = format!("https://www.pearvideo.com/detail_{id}");
    let api = format!("https://www.pearvideo.com/videoStatus.jsp?contId={id}&mrd={mrd}");

    // videoStatus 只给地址和封面，标题只在详情页上。两个请求互不依赖，
    // 并发发出去，多拿一个标题不多花时间。
    let (status, page) = tokio::join!(
        http.send(
            Req::get(&api)
                .referer(&detail_page)
                .header("User-Agent", ua::pick(ua::Platform::Desktop)),
        ),
        // 只要 <head> 里的标题，详情页有几百 KB，下完整页纯属白等
        http.send(
            Req::get(&detail_page)
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .head_bytes(24 * 1024),
        ),
    );

    let title = page
        .ok()
        .filter(|r| r.status.is_success())
        .map(|r| title_from_page(&r.text()))
        .unwrap_or_default();

    let resp = status?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let src = util::str_at(&json, &["videoInfo", "videos", "srcUrl"]);
    if src.is_empty() {
        return Err(Error::restricted(format!(
            "梨视频没有下发播放地址：{}",
            util::first_str(&json, &[&["msg"], &["resultDesc"]])
        )));
    }

    // srcUrl 形如 .../1.8-15-25/cont-1234-<systemTime>.mp4，
    // 把那段 systemTime 换成 cont-<id> 才是能直接播的地址
    let timer = util::str_at(&json, &["systemTime"]);
    let video_url = if timer.is_empty() {
        src
    } else {
        src.replace(&timer, &format!("cont-{id}"))
    };

    Ok(VideoInfo {
        video_url,
        cover_url: util::str_at(&json, &["videoInfo", "video_image"]),
        title,
        duration: util::num_at(&json, &["videoInfo", "videos", "duration"]),
        ..Default::default()
    })
}

/// 详情页里的标题。优先 `og:title`，退到 `<h1 class="video-tt">`。
fn title_from_page(html: &str) -> String {
    let og = crate::parsers::meta::meta_content(html, "og:title");
    if !og.is_empty() {
        return og;
    }
    let Some(at) = html.find("class=\"video-tt\"") else {
        return util::html_title(html);
    };
    let Some(start) = html[at..].find('>').map(|i| at + i + 1) else {
        return String::new();
    };
    html[start..]
        .find('<')
        .map(|i| html[start..start + i].trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn cont_id_from_every_url_shape() {
        assert_eq!(
            cont_id("https://www.pearvideo.com/detail_1742158"),
            Some("1742158")
        );
        // 首页 / 列表里的地址，以前把 "video_1807774" 整个当成 ID
        assert_eq!(
            cont_id("https://www.pearvideo.com/video_1807774"),
            Some("1807774")
        );
        assert_eq!(
            cont_id("https://www.pearvideo.com/videoStatus.jsp?contId=1742158&mrd=1"),
            Some("1742158")
        );
        assert_eq!(cont_id("https://www.pearvideo.com/category_1"), None);
        assert_eq!(cont_id("https://www.pearvideo.com/detail_"), None);
    }

    #[test]
    fn title_comes_from_og_then_h1() {
        let og = r#"<meta property="og:title" content="正式标题">"#;
        assert_eq!(title_from_page(og), "正式标题");

        let h1 = r#"<h1 class="video-tt">页面标题</h1>"#;
        assert_eq!(title_from_page(h1), "页面标题");

        assert_eq!(title_from_page("<html></html>"), "");
    }

    #[test]
    fn url_rewrite_replaces_timestamp() {
        let src = "https://video.pearvideo.com/mp4/short/20210101/1609459200000-15588898-hd.mp4";
        let timer = "1609459200000";
        let got = src.replace(timer, "cont-1234567");
        assert!(got.contains("cont-1234567-15588898-hd.mp4"));
        assert!(!got.contains(timer));
    }

    #[tokio::test]
    async fn rejects_non_detail_urls() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::LiShiPin),
        )
        .unwrap();
        let err = parse(&http, "https://www.pearvideo.com/category_1")
            .await
            .unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
    }
}
