//! Instagram / Threads。
//!
//! 主路径：把分享链接里的 shortcode 还原成 media_id，调 `api/v1/media/{id}/info/`。
//! 这个接口对公开内容不要求登录，但认 `X-IG-App-ID`（网页版自己带的那个）。
//!
//! 机房 IP 常被这个接口挡，所以还有一条 OG 标签的兜底路：分享卡片的元数据是
//! 给爬虫看的，一直公开。兜底拿到的只有一条视频 / 一张图，没有多图和档位，
//! 但总比报"解析失败"强。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::parsers::meta;
use crate::util;

/// 网页版 Instagram 自己用的 app id，公开值。
const IG_APP_ID: &str = "936619743392459";

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let code = shortcode_from_url(url)
        .ok_or_else(|| Error::unsupported("链接里没有 Instagram 的作品 code"))?;

    if let Some(id) = media_id_from_shortcode(&code) {
        match api_info(http, &id).await {
            Ok(info) => return Ok(info),
            Err(e) if !e.reason.is_retryable() && e.reason != crate::Reason::Empty => {
                return Err(e)
            }
            Err(e) => tracing::debug!(error = %e, "IG 接口不可用，退到 OG 标签"),
        }
    }

    og_fallback(http, url, "Instagram").await
}

pub async fn parse_threads(http: &Http, url: &str) -> Result<VideoInfo> {
    // Threads 和 Instagram 同一套后端，post code 也是同一种编码
    if let Some(code) = shortcode_from_url(url) {
        if let Some(id) = media_id_from_shortcode(&code) {
            if let Ok(info) = api_info(http, &id).await {
                return Ok(info);
            }
        }
    }
    og_fallback(http, url, "Threads").await
}

async fn api_info(http: &Http, media_id: &str) -> Result<VideoInfo> {
    let api = format!("https://i.instagram.com/api/v1/media/{media_id}/info/");
    let req = Req::get(&api)
        .header("X-IG-App-ID", IG_APP_ID)
        .header("User-Agent", ua::pick(ua::Platform::Desktop))
        .header("Accept", "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .referer("https://www.instagram.com/");

    let resp = http.send(req).await?;
    if resp.status.as_u16() == 401 || resp.status.as_u16() == 403 {
        return Err(Error::login("Instagram 要求这个出口登录"));
    }
    resp.error_for_status()?;
    let json = resp.json()?;

    let item = util::arr_at(&json, &["items"])
        .first()
        .cloned()
        .ok_or_else(|| Error::deleted("Instagram 没有返回作品"))?;

    let info = build(&item);
    if info.is_empty() {
        return Err(Error::bare(crate::Reason::Empty));
    }
    Ok(info)
}

fn build(item: &Value) -> VideoInfo {
    let mut images = Vec::new();
    let mut video_url = String::new();
    let mut width = 0;
    let mut height = 0;

    // 轮播帖：carousel_media 里每一条各自是图或视频
    let carousel = util::arr_at(item, &["carousel_media"]);
    if !carousel.is_empty() {
        for child in carousel {
            let v = best_video(child);
            if v.is_empty() {
                let img = best_image(child);
                if !img.is_empty() {
                    images.push(Image::new(img));
                }
            } else if video_url.is_empty() {
                // 轮播里混着视频时，第一条视频当主视频
                video_url = v;
                width = util::u32_at(child, &["original_width"]);
                height = util::u32_at(child, &["original_height"]);
            }
        }
    } else {
        video_url = best_video(item);
        if video_url.is_empty() {
            let img = best_image(item);
            if !img.is_empty() {
                images.push(Image::new(img));
            }
        }
        width = util::u32_at(item, &["original_width"]);
        height = util::u32_at(item, &["original_height"]);
    }

    if !video_url.is_empty() {
        images.clear();
    }

    let user = util::get(item, &["user"]).cloned().unwrap_or_default();
    VideoInfo {
        video_url,
        cover_url: best_image(item),
        title: util::str_at(item, &["caption", "text"]),
        images,
        duration: util::num_at(item, &["video_duration"]),
        width,
        height,
        author: Author::new(
            util::first_id(&user, &[&["pk"], &["id"]]),
            util::first_str(&user, &[&["username"], &["full_name"]]),
            util::str_at(&user, &["profile_pic_url"]),
        ),
        ..Default::default()
    }
}

/// `video_versions` 里分辨率最高的那条。
fn best_video(node: &Value) -> String {
    util::arr_at(node, &["video_versions"])
        .iter()
        .max_by_key(|v| util::u64_at(v, &["width"]) * util::u64_at(v, &["height"]))
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default()
}

fn best_image(node: &Value) -> String {
    util::arr_at(node, &["image_versions2", "candidates"])
        .iter()
        .max_by_key(|v| util::u64_at(v, &["width"]) * util::u64_at(v, &["height"]))
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default()
}

async fn og_fallback(http: &Http, url: &str, platform: &str) -> Result<VideoInfo> {
    let resp = http
        .send(
            Req::get(url)
                // 用爬虫 UA：这些站对已知爬虫反而会老老实实吐 OG 标签
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (compatible; facebookexternalhit/1.1; +http://www.facebook.com/externalhit_uatext.php)",
                )
                .header("Accept", "text/html,*/*"),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    meta::from_og(&html).ok_or_else(|| {
        let title = util::html_title(&html);
        if html.contains("/accounts/login") || title.contains("Login") {
            Error::login(format!("{platform} 要求这个出口登录"))
        } else {
            Error::restricted(format!(
                "{platform} 没有对外给出这条内容（标题: {}）",
                if title.is_empty() { "无" } else { &title }
            ))
        }
    })
}

/// `/p/<code>/`、`/reel/<code>/`、`/tv/<code>/`、`/post/<code>` 里的 code。
fn shortcode_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let i = segs
        .iter()
        .position(|s| matches!(*s, "p" | "reel" | "reels" | "tv" | "post"))?;
    let code = segs.get(i + 1)?;
    (!code.is_empty()).then(|| (*code).to_owned())
}

/// shortcode → media_id。
///
/// shortcode 就是 media_id 用一套 base64 变体编码出来的：每个字符 6 bit，
/// 大端拼起来。知道这个就不用再去页面里翻 id 了，省一次请求。
fn media_id_from_shortcode(code: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // 超过 11 位就会溢出 u64；正常的 code 是 11 位
    if code.is_empty() || code.len() > 11 {
        return None;
    }
    let mut id: u64 = 0;
    for b in code.bytes() {
        let v = ALPHABET.iter().position(|a| *a == b)? as u64;
        id = id.checked_mul(64)?.checked_add(v)?;
    }
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_shortcode() {
        for url in [
            "https://www.instagram.com/p/CxYzAbCdEfG/",
            "https://www.instagram.com/reel/CxYzAbCdEfG/?igshid=1",
            "https://www.instagram.com/tv/CxYzAbCdEfG/",
            "https://www.threads.net/@user/post/CxYzAbCdEfG",
        ] {
            assert_eq!(
                shortcode_from_url(url).as_deref(),
                Some("CxYzAbCdEfG"),
                "{url}"
            );
        }
        assert_eq!(
            shortcode_from_url("https://www.instagram.com/someuser/"),
            None
        );
    }

    #[test]
    fn shortcode_decodes_to_media_id() {
        // B 是字母表第 1 位, 所以 "B" -> 1; "BA" -> 64
        assert_eq!(media_id_from_shortcode("B").as_deref(), Some("1"));
        assert_eq!(media_id_from_shortcode("BA").as_deref(), Some("64"));
        // 已知样例: 真实 code 长度 11
        assert!(media_id_from_shortcode("CxYzAbCdEfG").is_some());
        // 非字母表字符和超长都要拒绝
        assert_eq!(media_id_from_shortcode("has space"), None);
        assert_eq!(media_id_from_shortcode("ABCDEFGHIJKLMNOP"), None);
    }

    #[test]
    fn builds_single_video_post() {
        let item = json!({
            "caption": {"text": "正文"},
            "video_duration": 12.5,
            "original_width": 1080, "original_height": 1920,
            "video_versions": [
                {"url": "https://v/low.mp4", "width": 480, "height": 854},
                {"url": "https://v/high.mp4", "width": 1080, "height": 1920}
            ],
            "image_versions2": {"candidates": [
                {"url": "https://p/small.jpg", "width": 320, "height": 568},
                {"url": "https://p/big.jpg", "width": 1080, "height": 1920}
            ]},
            "user": {"pk": 12345, "username": "someone", "profile_pic_url": "https://a/av.jpg"}
        });
        let info = build(&item);
        assert_eq!(info.video_url, "https://v/high.mp4");
        assert_eq!(info.cover_url, "https://p/big.jpg");
        assert_eq!(info.duration, 12.5);
        assert_eq!(info.author.uid, "12345");
        assert!(info.images.is_empty());
    }

    #[test]
    fn builds_carousel_gallery() {
        let item = json!({
            "caption": {"text": "多图"},
            "carousel_media": [
                {"image_versions2": {"candidates": [{"url": "https://p/1.jpg", "width": 1080, "height": 1080}]}},
                {"image_versions2": {"candidates": [{"url": "https://p/2.jpg", "width": 1080, "height": 1080}]}}
            ],
            "image_versions2": {"candidates": [{"url": "https://p/cover.jpg", "width": 640, "height": 640}]},
            "user": {"username": "u"}
        });
        let info = build(&item);
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 2);
        assert_eq!(info.cover_url, "https://p/cover.jpg");
    }

    #[test]
    fn carousel_with_video_prefers_video() {
        let item = json!({
            "carousel_media": [
                {"image_versions2": {"candidates": [{"url": "https://p/1.jpg", "width": 100, "height": 100}]}},
                {"video_versions": [{"url": "https://v/clip.mp4", "width": 720, "height": 1280}],
                 "original_width": 720, "original_height": 1280}
            ],
            "user": {"username": "u"}
        });
        let info = build(&item);
        assert_eq!(info.video_url, "https://v/clip.mp4");
        assert!(info.images.is_empty(), "有视频时图集要清掉");
        assert_eq!(info.width, 720);
    }
}
