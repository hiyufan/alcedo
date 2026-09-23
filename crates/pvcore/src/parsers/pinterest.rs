//! Pinterest。
//!
//! `resource/PinResource/get` 是网页版自己调的接口，公开可读，视频和原图都在里面。
//! 拿不到就退到 OG 标签。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::parsers::meta;
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // pin.it 是短链
    let url = if util::host_of(url).is_some_and(|h| h == "pin.it") {
        http.resolve_redirect(url)
            .await
            .unwrap_or_else(|_| url.to_owned())
    } else {
        url.to_owned()
    };

    if let Some(id) = pin_id_from_url(&url) {
        if let Ok(info) = resource_api(http, &id).await {
            return Ok(info);
        }
    }

    // 兜底：分享卡片
    let resp = http
        .send(Req::get(&url).header("User-Agent", ua::pick(ua::Platform::Desktop)))
        .await?;
    resp.error_for_status()?;
    let html = resp.text();
    meta::from_og(&html).ok_or_else(|| Error::restricted("Pinterest 没有对外给出这个 Pin 的内容"))
}

async fn resource_api(http: &Http, pin_id: &str) -> Result<VideoInfo> {
    let data = format!(
        r#"{{"options":{{"id":"{pin_id}","field_set_key":"unauth_react"}},"context":{{}}}}"#
    );
    let encoded = percent_encoding::utf8_percent_encode(&data, percent_encoding::NON_ALPHANUMERIC)
        .to_string();
    let api = format!(
        "https://www.pinterest.com/resource/PinResource/get/?source_url=%2F&data={encoded}"
    );

    let resp = http
        .send(
            Req::get(&api)
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .header("Accept", "application/json")
                .header("X-Requested-With", "XMLHttpRequest")
                .referer("https://www.pinterest.com/"),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let pin = util::get(&json, &["resource_response", "data"])
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::deleted("Pinterest 没有返回这个 Pin"))?;

    let info = build(pin);
    if info.is_empty() {
        return Err(Error::bare(crate::Reason::Empty));
    }
    Ok(info)
}

fn build(pin: &Value) -> VideoInfo {
    // 视频：video_list 里挑分辨率最高的
    let video_url = ["videos", "story_pin_data"]
        .iter()
        .find_map(|root| {
            let list = util::get(pin, &[root, "video_list"])?.as_object()?;
            list.values()
                .max_by_key(|v| util::u64_at(v, &["width"]) * util::u64_at(v, &["height"]))
                .map(|v| util::str_at(v, &["url"]))
        })
        .unwrap_or_default();

    let cover = util::first_str(
        pin,
        &[
            &["images", "orig", "url"],
            &["images", "736x", "url"],
            &["image_large_url"],
        ],
    );

    let images = if video_url.is_empty() && !cover.is_empty() {
        vec![Image::new(cover.clone())]
    } else {
        Vec::new()
    };

    VideoInfo {
        video_url,
        cover_url: cover,
        title: util::first_str(pin, &[&["title"], &["grid_title"], &["description"]]),
        images,
        author: Author::new(
            util::id_at(pin, &["pinner", "id"]),
            util::first_str(pin, &[&["pinner", "full_name"], &["pinner", "username"]]),
            util::str_at(pin, &["pinner", "image_medium_url"]),
        ),
        ..Default::default()
    }
}

fn pin_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let i = segs.iter().position(|s| *s == "pin")?;
    segs.get(i + 1)
        .map(|s| s.to_string())
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()) && !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_pin_id() {
        assert_eq!(
            pin_id_from_url("https://www.pinterest.com/pin/1234567890123456789/").as_deref(),
            Some("1234567890123456789")
        );
        assert_eq!(pin_id_from_url("https://www.pinterest.com/someuser/"), None);
    }

    #[test]
    fn picks_largest_video() {
        let pin = json!({
            "title": "标题",
            "videos": {"video_list": {
                "V_720P": {"url": "https://v/720.mp4", "width": 720, "height": 1280},
                "V_HLSV4": {"url": "https://v/playlist.m3u8", "width": 480, "height": 854}
            }},
            "images": {"orig": {"url": "https://i/orig.jpg"}},
            "pinner": {"id": 42, "username": "someone"}
        });
        let info = build(&pin);
        assert_eq!(info.video_url, "https://v/720.mp4");
        assert_eq!(info.author.uid, "42");
        assert!(info.images.is_empty());
    }

    #[test]
    fn image_pin_becomes_gallery() {
        let pin = json!({
            "grid_title": "图",
            "images": {"orig": {"url": "https://i/orig.jpg"}},
            "pinner": {"username": "u"}
        });
        let info = build(&pin);
        assert!(info.is_gallery());
        assert_eq!(info.images[0].url, "https://i/orig.jpg");
    }
}
