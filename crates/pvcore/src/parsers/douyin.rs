//! 抖音 / 抖音火山版。
//!
//! 主路径是 `slidesinfo` 接口，它同时返回视频和图文（含实况照片）的 `aweme_details`。
//!
//! 页面 SSR（`window._ROUTER_DATA`）只当安全网：2026-09 实测抖音已经不在 SSR 里渲染
//! `videoInfoRes` 了，能正常解析的作品走这条路一样拿不到。真要修抖音解析，
//! 从 `slidesinfo` 那条路查起。

use serde_json::Value;

use crate::error::{Error, Reason, Result};
use crate::http::{Http, Req};
use crate::model::{short_side, Author, Format, Image, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let host = util::host_of(url).unwrap_or_default();

    let video_id = if host == "v.douyin.com" {
        let location = http.resolve_redirect(url).await?;
        // 抖音的分享链接有时会跳到西瓜视频, 那边拿不到 aweme_id。
        // 早先这里返回空字符串, 上层统一报 "Failed to parse video ID", 被归类成
        // deleted, 对用户谎称"内容已被删除"——站点本来就支持西瓜, 直说就行。
        if location.contains("ixigua.com") {
            return Err(Error::unsupported(
                "这条分享链接跳转到了西瓜视频，直接粘贴西瓜视频的链接就能解析",
            ));
        }
        video_id_from_url(&location)
    } else {
        video_id_from_url(url)
    };

    let video_id = video_id
        .ok_or_else(|| Error::deleted("链接里没有作品 ID（可能跳到了个人主页或活动页）"))?;

    parse_id(http, &video_id).await
}

pub async fn parse_id(http: &Http, video_id: &str) -> Result<VideoInfo> {
    let data = match slides_info(http, video_id).await? {
        Some(d) => d,
        None => ssr_fallback(http, video_id).await?,
    };
    build(&data)
}

/// 从各种形态的抖音地址里取 aweme_id。
///
/// aweme_id 是一串纯数字。早先无脑取路径最后一段，短链跳到个人主页、活动页这类
/// 不带作品 ID 的地址时，会把 `user` 之类当 ID 拿去查接口，报出来的原因驴唇不对马嘴。
fn video_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;

    // 网页精选页: https://www.douyin.com/jingxuan?modal_id=7555093909760789812
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "modal_id") {
        if !v.is_empty() {
            return Some(v.into_owned());
        }
    }

    // https://www.iesdouyin.com/share/video/{id}/
    // https://www.douyin.com/video/{id}  /  /note/{id}
    parsed
        .path_segments()?
        .filter(|s| !s.is_empty())
        .rfind(|s| s.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
}

/// 作品详情接口。拿不到数据返回 `None`（留给 SSR 兜底），被平台明确挡掉则报错。
async fn slides_info(http: &Http, video_id: &str) -> Result<Option<Value>> {
    // 普通视频不带 request_source 就能拿到；图文（note）需要 request_source=200。
    let urls = [
        format!(
            "https://www.iesdouyin.com/web/api/v2/aweme/slidesinfo/?aweme_ids=%5B{video_id}%5D"
        ),
        format!(
            "https://www.iesdouyin.com/web/api/v2/aweme/slidesinfo/?aweme_ids=%5B{video_id}%5D&request_source=200"
        ),
    ];

    // 抖音对某些作品返回 status_code=0 + aweme_details=null + filter_list，意思是
    // "接口正常，但这条不对外给数据"。只有两个入口都没拿到才算数：正常视频带
    // request_source=200 也会被 filter，先判就会误报。
    let mut filtered: Option<Value> = None;

    for api in urls {
        let mut req = Req::get(&api).referer("https://www.douyin.com/");
        if let Some(c) = http.config().douyin_cookie.as_deref() {
            req = req.cookie_header(c);
        }
        let Ok(resp) = http.send(req).await else {
            continue;
        };
        if !resp.status.is_success() {
            continue;
        }
        let Ok(json) = resp.json() else { continue };

        if !util::arr_at(&json, &["aweme_details"]).is_empty() {
            return Ok(Some(json["aweme_details"][0].clone()));
        }
        if let Some(first) = util::arr_at(&json, &["filter_list"]).first() {
            filtered = Some(first.clone());
        }
    }

    if let Some(f) = filtered {
        // filter_list 一般只给个 reason 码，没有 detail_msg；有就带上
        let detail = util::first_str(&f, &[&["detail_msg"], &["notice"]]);
        let detail = if detail.is_empty() {
            format!("抖音 filter reason={}", util::i64_at(&f, &["reason"]))
        } else {
            detail
        };
        return Err(Error::restricted(detail));
    }

    Ok(None)
}

/// 页面 SSR 兜底。
async fn ssr_fallback(http: &Http, video_id: &str) -> Result<Value> {
    let page = format!("https://www.iesdouyin.com/share/video/{video_id}/");
    let mut req = Req::get(&page);
    if let Some(c) = http.config().douyin_cookie.as_deref() {
        req = req.cookie_header(c);
    }
    let resp = http.send(req).await?;
    resp.error_for_status()?;
    let html = resp.text();

    let raw = util::json_after(&html, "window._ROUTER_DATA")
        .ok_or_else(|| Error::parse("slidesinfo 无数据，页面里也没有 _ROUTER_DATA"))?;
    if !raw.contains("videoInfoRes") {
        // 注意别归成 restricted：SSR 这条路对所有作品都失效，拿它当"平台限制"
        // 的证据会把网络抖动也误报进去。
        return Err(Error::parse("slidesinfo 无数据，SSR 也没渲染 videoInfoRes"));
    }

    let json: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;
    let loader = util::get(&json, &["loaderData"])
        .ok_or_else(|| Error::parse("_ROUTER_DATA 里没有 loaderData"))?;

    let info = ["video_(id)/page", "note_(id)/page"]
        .iter()
        .find_map(|k| util::get(loader, &[k, "videoInfoRes"]))
        .ok_or_else(|| Error::parse("loaderData 里没有 videoInfoRes"))?;

    let items = util::arr_at(info, &["item_list"]);
    if items.is_empty() {
        let msg = util::arr_at(info, &["filter_list"])
            .first()
            .map(|f| util::str_at(f, &["detail_msg"]))
            .unwrap_or_default();
        return Err(if msg.is_empty() {
            Error::deleted("页面里没有作品数据")
        } else {
            Error::restricted(msg)
        });
    }
    Ok(items[0].clone())
}

fn build(data: &Value) -> Result<VideoInfo> {
    let video = util::get(data, &["video"]).cloned().unwrap_or(Value::Null);

    // ---- 图集 ----
    let mut images = Vec::new();
    for img in util::arr_at(data, &["images"]) {
        // download_url_list 带用户名水印 (tplv-dy-water-v2)，不能用
        let url = util::prefer_non_webp(util::arr_at(img, &["url_list"]));
        if url.is_empty() {
            continue;
        }
        let live = util::arr_at(img, &["video", "play_addr", "url_list"])
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        images.push(Image {
            url,
            live_photo_url: live,
        });
    }

    // ---- 视频 ----
    // 默认档用 H.264（浏览器能直接播），其余清晰度进 formats
    let primary = util::get(&video, &["play_addr_h264"])
        .or_else(|| util::get(&video, &["play_addr"]))
        .cloned()
        .unwrap_or(Value::Null);

    let mut video_url = String::new();
    let mut width = 0;
    let mut height = 0;
    if let Some(first) = util::arr_at(&primary, &["url_list"])
        .first()
        .and_then(Value::as_str)
    {
        video_url = first.replace("playwm", "play");
        width = util::u32_at(&primary, &["width"]);
        height = util::u32_at(&primary, &["height"]);
    }

    let duration = util::num_at(&video, &["duration"]) / 1000.0;
    let mut formats = collect_formats(&video, &primary);

    // 有图集就没有视频，这时候上面那个地址是打不开的，置空

    let music_url = if !images.is_empty() {
        video_url.clear();
        formats.clear();
        // 图集的音频在 video.play_addr.uri 里
        util::str_at(&primary, &["uri"])
    } else {
        let music = util::get(data, &["music", "play_url"])
            .cloned()
            .unwrap_or(Value::Null);
        util::arr_at(&music, &["url_list"])
            .first()
            .and_then(Value::as_str)
            .map_or_else(|| util::str_at(&music, &["uri"]), str::to_owned)
    };

    let cover_url = util::prefer_non_webp(util::arr_at(&video, &["cover", "url_list"]));

    let author = util::get(data, &["author"]).cloned().unwrap_or(Value::Null);
    let avatar = util::arr_at(&author, &["avatar_thumb", "url_list"])
        .first()
        .and_then(Value::as_str)
        .unwrap_or_default();

    let info = VideoInfo {
        video_url,
        cover_url,
        title: util::str_at(data, &["desc"]),
        music_url,
        images,
        author: Author::new(
            util::str_at(&author, &["sec_uid"]),
            util::str_at(&author, &["nickname"]),
            avatar,
        ),
        duration,
        width,
        height,
        formats,
        ..Default::default()
    };

    if info.is_empty() {
        return Err(Error::bare(Reason::Empty));
    }
    Ok(info)
}

/// 从 `bit_rate` 档位里挑出和默认直链不同的清晰度 / 编码。
///
/// 每个 (短边, 编码) 只留码率最高的一档，并且只保留比默认直链更清晰的，
/// 或者同清晰度但体积更小的 H.265。
fn collect_formats(video: &Value, primary: &Value) -> Vec<Format> {
    let primary_key = util::str_at(primary, &["url_key"]);
    let primary_short = short_side(
        util::u32_at(primary, &["width"]),
        util::u32_at(primary, &["height"]),
    );

    let mut best: Vec<(u32, String, Format)> = Vec::new();

    for br in util::arr_at(video, &["bit_rate"]) {
        let pa = util::get(br, &["play_addr"])
            .cloned()
            .unwrap_or(Value::Null);
        let Some(url) = util::arr_at(&pa, &["url_list"])
            .first()
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !primary_key.is_empty() && util::str_at(&pa, &["url_key"]) == primary_key {
            continue;
        }

        let w = util::u32_at(&pa, &["width"]);
        let h = util::u32_at(&pa, &["height"]);
        // 竖屏视频 height 才是长边，统一用短边当"清晰度"
        let short = short_side(w, h);
        let codec = if util::get(br, &["is_h265"])
            .and_then(Value::as_i64)
            .unwrap_or(0)
            != 0
        {
            "H.265"
        } else {
            ""
        };
        let size = util::u64_at(&pa, &["data_size"]);

        if let Some(slot) = best.iter_mut().find(|(s, c, _)| *s == short && c == codec) {
            if slot.2.filesize >= size {
                continue;
            }
            slot.2 = make_format(short, codec, url, size);
        } else {
            best.push((
                short,
                codec.to_owned(),
                make_format(short, codec, url, size),
            ));
        }
    }

    best.into_iter()
        .filter(|(short, codec, _)| {
            *short > primary_short || (!codec.is_empty() && *short == primary_short)
        })
        .map(|(_, _, f)| f)
        .collect()
}

fn make_format(short: u32, codec: &str, url: &str, size: u64) -> Format {
    Format {
        label: Format::label_for("", short, codec),
        url: url.replace("playwm", "play"),
        ext: "mp4".into(),
        height: short,
        filesize: size,
        codec: codec.to_owned(),
        ..Default::default()
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的；扩展名断言用的是测试自己造的小写数据
#[allow(clippy::float_cmp, clippy::case_sensitive_file_extension_comparisons)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_id_from_every_url_shape() {
        let cases = [
            (
                "https://www.douyin.com/video/7424432820954598707",
                "7424432820954598707",
            ),
            (
                "https://www.douyin.com/note/7424432820954598707",
                "7424432820954598707",
            ),
            (
                "https://www.iesdouyin.com/share/video/7424432820954598707/?region=CN&mid=123",
                "7424432820954598707",
            ),
            (
                "https://www.douyin.com/jingxuan?modal_id=7555093909760789812",
                "7555093909760789812",
            ),
        ];
        for (url, want) in cases {
            assert_eq!(video_id_from_url(url).as_deref(), Some(want), "{url}");
        }
    }

    #[test]
    fn non_numeric_paths_yield_no_id() {
        // 跳到个人主页时不能把 "user" 当成作品 ID 拿去查接口
        assert_eq!(
            video_id_from_url("https://www.douyin.com/user/MS4wLjABAAAA"),
            None
        );
        assert_eq!(video_id_from_url("https://www.douyin.com/"), None);
    }

    #[test]
    fn builds_video_with_formats() {
        let data = json!({
            "desc": "标题",
            "video": {
                "duration": 15000,
                "play_addr_h264": {
                    "url_list": ["https://v.douyinvod.com/playwm/a.mp4"],
                    "width": 720, "height": 1280, "url_key": "k720"
                },
                "cover": { "url_list": ["https://p.douyinpic.com/c.webp", "https://p.douyinpic.com/c.jpeg"] },
                "bit_rate": [
                    { "is_h265": 0, "play_addr": {
                        "url_list": ["https://v.douyinvod.com/1080.mp4"],
                        "width": 1080, "height": 1920, "url_key": "k1080", "data_size": 5000000 } },
                    { "is_h265": 1, "play_addr": {
                        "url_list": ["https://v.douyinvod.com/720h265.mp4"],
                        "width": 720, "height": 1280, "url_key": "k720h265", "data_size": 2000000 } }
                ]
            },
            "music": { "play_url": { "url_list": ["https://music/m.mp3"] } },
            "author": { "sec_uid": "MS4w", "nickname": "作者", "avatar_thumb": { "url_list": ["https://a/av.jpg"] } }
        });

        let info = build(&data).unwrap();
        // playwm -> play: 带 wm 的是有水印版本
        assert_eq!(info.video_url, "https://v.douyinvod.com/play/a.mp4");
        assert_eq!(info.duration, 15.0);
        assert_eq!(info.title, "标题");
        assert_eq!(info.author.name, "作者");
        // 封面优先非 webp
        assert!(info.cover_url.ends_with(".jpeg"));
        assert_eq!(info.music_url, "https://music/m.mp3");

        // 1080p 比默认的 720 清晰要留; 720p H.265 同清晰度但更省体积也要留
        assert_eq!(info.formats.len(), 2);
        assert!(info.formats.iter().any(|f| f.label == "1080p"));
        assert!(info.formats.iter().any(|f| f.label == "720p H.265"));
    }

    #[test]
    fn gallery_clears_video_url() {
        let data = json!({
            "desc": "图文",
            "images": [
                { "url_list": ["https://p/1.webp", "https://p/1.jpeg"],
                  "video": { "play_addr": { "url_list": ["https://v/live.mp4"] } } },
                { "url_list": ["https://p/2.jpeg"] }
            ],
            "video": {
                "play_addr": { "url_list": ["https://v/should-be-dropped.mp4"], "uri": "audio-uri" },
                "cover": { "url_list": ["https://p/c.jpeg"] }
            }
        });
        let info = build(&data).unwrap();
        assert!(info.video_url.is_empty(), "图集不该带视频地址");
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 2);
        assert_eq!(info.images[0].live_photo_url, "https://v/live.mp4");
        assert!(info.images[0].url.ends_with(".jpeg"));
        assert_eq!(info.music_url, "audio-uri");
    }

    #[test]
    fn lower_quality_formats_are_dropped() {
        let data = json!({
            "video": {
                "play_addr_h264": {
                    "url_list": ["https://v/720.mp4"], "width": 720, "height": 1280, "url_key": "k" },
                "bit_rate": [
                    { "play_addr": { "url_list": ["https://v/480.mp4"],
                      "width": 480, "height": 854, "url_key": "k480" } }
                ]
            }
        });
        let info = build(&data).unwrap();
        assert!(info.formats.is_empty(), "比默认档更低的清晰度没有意义");
    }

    #[test]
    fn empty_payload_is_reported_as_empty() {
        let err = build(&json!({"desc": "空"})).unwrap_err();
        assert_eq!(err.reason, Reason::Empty);
    }
}
