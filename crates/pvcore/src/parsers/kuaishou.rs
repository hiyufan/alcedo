//! 快手。
//!
//! 流程：短链不跟跳转拿到 `Location` 和第一份 cookie，再带着这份 cookie 去请求
//! 落地页，从 `window.INIT_STATE` 里取数据。cookie 必须传下去——快手拿它认会话，
//! 少了这一步落地页会返回空壳。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{Http, Req};
use crate::model::{short_side, Author, Format, Image, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    // 第一跳：拿 Location + cookie
    let first = http
        .send(Req::get(url).referer("https://v.kuaishou.com/").no_follow())
        .await?;

    let location = first
        .location()
        .ok_or_else(|| Error::deleted("快手短链没有返回跳转地址"))?;
    // /fw/long-video/ 返回的结构和普通作品不一样，统一换成 /fw/photo/
    let location = location.replace("/fw/long-video/", "/fw/photo/");

    let mut req = Req::get(&location).referer("https://v.kuaishou.com/");
    for (k, v) in first.set_cookies() {
        req = req.cookie(k, v);
    }
    let resp = http.send(req).await?;
    resp.error_for_status()?;
    let html = resp.text();

    let raw = util::json_after(&html, "window.INIT_STATE").ok_or_else(|| block_error(&html))?;
    let json: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    build(&json)
}

fn build(state: &Value) -> Result<VideoInfo> {
    // INIT_STATE 是个以随机 key 组织的字典，作品数据藏在同时含 result 和 photo 的那一项
    let entry = state
        .as_object()
        .and_then(|m| {
            m.values()
                .find(|v| v.get("result").is_some() && v.get("photo").is_some())
        })
        .ok_or_else(|| Error::parse("INIT_STATE 里没有作品数据"))?;

    let result = util::i64_at(entry, &["result"]);
    if result != 1 {
        return Err(match result {
            2 | 400002 => Error::deleted(format!("快手 result={result}")),
            _ => Error::restricted(format!("获取作品信息失败 result={result}")),
        });
    }

    let photo = util::get(entry, &["photo"]).ok_or_else(|| Error::parse("没有 photo 字段"))?;

    // 视频：mainMvUrls 里第一条是默认档，其余当备用地址
    let mv = util::arr_at(photo, &["mainMvUrls"]);
    let video_url = mv
        .first()
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default();

    // 图集：cdn + 相对路径拼出来
    let cdns = util::arr_at(photo, &["ext_params", "atlas", "cdn"]);
    let list = util::arr_at(photo, &["ext_params", "atlas", "list"]);
    let mut images = Vec::new();
    if let Some(cdn) = cdns.first().and_then(Value::as_str) {
        for item in list {
            if let Some(path) = item.as_str() {
                images.push(Image::new(format!("https://{cdn}/{path}")));
            }
        }
    }

    // 备用清晰度：有的作品会给多档 manifest
    let mut formats = Vec::new();
    for rep in util::arr_at(photo, &["manifest", "adaptationSet", "0", "representation"]) {
        let u = util::str_at(rep, &["url"]);
        if u.is_empty() || u == video_url {
            continue;
        }
        let w = util::u32_at(rep, &["width"]);
        let h = util::u32_at(rep, &["height"]);
        let short = short_side(w, h);
        // 算得出短边就用短边；qualityLabel 是快手自己的叫法，只当兜底
        let quality = if short > 0 {
            String::new()
        } else {
            util::str_at(rep, &["qualityLabel"])
        };
        formats.push(Format {
            label: Format::label_for(&quality, short, ""),
            url: u,
            ext: "mp4".into(),
            height: short,
            filesize: util::u64_at(rep, &["fileSize"]),
            ..Default::default()
        });
    }

    let width = util::u32_at(photo, &["width"]);
    let height = util::u32_at(photo, &["height"]);

    Ok(VideoInfo {
        video_url: if images.is_empty() {
            video_url
        } else {
            String::new()
        },
        cover_url: util::first_str(
            photo,
            &[
                &["coverUrls", "0", "url"],
                &["coverUrl"],
                &["webpCoverUrls", "0", "url"],
            ],
        ),
        title: util::str_at(photo, &["caption"]),
        images,
        author: Author::new(
            util::first_id(photo, &[&["userEid"], &["userId"]]),
            util::str_at(photo, &["userName"]),
            util::str_at(photo, &["headUrl"]),
        ),
        duration: util::num_at(photo, &["duration"]) / 1000.0,
        width,
        height,
        formats,
        ..Default::default()
    })
}

/// 落地页里没有 INIT_STATE 时，说清楚快手到底返回了什么。
fn block_error(html: &str) -> Error {
    let title = util::html_title(html);
    let markers = ["验证", "captcha", "滑块", "安全", "访问频繁"];
    if markers
        .iter()
        .any(|m| !html.is_empty() && html[..html.len().min(20000)].contains(m))
    {
        return Error::blocked(format!(
            "快手返回了验证页（标题: {}），服务器 IP 被限流了",
            if title.is_empty() { "无" } else { &title }
        ));
    }
    Error::parse(format!(
        "页面里没有 INIT_STATE（标题: {}）",
        if title.is_empty() { "无" } else { &title }
    ))
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state_with(photo: Value, result: i64) -> Value {
        json!({
            "tachyonjs": {"whatever": 1},
            "pc-fw-photo-1a2b": {"result": result, "photo": photo}
        })
    }

    #[test]
    fn parses_video() {
        let state = state_with(
            json!({
                "caption": "标题",
                "duration": 12000,
                "width": 720, "height": 1280,
                "mainMvUrls": [{"url": "https://v.kwaicdn.com/a.mp4"}],
                "coverUrls": [{"url": "https://c.kwaicdn.com/c.jpg"}],
                "userName": "作者", "headUrl": "https://a/av.jpg", "userEid": "uid1"
            }),
            1,
        );
        let info = build(&state).unwrap();
        assert_eq!(info.video_url, "https://v.kwaicdn.com/a.mp4");
        assert_eq!(info.title, "标题");
        assert_eq!(info.duration, 12.0);
        assert_eq!(info.author.uid, "uid1");
    }

    #[test]
    fn parses_atlas_gallery() {
        let state = state_with(
            json!({
                "caption": "图集",
                "coverUrls": [{"url": "https://c/c.jpg"}],
                "mainMvUrls": [{"url": "https://v/should-drop.mp4"}],
                "userName": "u",
                "ext_params": {"atlas": {
                    "cdn": ["img.kwaicdn.com"],
                    "list": ["p/1.jpg", "p/2.jpg"]
                }}
            }),
            1,
        );
        let info = build(&state).unwrap();
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 2);
        assert_eq!(info.images[0].url, "https://img.kwaicdn.com/p/1.jpg");
        assert!(info.video_url.is_empty(), "图集不该带视频地址");
    }

    #[test]
    fn non_ok_result_is_reported() {
        let err = build(&state_with(json!({}), 2)).unwrap_err();
        assert_eq!(err.reason, crate::Reason::Deleted);
    }

    #[test]
    fn missing_photo_entry_is_parse_error() {
        let err = build(&json!({"a": {"b": 1}})).unwrap_err();
        assert_eq!(err.reason, crate::Reason::Parse);
    }

    #[test]
    fn captcha_page_is_classified_as_blocked() {
        let html = "<html><title>验证</title><body>请完成滑块验证</body></html>";
        assert_eq!(block_error(html).reason, crate::Reason::Blocked);
    }
}
