//! Reddit。
//!
//! 帖子地址后面加 `.json` 就是完整数据，公开、无需登录。视频在 `secure_media`
//! 里，音视频是分开的两条 DASH 流——Reddit 自己的播放器也是现合的。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, Image, VideoInfo};
use crate::util;

/// 解析一条Reddit分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // v.redd.it/<id> 是直链短链，先跟到帖子页
    let url = if util::host_of(url).is_some_and(|h| h.ends_with("redd.it")) {
        http.resolve_redirect(url)
            .await
            .unwrap_or_else(|_| url.to_owned())
    } else {
        url.to_owned()
    };

    let json_url = to_json_url(&url)?;
    let resp = http
        .send(
            Req::get(&json_url)
                // 默认 UA 会被 Reddit 直接 429
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .header("Accept", "application/json"),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    // 返回的是 [帖子listing, 评论listing]
    let post = util::get(&json, &["0", "data", "children", "0", "data"])
        .ok_or_else(|| Error::deleted("Reddit 没有返回帖子数据"))?;

    let info = build(post);
    if info.is_empty() {
        return Err(Error::new(
            crate::Reason::Empty,
            "这个帖子里没有可下载的视频或图片（可能是纯文字或外链）",
        ));
    }
    Ok(info)
}

fn build(post: &Value) -> VideoInfo {
    let media = util::get(post, &["secure_media", "reddit_video"])
        .or_else(|| util::get(post, &["media", "reddit_video"]))
        // 转贴的视频挂在 crosspost_parent_list 上
        .or_else(|| {
            util::get(
                post,
                &["crosspost_parent_list", "0", "secure_media", "reddit_video"],
            )
        })
        .cloned()
        .unwrap_or_default();

    let mut video_url = util::str_at(&media, &["fallback_url"]);
    // fallback_url 带 ?source=fallback，去掉更干净
    if let Some(i) = video_url.find('?') {
        video_url.truncate(i);
    }

    let mut formats = Vec::new();
    if !video_url.is_empty() {
        // Reddit 的 fallback_url 是纯视频轨，音频要单独取
        let audio_url = audio_track(&video_url);
        if !audio_url.is_empty() {
            formats.push(Format {
                label: format!("{}p（含音轨）", util::u32_at(&media, &["height"])),
                url: String::new(),
                ext: "mp4".into(),
                height: util::u32_at(&media, &["height"]),
                filesize: 0,
                codec: String::new(),
                video_url: video_url.clone(),
                audio_url,
            });
        }
    }

    // 图集
    let mut images = Vec::new();
    if video_url.is_empty() {
        if let Some(items) = util::get(post, &["gallery_data", "items"]).and_then(Value::as_array) {
            let meta = util::get(post, &["media_metadata"]);
            for it in items {
                let mid = util::id_at(it, &["media_id"]);
                let u = meta
                    .and_then(|m| util::get(m, &[&mid, "s", "u"]))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !u.is_empty() {
                    images.push(Image::new(crate::parsers::meta::decode_entities(u)));
                }
            }
        }
        // 单图帖
        if images.is_empty() {
            let u = util::str_at(post, &["url_overridden_by_dest"]);
            let path = u.split('?').next().unwrap_or("").to_ascii_lowercase();
            if [".jpg", ".jpeg", ".png", ".gif", ".webp"]
                .iter()
                .any(|ext| path.ends_with(ext))
            {
                images.push(Image::new(u));
            }
        }
    }

    VideoInfo {
        video_url,
        cover_url: crate::parsers::meta::decode_entities(&util::first_str(
            post,
            &[&["preview", "images", "0", "source", "url"], &["thumbnail"]],
        )),
        title: util::str_at(post, &["title"]),
        images,
        duration: util::num_at(&media, &["duration"]),
        width: util::u32_at(&media, &["width"]),
        height: util::u32_at(&media, &["height"]),
        formats,
        author: Author::named(util::str_at(post, &["author"])),
        ..Default::default()
    }
}

/// 从视频轨地址推出音频轨地址。
///
/// Reddit 的 DASH 目录里音频固定叫 `DASH_AUDIO_128.mp4`（老帖子是 `DASH_audio.mp4`），
/// 和视频轨同目录。拿不准就返回空，上层照样能下无声视频。
fn audio_track(video_url: &str) -> String {
    let Some(dir) = video_url.rfind('/') else {
        return String::new();
    };
    format!("{}/DASH_AUDIO_128.mp4", &video_url[..dir])
}

fn to_json_url(url: &str) -> Result<String> {
    let mut parsed = url::Url::parse(url)?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    let path = parsed.path().trim_end_matches('/').to_owned();
    if path.is_empty() || !path.contains("/comments/") {
        return Err(Error::unsupported("不是 Reddit 的帖子链接"));
    }
    parsed.set_path(&format!("{path}.json"));
    // 用 old. 域名：新版页面对机房 IP 更容易触发验证
    let _ = parsed.set_host(Some("www.reddit.com"));
    Ok(parsed.to_string())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_json_url() {
        assert_eq!(
            to_json_url("https://www.reddit.com/r/videos/comments/abc123/some_title/?utm=1")
                .unwrap(),
            "https://www.reddit.com/r/videos/comments/abc123/some_title.json"
        );
        assert!(to_json_url("https://www.reddit.com/r/videos/").is_err());
    }

    #[test]
    fn audio_track_is_sibling_of_video() {
        assert_eq!(
            audio_track("https://v.redd.it/abc123/DASH_720.mp4"),
            "https://v.redd.it/abc123/DASH_AUDIO_128.mp4"
        );
        assert_eq!(audio_track("noslash"), "");
    }

    #[test]
    fn builds_video_post() {
        let post = json!({
            "title": "帖子标题",
            "author": "someone",
            "secure_media": {"reddit_video": {
                "fallback_url": "https://v.redd.it/abc/DASH_1080.mp4?source=fallback",
                "width": 1920, "height": 1080, "duration": 45
            }},
            "preview": {"images": [{"source": {"url": "https://preview.redd.it/x.jpg?width=1&amp;h=2"}}]}
        });
        let info = build(&post);
        assert_eq!(
            info.video_url, "https://v.redd.it/abc/DASH_1080.mp4",
            "要去掉 ?source=fallback"
        );
        assert_eq!(info.duration, 45.0);
        assert!(info.cover_url.contains("&h=2"), "封面地址里的实体要还原");
        assert_eq!(info.formats.len(), 1);
        assert!(info.formats[0].needs_merge(), "Reddit 的视频轨不含声音");
    }

    #[test]
    fn builds_gallery_post() {
        let post = json!({
            "title": "多图",
            "author": "u",
            "gallery_data": {"items": [{"media_id": "aaa"}, {"media_id": "bbb"}]},
            "media_metadata": {
                "aaa": {"s": {"u": "https://preview.redd.it/aaa.jpg?a=1&amp;b=2"}},
                "bbb": {"s": {"u": "https://preview.redd.it/bbb.jpg"}}
            }
        });
        let info = build(&post);
        assert_eq!(info.images.len(), 2);
        assert!(info.images[0].url.contains("&b=2"));
    }

    #[test]
    fn crosspost_video_is_found() {
        let post = json!({
            "title": "转贴",
            "author": "u",
            "crosspost_parent_list": [{"secure_media": {"reddit_video": {
                "fallback_url": "https://v.redd.it/xyz/DASH_720.mp4", "height": 720
            }}}]
        });
        assert_eq!(build(&post).video_url, "https://v.redd.it/xyz/DASH_720.mp4");
    }

    #[test]
    fn text_post_is_empty() {
        assert!(build(&json!({"title": "纯文字", "author": "u"})).is_empty());
    }
}
