//! 虎牙。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

/// `v.huya.com/play/123456.html` → `123456`。
///
/// App 分享出来的链接常带查询串（`?shareFrom=x`），先剥掉再取末段，
/// 否则 strip_suffix 认不出 `.html`，整条链接直接解析失败。
fn video_id(url: &str) -> Result<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/')
        .next()
        .and_then(|s| s.strip_suffix(".html"))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
        .ok_or_else(|| Error::unsupported("链接里没有虎牙的视频 ID"))
}

/// 解析一条虎牙分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    parse_id(http, &video_id(url)?).await
}

/// 已知虎牙作品 ID 时直接解析。
pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let api = format!("https://liveapi.huya.com/moment/getMomentContent?videoId={id}");
    let resp = http
        .send(
            Req::get(&api)
                .referer("https://v.huya.com/")
                .header("User-Agent", ua::pick(ua::Platform::Desktop)),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let video = util::get(&json, &["data", "moment", "videoInfo"])
        .ok_or_else(|| Error::deleted("虎牙没有返回视频信息"))?;

    if util::num_at(video, &["uid"]) == 0.0 {
        return Err(Error::deleted("视频不存在或已下架"));
    }

    // definitions 是按清晰度降序给的，第一条最清晰
    let defs = util::arr_at(video, &["definitions"]);
    let first = defs
        .first()
        .ok_or_else(|| Error::parse("没有可播放的清晰度"))?;

    let formats = defs
        .iter()
        .skip(1)
        .filter_map(|d| {
            let u = util::str_at(d, &["url"]);
            (!u.is_empty()).then(|| Format {
                label: util::first_str(d, &[&["defName"], &["definition"]]),
                url: u,
                ext: "mp4".into(),
                height: util::u32_at(d, &["height"]),
                filesize: util::u64_at(d, &["size"]),
                ..Default::default()
            })
        })
        .collect();

    Ok(VideoInfo {
        video_url: util::str_at(first, &["url"]),
        cover_url: util::str_at(video, &["videoCover"]),
        title: util::str_at(video, &["videoTitle"]),
        duration: util::num_at(video, &["videoDuration"]),
        width: util::u32_at(first, &["width"]),
        height: util::u32_at(first, &["height"]),
        formats,
        author: Author::new(
            util::i64_at(video, &["uid"]).to_string(),
            util::str_at(video, &["actorNick"]),
            util::str_at(video, &["actorAvatarUrl"]),
        ),
        ..Default::default()
    })
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn video_id_survives_query_strings() {
        // App 分享出来的链接带查询串，不能让 strip_suffix 认不出 .html
        assert_eq!(
            video_id("https://v.huya.com/play/123456.html?shareFrom=x").unwrap(),
            "123456"
        );
        assert_eq!(
            video_id("https://v.huya.com/play/123456.html").unwrap(),
            "123456"
        );
        assert_eq!(
            video_id("https://v.huya.com/play/123456.html#comment").unwrap(),
            "123456"
        );
        assert!(video_id("https://v.huya.com/play/notanid").is_err());
        assert!(video_id("https://v.huya.com/").is_err());
    }
}
