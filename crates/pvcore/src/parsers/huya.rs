//! 虎牙。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

/// 解析一条虎牙分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // https://v.huya.com/play/123456.html
    let id = url
        .rsplit('/')
        .next()
        .and_then(|s| s.strip_suffix(".html"))
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()) && !s.is_empty())
        .ok_or_else(|| Error::unsupported("链接里没有虎牙的视频 ID"))?;
    parse_id(http, id).await
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

    #[tokio::test]
    async fn video_id_from_share_url() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::HuYa),
        )
        .unwrap();
        // 只验证取 ID 这一步的分支, 不发请求
        assert!(parse(&http, "https://v.huya.com/play/notanid")
            .await
            .is_err());
    }
}
