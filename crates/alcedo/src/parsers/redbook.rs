//! 小红书。
//!
//! 两条路都能拿到 `__INITIAL_STATE__`，但门槛不一样：
//!
//! - **桌面页** (`note.noteDetailMap`)：视频档位最全，但机房 / 海外 IP 常被弹登录墙。
//! - **手机分享页** (`noteData.data.noteData`)：给"在手机浏览器里打开分享链接"用的
//!   落地页，对出口 IP 宽容得多，代价是档位少一些。
//!
//! 默认先走桌面页；撞过一次登录墙之后，进程内记住，后面直接走手机页——那条路
//! 已经证明走不通，再撞一次就是白白多一个跨洋往返。

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::error::{Error, Reason, Result};
use crate::http::{Http, Req};
use crate::model::{short_side, Author, Format, Image, VideoInfo};
use crate::util;

/// 原图：按 key 从 ci 取，`w/0` 表示不缩放。页面里给的都是缩到 1080 宽的版本。
const ORIGINAL_IMAGE: &str = "https://ci.xiaohongshu.com/{key}?imageView2/2/w/0/format/jpg/q/90";

const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36";
const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) \
                         AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1";

/// 桌面页撞过登录墙就置位，之后优先走手机页。重启即忘，代价只是重启后第一个
/// 用户替后面的人垫一次白跑的请求。
static PREFER_MOBILE: AtomicBool = AtomicBool::new(false);

/// 解析一条小红书分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let mobile_first = PREFER_MOBILE.load(Ordering::Relaxed);
    let order = if mobile_first {
        [Mode::Mobile, Mode::Desktop]
    } else {
        [Mode::Desktop, Mode::Mobile]
    };

    let mut last: Option<Error> = None;
    for mode in order {
        match fetch(http, url, mode).await {
            Ok(info) => return Ok(info),
            Err(e) if e.reason == Reason::Login => {
                PREFER_MOBILE.store(true, Ordering::Relaxed);
                last = Some(e);
            }
            // 链接过期 / 笔记不存在：换条路也是一样的结果，别浪费时间
            Err(e) if e.reason == Reason::Deleted => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| Error::parse("小红书解析失败")))
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Desktop,
    Mobile,
}

async fn fetch(http: &Http, url: &str, mode: Mode) -> Result<VideoInfo> {
    let ua = if mode == Mode::Desktop {
        DESKTOP_UA
    } else {
        MOBILE_UA
    };
    let mut req = Req::get(url).header("User-Agent", ua).header(
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    // 海外 / 机房 IP 经常拿到登录页，带上登录后的 cookie 就正常了
    if let Some(c) = http.config().xhs_cookie.as_deref() {
        req = req.cookie_header(c);
    }

    let resp = http.send(req).await?;
    resp.error_for_status()?;
    let final_url = resp.url.clone();
    let html = resp.text();

    if url::Url::parse(&final_url).is_ok_and(|u| u.path().contains("/login")) {
        return Err(Error::login("小红书要求这个出口登录"));
    }

    let raw = util::json_after(&html, "window.__INITIAL_STATE__")
        .ok_or_else(|| block_error(&final_url, &html))?;
    // 页面里的 JSON 混着 JS 的 undefined
    let state: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    match mode {
        Mode::Desktop => {
            let note =
                util::get(&state, &["note"]).ok_or_else(|| block_error(&final_url, &html))?;
            let note_id = util::str_at(note, &["currentNoteId"]);
            // 分享链接有有效期，过期后这里会是 undefined（已被换成 null）
            if note_id.is_empty() {
                return Err(Error::deleted("链接可能已过期（currentNoteId 为空）"));
            }
            let data = util::get(note, &["noteDetailMap", &note_id, "note"])
                .ok_or_else(|| Error::deleted("笔记详情为空（链接可能过期或缺 xsec_token）"))?;
            Ok(build(data, "urlDefault", "nickname"))
        }
        Mode::Mobile => {
            let data = util::get(&state, &["noteData", "data", "noteData"]).ok_or_else(|| {
                if final_url.contains("/404") || final_url.contains("sec_") {
                    Error::deleted("链接过期或笔记不存在")
                } else {
                    block_error(&final_url, &html)
                }
            })?;
            Ok(build(data, "url", "nickName"))
        }
    }
}

fn build(data: &Value, image_url_key: &str, nick_key: &str) -> VideoInfo {
    let mut video = video_part(data);
    if video.duration <= 0.0 {
        video.duration = util::num_at(data, &["video", "capa", "duration"]);
    }
    let Video {
        video_url,
        width,
        height,
        duration,
        formats,
    } = video;

    // 图集：换成原图分辨率；实况图带上那段短视频
    let image_list = util::arr_at(data, &["imageList"]);
    let mut images = Vec::new();
    if video_url.is_empty() {
        for item in image_list {
            let page_img = util::first_str(item, &[&[image_url_key], &["urlDefault"], &["url"]]);
            if page_img.is_empty() {
                continue;
            }
            let key = {
                let k = util::str_at(item, &["fileId"]);
                if k.is_empty() {
                    image_key(&page_img)
                } else {
                    k
                }
            };
            let mut img = Image::new(if key.is_empty() {
                page_img.clone()
            } else {
                ORIGINAL_IMAGE.replace("{key}", &key)
            });
            let live = util::arr_at(item, &["stream", "h264"])
                .iter()
                .find_map(|s| {
                    let u = util::str_at(s, &["masterUrl"]);
                    (!u.is_empty()).then_some(u)
                })
                .unwrap_or_default();
            if util::get(item, &["livePhoto"])
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                img.live_photo_url = live;
            }
            images.push(img);
        }
    }

    let cover = image_list
        .first()
        .map(|i| util::first_str(i, &[&[image_url_key], &["urlDefault"], &["url"]]))
        .unwrap_or_default();

    let user = util::get(data, &["user"]).cloned().unwrap_or_default();
    let title = {
        let t = util::str_at(data, &["title"]);
        if t.is_empty() {
            util::str_at(data, &["desc"]).chars().take(60).collect()
        } else {
            t
        }
    };

    VideoInfo {
        video_url,
        cover_url: cover,
        title,
        images,
        duration,
        width,
        height,
        formats,
        author: Author::new(
            util::id_at(&user, &["userId"]),
            util::first_str(&user, &[&[nick_key], &["nickname"], &["nickName"]]),
            util::str_at(&user, &["avatar"]),
        ),
        ..Default::default()
    }
}

/// `build` 拆出来的视频部分。
#[derive(Default)]
struct Video {
    video_url: String,
    width: u32,
    height: u32,
    duration: f64,
    formats: Vec<Format>,
}

/// 挑默认播放档 + 列出其余清晰度。
///
/// h264 里分辨率（再按码率）最高的那条当默认——浏览器能直接播；其余档位和
/// 一条 h265 进 formats。h265 体积小很多但兼容性差，所以只给一档、不当默认。
fn video_part(data: &Value) -> Video {
    let stream = util::get(data, &["video", "media", "stream"])
        .cloned()
        .unwrap_or_default();

    let mut h264: Vec<&Value> = util::arr_at(&stream, &["h264"])
        .iter()
        .filter(|s| !util::str_at(s, &["masterUrl"]).is_empty())
        .collect();
    h264.sort_by_key(|s| {
        let px = util::u64_at(s, &["width"]) * util::u64_at(s, &["height"]);
        let br = util::u64_at(s, &["videoBitrate"]).max(util::u64_at(s, &["avgBitrate"]));
        std::cmp::Reverse((px, br))
    });

    let Some(best) = h264.first() else {
        return Video::default();
    };

    let width = util::u32_at(best, &["width"]);
    let height = util::u32_at(best, &["height"]);
    let mut formats = Vec::new();

    // 同分辨率的多条只留一条：小红书经常给好几个码率版本
    let mut seen = vec![(width, height)];
    for s in h264.iter().skip(1) {
        let key = (util::u32_at(s, &["width"]), util::u32_at(s, &["height"]));
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        formats.push(stream_format(s, ""));
    }
    if let Some(s) = util::arr_at(&stream, &["h265"])
        .iter()
        .find(|s| !util::str_at(s, &["masterUrl"]).is_empty())
    {
        formats.push(stream_format(s, "H.265"));
    }

    Video {
        video_url: util::str_at(best, &["masterUrl"]),
        width,
        height,
        duration: util::num_at(best, &["duration"]) / 1000.0,
        formats,
    }
}

fn stream_format(s: &Value, codec: &str) -> Format {
    let short = short_side(util::u32_at(s, &["width"]), util::u32_at(s, &["height"]));
    Format {
        label: Format::label_for("", short, codec),
        url: util::str_at(s, &["masterUrl"]),
        ext: "mp4".into(),
        height: short,
        filesize: util::u64_at(s, &["size"]),
        codec: codec.to_owned(),
        ..Default::default()
    }
}

/// 从页面图片地址里取出存储 key，可能带 `notes_pre_post/` 或 `spectrum/` 前缀。
///
/// `http://sns-webpic-qc.xhscdn.com/<日期>/<hash>/notes_pre_post/<token>!h5_1080jpg`
/// → `notes_pre_post/<token>`
fn image_key(page_url: &str) -> String {
    let Ok(u) = url::Url::parse(page_url) else {
        return String::new();
    };
    let parts: Vec<&str> = u.path().split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 3 {
        return String::new();
    }
    parts[2..]
        .join("/")
        .split('!')
        .next()
        .unwrap_or("")
        .to_owned()
}

/// 页面不是笔记时，说清楚小红书到底返回了什么——站长才知道该配 cookie 还是代理。
fn block_error(final_url: &str, html: &str) -> Error {
    let title = util::html_title(html);
    if final_url.contains("/404") || final_url.contains("sec_") {
        return Error::deleted("链接过期或笔记不存在（小红书跳到了 404）");
    }
    if final_url.contains("/login") {
        return Error::login("小红书要求这个出口登录");
    }
    let head = &html[..html.len().min(20_000)];
    let markers = ["验证", "captcha", "verify", "安全", "网络连接异常", "海外"];
    if markers
        .iter()
        .any(|m| head.contains(m) || title.contains(m))
    {
        return Error::blocked(
            "小红书对服务器所在网络返回了验证页（被限流），请配置 ALCEDO_PROXY_CN 或 ALCEDO_XHS_COOKIE",
        );
    }
    Error::parse(format!(
        "小红书返回了意外页面（标题: {}，地址: {}）",
        if title.is_empty() { "无" } else { &title },
        &final_url[..final_url.len().min(80)]
    ))
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_key_handles_prefixes() {
        assert_eq!(
            image_key(
                "http://sns-webpic-qc.xhscdn.com/202401/abcdef/notes_pre_post/tok123!h5_1080jpg"
            ),
            "notes_pre_post/tok123"
        );
        assert_eq!(
            image_key("http://sns-webpic-qc.xhscdn.com/202401/abcdef/tok999!nd_dft_wlteh_jpg_3"),
            "tok999"
        );
        assert_eq!(image_key("http://x/short"), "");
    }

    #[test]
    fn picks_highest_h264_and_lists_rest() {
        let data = json!({
            "title": "笔记标题",
            "video": {"media": {"stream": {
                "h264": [
                    {"masterUrl": "https://v/720.mp4", "width": 720, "height": 1280, "size": 1000, "duration": 15000},
                    {"masterUrl": "https://v/1080.mp4", "width": 1080, "height": 1920, "size": 3000, "duration": 15000}
                ],
                "h265": [{"masterUrl": "https://v/1080h265.mp4", "width": 1080, "height": 1920, "size": 1500}]
            }}},
            "user": {"userId": "u1", "nickname": "作者"}
        });
        let info = build(&data, "urlDefault", "nickname");
        assert_eq!(info.video_url, "https://v/1080.mp4");
        assert_eq!(info.height, 1920);
        assert_eq!(info.duration, 15.0);
        assert_eq!(info.formats.len(), 2);
        assert!(info.formats.iter().any(|f| f.codec == "H.265"));
    }

    #[test]
    fn gallery_uses_original_image_and_live_photo() {
        let data = json!({
            "desc": "一段很长的正文".repeat(20),
            "imageList": [
                {"urlDefault": "http://sns.xhscdn.com/202401/hash/notes_pre_post/tok1!h5_1080jpg",
                 "livePhoto": true,
                 "stream": {"h264": [{"masterUrl": "https://v/live.mp4"}]}},
                {"urlDefault": "http://sns.xhscdn.com/202401/hash/tok2!x", "fileId": "fid2"}
            ],
            "user": {"nickname": "作者"}
        });
        let info = build(&data, "urlDefault", "nickname");
        assert_eq!(info.images.len(), 2);
        assert!(info.images[0]
            .url
            .contains("ci.xiaohongshu.com/notes_pre_post/tok1"));
        assert!(info.images[0].url.contains("w/0"), "要取原图尺寸");
        assert_eq!(info.images[0].live_photo_url, "https://v/live.mp4");
        // fileId 优先于从地址里推导
        assert!(info.images[1].url.contains("/fid2?"));
        // 没有 title 时截取正文
        assert_eq!(info.title.chars().count(), 60);
    }

    #[test]
    fn live_photo_url_only_set_when_flagged() {
        let data = json!({
            "imageList": [{"urlDefault": "http://a/b/c/tok!x",
                           "stream": {"h264": [{"masterUrl": "https://v/live.mp4"}]}}]
        });
        let info = build(&data, "urlDefault", "nickname");
        assert!(
            info.images[0].live_photo_url.is_empty(),
            "没标 livePhoto 就不该带实况地址"
        );
    }

    #[test]
    fn captcha_page_is_blocked_not_parse() {
        let e = block_error("https://www.xiaohongshu.com/x", "<title>请完成验证</title>");
        assert_eq!(e.reason, Reason::Blocked);
    }

    #[test]
    fn four_oh_four_is_deleted() {
        let e = block_error("https://www.xiaohongshu.com/404", "<html/>");
        assert_eq!(e.reason, Reason::Deleted);
    }
}
