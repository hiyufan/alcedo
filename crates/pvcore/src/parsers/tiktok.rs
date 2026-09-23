//! TikTok。
//!
//! 读网页里的 `__UNIVERSAL_DATA_FOR_REHYDRATION__`。这是 TikTok 自己给前端
//! 水合用的数据，包含无水印直链、各档码率、图文帖和音乐地址。
//!
//! 直链带签名且校验 Referer，所以解析结果里会附上必要的请求头——上层下载 / 代理
//! 时必须原样带上，否则 CDN 返回 403。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{short_side, Author, Format, Image, VideoInfo};
use crate::util;

const WEB_REFERER: &str = "https://www.tiktok.com/";

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let host = util::host_of(url).unwrap_or_default();

    // vm./vt./t. 都是短链，先跟到真实地址
    let page_url = if host.starts_with("vm.") || host.starts_with("vt.") || host.starts_with("t.") {
        http.resolve_redirect(url).await?
    } else {
        url.to_owned()
    };

    let info = from_page(http, &page_url).await?;
    Ok(info)
}

pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::unsupported("TikTok 的作品 ID 应当是一串数字"));
    }
    // 用户名用占位符也能跳到正确的作品页
    from_page(http, &format!("https://www.tiktok.com/@i/video/{id}")).await
}

async fn from_page(http: &Http, page_url: &str) -> Result<VideoInfo> {
    let resp = http
        .send(
            Req::get(page_url)
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .referer(WEB_REFERER)
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    let raw = util::script_by_id(&html, "__UNIVERSAL_DATA_FOR_REHYDRATION__")
        .ok_or_else(|| block_error(&html))?;
    let data: Value = serde_json::from_str(raw)?;

    let scope = util::get(&data, &["__DEFAULT_SCOPE__"])
        .ok_or_else(|| Error::parse("水合数据里没有 __DEFAULT_SCOPE__"))?;
    let detail = util::get(scope, &["webapp.video-detail"])
        .ok_or_else(|| Error::parse("水合数据里没有 webapp.video-detail"))?;

    let status = util::i64_at(detail, &["statusCode"]);
    if status != 0 {
        return Err(status_error(status));
    }

    let item = util::get(detail, &["itemInfo", "itemStruct"])
        .ok_or_else(|| Error::deleted("没有作品数据"))?;

    let mut info = build(item);
    // 直链要带这些头才不 403
    info.set_header("Referer", WEB_REFERER);
    info.set_header("User-Agent", ua::DESKTOP_FIXED);
    info.page_url = page_url.to_owned();
    Ok(info)
}

fn build(item: &Value) -> VideoInfo {
    let video = util::get(item, &["video"]).cloned().unwrap_or_default();
    let author = util::get(item, &["author"]).cloned().unwrap_or_default();

    // 图文帖
    let images: Vec<Image> = util::arr_at(item, &["imagePost", "images"])
        .iter()
        .filter_map(|img| {
            let u = util::arr_at(img, &["imageURL", "urlList"])
                .first()
                .and_then(Value::as_str)
                .unwrap_or("");
            (!u.is_empty()).then(|| Image::new(u))
        })
        .collect();

    // 视频：playAddr 是无水印的；downloadAddr 有的作品带水印，只当退路
    let video_url = if images.is_empty() {
        util::first_str(&video, &[&["playAddr"], &["downloadAddr"]])
    } else {
        String::new()
    };

    // 各档码率
    let mut formats = Vec::new();
    if images.is_empty() {
        for b in util::arr_at(&video, &["bitrateInfo"]) {
            let u = util::arr_at(b, &["PlayAddr", "UrlList"])
                .first()
                .and_then(Value::as_str)
                .unwrap_or("");
            if u.is_empty() || u == video_url {
                continue;
            }
            let w = util::u32_at(b, &["PlayAddr", "Width"]);
            let h = util::u32_at(b, &["PlayAddr", "Height"]);
            let short = short_side(w, h);
            let codec = if util::str_at(b, &["CodecType"]).contains("h265") {
                "H.265"
            } else {
                ""
            };
            // GearName 是 TikTok 的内部档位名（normal_720 / lowest_1080_1），
            // 不适合直接给用户看，只在算不出短边时兜底
            let gear = util::str_at(b, &["GearName"]);
            let quality = if short > 0 { "" } else { gear.as_str() };
            formats.push(Format {
                label: Format::label_for(quality, short, codec),
                url: u.to_owned(),
                ext: "mp4".into(),
                height: short,
                filesize: util::u64_at(b, &["PlayAddr", "DataSize"]),
                codec: codec.to_owned(),
                ..Default::default()
            });
        }
    }

    VideoInfo {
        video_url,
        cover_url: util::first_str(&video, &[&["cover"], &["originCover"], &["dynamicCover"]]),
        title: util::first_str(item, &[&["desc"], &["contents", "0", "desc"]]),
        music_url: util::str_at(item, &["music", "playUrl"]),
        images,
        duration: util::num_at(&video, &["duration"]),
        width: util::u32_at(&video, &["width"]),
        height: util::u32_at(&video, &["height"]),
        formats,
        author: Author::new(
            util::first_id(&author, &[&["id"], &["uniqueId"]]),
            util::first_str(&author, &[&["nickname"], &["uniqueId"]]),
            util::first_str(
                &author,
                &[&["avatarLarger"], &["avatarMedium"], &["avatarThumb"]],
            ),
        ),
        ..Default::default()
    }
}

/// `webapp.video-detail.statusCode` 的含义。
fn status_error(status: i64) -> Error {
    match status {
        10204 | 10231 => Error::deleted("作品不存在或已被删除"),
        10216 => Error::restricted("这是一条私密作品"),
        10222 => Error::restricted("作品仅对好友可见"),
        10217 | 10218 => Error::restricted("作品在当前地区不可见"),
        _ => Error::restricted(format!("TikTok 返回 statusCode={status}")),
    }
}

fn block_error(html: &str) -> Error {
    let title = util::html_title(html);
    let head = &html[..html.len().min(20_000)];
    if head.contains("captcha") || head.contains("verify") || title.contains("Verify") {
        return Error::blocked("TikTok 对服务器所在网络弹了验证，换个出口或配置代理");
    }
    if head.contains("tiktok.com/login") || title.contains("Log in") {
        return Error::login("TikTok 要求这个出口登录");
    }
    Error::parse(format!(
        "页面里没有 __UNIVERSAL_DATA_FOR_REHYDRATION__（标题: {}）",
        if title.is_empty() { "无" } else { &title }
    ))
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_video_with_bitrate_ladder() {
        let item = json!({
            "desc": "标题文案",
            "video": {
                "playAddr": "https://v16.tiktokcdn.com/clean.mp4",
                "downloadAddr": "https://v16.tiktokcdn.com/wm.mp4",
                "cover": "https://p16.tiktokcdn.com/c.jpg",
                "duration": 15, "width": 576, "height": 1024,
                "bitrateInfo": [
                    {"PlayAddr": {"UrlList": ["https://v/720.mp4"], "Width": 720, "Height": 1280, "DataSize": 3000},
                     "CodecType": "h264", "GearName": "normal_720"},
                    {"PlayAddr": {"UrlList": ["https://v/720h265.mp4"], "Width": 720, "Height": 1280, "DataSize": 1500},
                     "CodecType": "h265_hvc1", "GearName": "hvc_720"}
                ]
            },
            "music": {"playUrl": "https://m/audio.mp3"},
            "author": {"id": 6789, "nickname": "作者", "uniqueId": "user1",
                       "avatarLarger": "https://a/big.jpg"}
        });
        let info = build(&item);
        assert_eq!(
            info.video_url, "https://v16.tiktokcdn.com/clean.mp4",
            "要无水印的 playAddr"
        );
        assert_eq!(info.music_url, "https://m/audio.mp3");
        assert_eq!(info.author.uid, "6789");
        assert_eq!(info.author.name, "作者");
        assert_eq!(info.formats.len(), 2);
        assert!(info.formats.iter().any(|f| f.codec == "H.265"));
    }

    #[test]
    fn internal_gear_names_do_not_leak_into_labels() {
        // GearName 是 TikTok 内部叫法，展示出来用户看不懂；能算出短边就该用 "720p"
        let item = json!({
            "video": {
                "playAddr": "https://v/default.mp4",
                "bitrateInfo": [
                    {"PlayAddr": {"UrlList": ["https://v/720.mp4"], "Width": 720, "Height": 1280},
                     "GearName": "normal_720", "CodecType": "h264"},
                    // 拿不到宽高时才退回 GearName，总比空标签强
                    {"PlayAddr": {"UrlList": ["https://v/unknown.mp4"]}, "GearName": "lowest_1080_1"}
                ]
            },
            "author": {"uniqueId": "u"}
        });
        let labels: Vec<_> = build(&item).formats.into_iter().map(|f| f.label).collect();
        assert_eq!(labels, vec!["720p", "lowest_1080_1"]);
    }

    #[test]
    fn builds_photo_post() {
        let item = json!({
            "desc": "图文",
            "imagePost": {"images": [
                {"imageURL": {"urlList": ["https://p/1.jpg", "https://p/1-alt.jpg"]}},
                {"imageURL": {"urlList": ["https://p/2.jpg"]}}
            ]},
            "video": {"playAddr": "https://v/should-drop.mp4", "cover": "https://p/c.jpg"},
            "author": {"uniqueId": "u"}
        });
        let info = build(&item);
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 2);
        assert!(info.video_url.is_empty(), "图文帖不该带视频地址");
        assert!(info.formats.is_empty());
        // uniqueId 当 uid 的退路
        assert_eq!(info.author.uid, "u");
    }

    #[test]
    fn status_codes_map_to_reasons() {
        assert_eq!(status_error(10204).reason, crate::Reason::Deleted);
        assert_eq!(status_error(10216).reason, crate::Reason::Restricted);
        assert_eq!(status_error(99999).reason, crate::Reason::Restricted);
    }

    #[test]
    fn captcha_page_is_blocked() {
        assert_eq!(
            block_error("<html><body>captcha required</body></html>").reason,
            crate::Reason::Blocked
        );
    }

    #[tokio::test]
    async fn non_numeric_id_rejected() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::TikTok),
        )
        .unwrap();
        assert_eq!(
            parse_id(&http, "abc").await.unwrap_err().reason,
            crate::Reason::Unsupported
        );
    }
}
