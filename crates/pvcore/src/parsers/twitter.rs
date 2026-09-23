//! X (Twitter)。
//!
//! 主路径是 syndication 接口（给站外嵌入用的，不需要登录）；它对敏感内容和
//! 部分墓碑推文不给数据，这时候退到 fxtwitter 的公开接口再试一次。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::util;

const WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";

/// 解析一条X（Twitter）分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // t.co 短链先跟一跳
    let url = if url.contains("t.co/") {
        http.resolve_redirect(url)
            .await
            .unwrap_or_else(|_| url.to_owned())
    } else {
        url.to_owned()
    };

    let id = tweet_id_from_url(&url)
        .ok_or_else(|| Error::unsupported(format!("无法从链接里取出推文 ID: {url}")))?;
    parse_id(http, &id).await
}

/// 已知X（Twitter）作品 ID 时直接解析。
pub async fn parse_id(http: &Http, tweet_id: &str) -> Result<VideoInfo> {
    if !tweet_id.bytes().all(|b| b.is_ascii_digit()) || tweet_id.is_empty() {
        return Err(Error::unsupported("推文 ID 应当是一串数字"));
    }

    let syndication = fetch_syndication(http, tweet_id).await;

    match &syndication {
        Ok(json) if has_media(json) => return Ok(build_syndication(json)),
        _ => {}
    }

    // syndication 拿不到就试 fxtwitter，它对敏感内容宽容些
    if let Some(info) = fetch_fxtwitter(http, tweet_id).await {
        return Ok(info);
    }

    match syndication {
        Ok(json) if util::str_at(&json, &["__typename"]) == "TweetTombstone" => {
            Err(Error::deleted("这条推文不存在、已删除或需要登录才能看"))
        }
        Ok(json) if util::get(&json, &["user"]).is_some() => {
            // 推文在，只是没有任何媒体
            Err(Error::new(crate::Reason::Empty, "这条推文里没有视频或图片"))
        }
        Ok(_) => Err(Error::deleted("这条推文不存在、已删除或需要登录才能看")),
        Err(e) => Err(e),
    }
}

async fn fetch_syndication(http: &Http, tweet_id: &str) -> Result<Value> {
    let token = syndication_token(tweet_id);
    let api = format!("https://cdn.syndication.twimg.com/tweet-result?id={tweet_id}&token={token}");
    let resp = http
        .send(
            Req::get(&api)
                .header("User-Agent", WEB_UA)
                .header("Accept", "application/json")
                .referer("https://platform.twitter.com/"),
        )
        .await?;
    if !resp.status.is_success() {
        return Err(crate::error::from_status(resp.status.as_u16()));
    }
    resp.json()
}

fn has_media(json: &Value) -> bool {
    !util::arr_at(json, &["mediaDetails"]).is_empty() || util::get(json, &["video"]).is_some()
}

fn build_syndication(json: &Value) -> VideoInfo {
    let user = util::get(json, &["user"]).cloned().unwrap_or_default();

    let media = util::arr_at(json, &["mediaDetails"]);
    let mut video_url = String::new();
    let mut cover_url = String::new();
    let mut duration = 0.0;
    let mut width = 0;
    let mut height = 0;

    // 只取第一条视频
    for m in media {
        let ty = util::str_at(m, &["type"]);
        if ty != "video" && ty != "animated_gif" {
            continue;
        }
        cover_url = util::str_at(m, &["media_url_https"]);
        video_url = best_mp4(util::arr_at(m, &["video_info", "variants"]));
        duration = util::num_at(m, &["video_info", "duration_millis"]) / 1000.0;
        width = util::u32_at(m, &["original_info", "width"]);
        height = util::u32_at(m, &["original_info", "height"]);
        break;
    }

    // 顶层 video 字段（部分响应只有这个）
    if video_url.is_empty() {
        if let Some(v) = util::get(json, &["video"]) {
            video_url = best_mp4(util::arr_at(v, &["variants"]));
            cover_url = util::str_at(v, &["poster"]);
        }
    }

    // 没有视频才收图
    let mut images = Vec::new();
    if video_url.is_empty() {
        for m in media {
            if util::str_at(m, &["type"]) == "photo" {
                let u = util::str_at(m, &["media_url_https"]);
                if !u.is_empty() {
                    // ?name=orig 才是原图，页面给的是压过的
                    images.push(Image::new(format!("{u}?format=jpg&name=orig")));
                }
            }
        }
        if let Some(first) = images.first() {
            cover_url = first.url.clone();
        }
    }

    let name = {
        let n = util::str_at(&user, &["name"]);
        if n.is_empty() {
            util::str_at(&user, &["screen_name"])
        } else {
            n
        }
    };

    VideoInfo {
        video_url,
        cover_url,
        title: util::str_at(json, &["text"]),
        images,
        duration,
        width,
        height,
        author: Author::new(
            util::first_id(&user, &[&["id_str"], &["id"]]),
            name,
            util::str_at(&user, &["profile_image_url_https"]),
        ),
        ..Default::default()
    }
}

/// variants 里码率最高的 mp4。
fn best_mp4(variants: &[Value]) -> String {
    variants
        .iter()
        .filter(|v| util::str_at(v, &["content_type"]) == "video/mp4")
        .filter(|v| !util::str_at(v, &["url"]).is_empty())
        .max_by_key(|v| util::u64_at(v, &["bitrate"]))
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default()
}

async fn fetch_fxtwitter(http: &Http, tweet_id: &str) -> Option<VideoInfo> {
    let resp = http
        .send(
            Req::get(format!("https://api.fxtwitter.com/i/status/{tweet_id}"))
                .header("User-Agent", WEB_UA),
        )
        .await
        .ok()?;
    if !resp.status.is_success() {
        return None;
    }
    let json = resp.json().ok()?;
    let tweet = util::get(&json, &["tweet"])?;

    let mut video_url = String::new();
    let mut cover_url = String::new();
    let mut duration = 0.0;
    let (mut width, mut height) = (0, 0);
    let mut images = Vec::new();

    for m in util::arr_at(tweet, &["media", "all"]) {
        let ty = util::str_at(m, &["type"]);
        if (ty == "video" || ty == "gif") && video_url.is_empty() {
            video_url = {
                let best = best_mp4(util::arr_at(m, &["variants"]));
                if best.is_empty() {
                    util::str_at(m, &["url"])
                } else {
                    best
                }
            };
            cover_url = util::str_at(m, &["thumbnail_url"]);
            duration = util::num_at(m, &["duration"]);
            width = util::u32_at(m, &["width"]);
            height = util::u32_at(m, &["height"]);
        } else if ty == "photo" {
            let u = util::str_at(m, &["url"]);
            if !u.is_empty() {
                images.push(Image::new(u));
            }
        }
    }

    if !video_url.is_empty() {
        images.clear();
    } else if let Some(first) = images.first() {
        cover_url = first.url.clone();
    }
    if video_url.is_empty() && images.is_empty() {
        return None;
    }

    let author = util::get(tweet, &["author"]).cloned().unwrap_or_default();
    Some(VideoInfo {
        video_url,
        cover_url,
        title: util::str_at(tweet, &["text"]),
        images,
        duration,
        width,
        height,
        author: Author::new(
            util::first_id(&author, &[&["id"]]),
            util::first_str(&author, &[&["name"], &["screen_name"]]),
            util::str_at(&author, &["avatar_url"]),
        ),
        ..Default::default()
    })
}

/// syndication 接口要的 token。
///
/// 算法是 X 自己前端里的：`(id / 1e15 * π)` 转成字符串，去掉所有 `0` 和 `.`。
/// 没有密钥，纯粹是个防直接爬的障眼法。
fn syndication_token(tweet_id: &str) -> String {
    let n: f64 = tweet_id.parse().unwrap_or(0.0);
    let v = (n / 1e15) * std::f64::consts::PI;
    let s = format!("{v}");
    s.chars().filter(|c| *c != '0' && *c != '.').collect()
}

fn tweet_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    // /<user>/status/<id> 或 /<user>/statuses/<id>
    let i = segs
        .iter()
        .position(|s| *s == "status" || *s == "statuses")?;
    let id = segs.get(i + 1)?;
    let id = id.split(['?', '#']).next().unwrap_or(id);
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_tweet_id() {
        for url in [
            "https://x.com/someone/status/1234567890123456789",
            "https://twitter.com/someone/status/1234567890123456789?s=20",
            "https://mobile.twitter.com/someone/statuses/1234567890123456789",
        ] {
            assert_eq!(
                tweet_id_from_url(url).as_deref(),
                Some("1234567890123456789"),
                "{url}"
            );
        }
        assert_eq!(tweet_id_from_url("https://x.com/someone"), None);
    }

    #[test]
    fn token_has_no_zeros_or_dots() {
        let t = syndication_token("1234567890123456789");
        assert!(!t.contains('0') && !t.contains('.'), "token = {t}");
        assert!(!t.is_empty());
    }

    #[test]
    fn picks_highest_bitrate_mp4() {
        let variants = vec![
            json!({"content_type": "video/mp4", "bitrate": 832000, "url": "https://v/mid.mp4"}),
            json!({"content_type": "application/x-mpegURL", "url": "https://v/playlist.m3u8"}),
            json!({"content_type": "video/mp4", "bitrate": 2176000, "url": "https://v/high.mp4"}),
        ];
        assert_eq!(best_mp4(&variants), "https://v/high.mp4");
        assert_eq!(best_mp4(&[]), "");
    }

    #[test]
    fn builds_video_tweet() {
        let json = json!({
            "text": "看这个",
            "user": {"id_str": "99", "name": "某人", "screen_name": "someone",
                     "profile_image_url_https": "https://a/av.jpg"},
            "mediaDetails": [{
                "type": "video",
                "media_url_https": "https://pbs.twimg.com/cover.jpg",
                "original_info": {"width": 1280, "height": 720},
                "video_info": {
                    "duration_millis": 30000,
                    "variants": [
                        {"content_type": "video/mp4", "bitrate": 256000, "url": "https://v/low.mp4"},
                        {"content_type": "video/mp4", "bitrate": 2176000, "url": "https://v/high.mp4"}
                    ]
                }
            }]
        });
        let info = build_syndication(&json);
        assert_eq!(info.video_url, "https://v/high.mp4");
        assert_eq!(info.duration, 30.0);
        assert_eq!(info.width, 1280);
        assert_eq!(info.author.name, "某人");
    }

    #[test]
    fn photos_use_original_size() {
        let json = json!({
            "text": "图",
            "user": {"screen_name": "someone"},
            "mediaDetails": [
                {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/A.jpg"},
                {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/B.jpg"}
            ]
        });
        let info = build_syndication(&json);
        assert_eq!(info.images.len(), 2);
        assert!(info.images[0].url.ends_with("name=orig"), "要取原图尺寸");
        assert!(info.video_url.is_empty());
        // 没有 name 时用 screen_name 兜底
        assert_eq!(info.author.name, "someone");
    }

    #[test]
    fn media_presence_check() {
        assert!(has_media(&json!({"mediaDetails": [{"type": "photo"}]})));
        assert!(has_media(&json!({"video": {"variants": []}})));
        assert!(!has_media(&json!({"text": "纯文字"})));
    }
}
