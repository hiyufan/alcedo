//! Dailymotion。
//!
//! `player/metadata/video/<xid>` 是播放器自己调的接口，公开可读。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let xid =
        xid_from_url(url).ok_or_else(|| Error::unsupported("链接里没有 Dailymotion 的视频 ID"))?;
    parse_id(http, &xid).await
}

pub async fn parse_id(http: &Http, xid: &str) -> Result<VideoInfo> {
    let api = format!("https://www.dailymotion.com/player/metadata/video/{xid}");
    let resp = http
        .send(
            Req::get(&api)
                .referer("https://www.dailymotion.com/")
                .header("User-Agent", ua::pick(ua::Platform::Desktop)),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    if let Some(err) = util::get(&json, &["error"]).filter(|v| !v.is_null()) {
        let code = util::num_at(err, &["code"]) as i64;
        let title = util::first_str(err, &[&["title"], &["raw_message"]]);
        return Err(match code {
            // DM_ERR_GEO_RESTRICTED / DM_ERR_MEDIA_NOT_FOUND 之类
            404 => Error::deleted(title),
            403 => Error::restricted(title),
            _ => Error::restricted(format!("Dailymotion 返回 {code}: {title}")),
        });
    }

    // qualities 是 {"720": [{type, url}], "auto": [...]}
    let qualities = util::get(&json, &["qualities"])
        .and_then(|q| q.as_object())
        .ok_or_else(|| Error::parse("接口没有返回 qualities"))?;

    let mut best_height = 0u32;
    let mut video_url = String::new();
    let mut auto_url = String::new();

    for (label, entries) in qualities {
        let url = entries
            .as_array()
            .and_then(|a| a.first())
            .map(|e| util::str_at(e, &["url"]))
            .unwrap_or_default();
        if url.is_empty() {
            continue;
        }
        if label == "auto" {
            auto_url = url;
            continue;
        }
        let h: u32 = label.trim_end_matches('p').parse().unwrap_or(0);
        if h >= best_height {
            best_height = h;
            video_url = url;
        }
    }
    if video_url.is_empty() {
        video_url = auto_url;
    }
    if video_url.is_empty() {
        return Err(Error::restricted("Dailymotion 没有下发可播放的地址"));
    }

    Ok(VideoInfo {
        video_url,
        cover_url: util::first_str(
            &json,
            &[
                &["posters", "720"],
                &["posters", "480"],
                &["thumbnails", "720"],
            ],
        ),
        title: util::str_at(&json, &["title"]),
        duration: util::num_at(&json, &["duration"]),
        height: best_height,
        author: Author::new(
            util::id_at(&json, &["owner", "id"]),
            util::first_str(&json, &[&["owner", "screenname"], &["owner", "username"]]),
            util::str_at(&json, &["owner", "avatar_190_url"]),
        ),
        ..Default::default()
    })
}

fn xid_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();

    // dai.ly/<xid>
    if parsed.host_str()?.ends_with("dai.ly") {
        return segs.first().map(|s| (*s).to_owned());
    }
    // /video/<xid> 或 /embed/video/<xid>，xid 后面可能跟标题 slug
    let i = segs.iter().rposition(|s| *s == "video")?;
    segs.get(i + 1)
        .map(|s| s.split('_').next().unwrap_or(s).to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_xid() {
        for url in [
            "https://www.dailymotion.com/video/x8abcde",
            "https://www.dailymotion.com/video/x8abcde_some-title-slug",
            "https://www.dailymotion.com/embed/video/x8abcde",
            "https://dai.ly/x8abcde",
        ] {
            assert_eq!(xid_from_url(url).as_deref(), Some("x8abcde"), "{url}");
        }
        assert_eq!(xid_from_url("https://www.dailymotion.com/someuser"), None);
    }
}
