//! AcFun。
//!
//! 页面结构在 2026 年换过：以前是 `var videoInfo = {...}` 加一个平行的
//! `var playInfo`，现在合并成了 `window.pageInfo = window.videoInfo = {...}`，
//! 播放地址挪进 `currentVideoInfo.ksPlayJson`——注意那是个**被当成字符串存的
//! JSON**，要解两次。照老结构写的解析器在现在的站点上一条都取不到。
//!
//! 给出的是 HLS（m3u8），不是单文件 mp4，上层下载要先合流。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let resp = http
        .send(
            Req::get(url)
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .referer("https://www.acfun.cn/"),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();
    build(&html)
}

fn build(html: &str) -> Result<VideoInfo> {
    // 两个名字指向同一个对象，哪个在前都认
    let raw = util::json_after(html, "window.videoInfo")
        .or_else(|| util::json_after(html, "window.pageInfo"))
        .ok_or_else(|| {
            let title = util::html_title(html);
            if html.contains("验证") || html.contains("captcha") {
                Error::blocked("AcFun 返回了验证页，服务器 IP 被限流了")
            } else {
                Error::parse(format!(
                    "页面里没有 window.videoInfo（标题: {}）",
                    if title.is_empty() { "无" } else { &title }
                ))
            }
        })?;

    let info: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    // 注意：`fansOnlyDesc` 名字唬人，其实就是视频简介，每条投稿都有——
    // 拿它判"粉丝专属"会把所有视频都误杀。真正的信号是下面取不到播放地址。
    let current = util::get(&info, &["currentVideoInfo"])
        .ok_or_else(|| Error::deleted("页面里没有 currentVideoInfo，投稿可能已删除"))?;

    // H.264 和 H.265 两套档位，各自是一段嵌套的 JSON 字符串
    let mut formats = Vec::new();
    formats.extend(representations(current, "ksPlayJson", ""));
    formats.extend(representations(current, "ksPlayJsonHevc", "H.265"));

    if formats.is_empty() {
        return Err(Error::restricted(
            "AcFun 没有下发播放地址（可能是充电专属或已下架）",
        ));
    }
    // 最清晰的那一档当默认
    formats.sort_by(|a, b| b.height.cmp(&a.height).then(a.codec.cmp(&b.codec)));
    let best = formats.remove(0);

    let user = util::get(&info, &["user"]).cloned().unwrap_or_default();

    Ok(VideoInfo {
        video_url: best.url,
        cover_url: util::first_str(&info, &[&["coverUrl"], &["coverCdnUrls", "0"]]),
        title: util::str_at(&info, &["title"]),
        duration: util::num_at(&info, &["durationMillis"]) / 1000.0,
        height: best.height,
        formats,
        author: Author::new(
            util::first_id(&user, &[&["id"], &["href"]]),
            util::str_at(&user, &["name"]),
            util::str_at(&user, &["headUrl"]),
        ),
        ..Default::default()
    })
}

/// 解开 `ksPlayJson` 这层字符串里的清晰度列表。
fn representations(current: &Value, key: &str, codec: &str) -> Vec<Format> {
    let Some(raw) = util::get(current, &[key]).and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(play) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };

    util::arr_at(&play, &["adaptationSet", "0", "representation"])
        .iter()
        .filter_map(|r| {
            let url = util::first_str(r, &[&["url"], &["backupUrl", "0"]]);
            if url.is_empty() {
                return None;
            }
            let height = util::u32_at(r, &["height"]);
            let quality = util::first_str(r, &[&["qualityLabel"], &["qualityType"]]);
            Some(Format {
                label: Format::label_for(&quality, height, codec),
                url,
                ext: "m3u8".into(),
                height,
                filesize: 0, // HLS 分片，页面不给总体积
                codec: codec.to_owned(),
                ..Default::default()
            })
        })
        .collect()
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// 按线上真实结构造的最小页面。
    fn page() -> String {
        let ks = serde_json::json!({
            "adaptationSet": [{"representation": [
                {"url": "https://v.acfun.cn/hls_1080p60.m3u8", "qualityLabel": "1080P60",
                 "height": 1080, "width": 1920, "qualityType": "1080p60"},
                {"url": "https://v.acfun.cn/hls_720p.m3u8", "qualityLabel": "720P",
                 "height": 720, "width": 1280, "qualityType": "720p"}
            ]}]
        })
        .to_string();
        let hevc = serde_json::json!({
            "adaptationSet": [{"representation": [
                {"url": "https://v.acfun.cn/hls_1080p_hevc.m3u8", "qualityLabel": "1080P",
                 "height": 1080, "width": 1920}
            ]}]
        })
        .to_string();

        let info = serde_json::json!({
            "title": "玩 家 群 体 鄙 视 链",
            "coverUrl": "https://img.acfun.cn/cover.jpeg",
            "durationMillis": 250466,
            "user": {"id": 17735808, "name": "Shi咪", "headUrl": "https://img.acfun.cn/head.jpg"},
            // ksPlayJson 在页面里是字符串, 不是对象
            "currentVideoInfo": {"ksPlayJson": ks, "ksPlayJsonHevc": hevc}
        });
        format!("<script>window.pageInfo = window.videoInfo = {info};</script>")
    }

    #[test]
    fn parses_current_page_structure() {
        let info = build(&page()).unwrap();
        assert_eq!(info.title, "玩 家 群 体 鄙 视 链");
        assert_eq!(info.author.name, "Shi咪");
        assert_eq!(info.author.uid, "17735808");
        assert!((info.duration - 250.466).abs() < 0.001);
        // 最清晰的一档当默认播放地址
        assert_eq!(info.video_url, "https://v.acfun.cn/hls_1080p60.m3u8");
        assert_eq!(info.height, 1080);
        // 剩下两档（720p 和 1080p H.265）进 formats
        assert_eq!(info.formats.len(), 2);
        assert!(info.formats.iter().any(|f| f.codec == "H.265"));
        assert!(info.formats.iter().all(|f| f.ext == "m3u8"));
    }

    #[test]
    fn description_field_is_not_mistaken_for_a_paywall() {
        // fansOnlyDesc 是简介，每条投稿都有；早先拿它当"粉丝专属"标记，
        // 结果所有视频都被误判成受限
        let mut info: serde_json::Value = serde_json::from_str(
            page()
                .split_once("videoInfo = ")
                .unwrap()
                .1
                .trim_end_matches(";</script>"),
        )
        .unwrap();
        info["fansOnlyDesc"] = serde_json::json!("看过太多不同玩家群体之间互相争执……");
        let html = format!("<script>window.videoInfo = {info};</script>");
        assert!(build(&html).is_ok(), "有简介的正常投稿必须能解析");
    }

    #[test]
    fn missing_play_json_is_restricted_not_parse() {
        // 页面结构对得上、只是没给地址，这是平台限制，不是解析器过期
        let html = r#"<script>window.videoInfo = {"title":"x","currentVideoInfo":{}};</script>"#;
        assert_eq!(build(html).unwrap_err().reason, crate::Reason::Restricted);
    }

    #[test]
    fn old_structure_is_a_parse_error() {
        let html = r#"<script>var videoInfo = {"title":"老结构"};var playInfo = {};</script>"#;
        assert_eq!(build(html).unwrap_err().reason, crate::Reason::Parse);
    }

    #[test]
    fn captcha_page_is_blocked() {
        assert_eq!(
            build("<html><body>请完成验证</body></html>")
                .unwrap_err()
                .reason,
            crate::Reason::Blocked
        );
    }
}
