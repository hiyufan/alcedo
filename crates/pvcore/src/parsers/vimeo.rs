//! Vimeo。
//!
//! 播放器的 `config` 接口把所有档位都摊开给你，不需要登录也不需要签名。

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Author, Format, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let id =
        video_id_from_url(url).ok_or_else(|| Error::unsupported("链接里没有 Vimeo 的视频 ID"))?;
    parse_id(http, &id).await
}

pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    if !id.bytes().all(|b| b.is_ascii_digit()) || id.is_empty() {
        return Err(Error::unsupported("Vimeo 的视频 ID 应当是一串数字"));
    }

    let api = format!("https://player.vimeo.com/video/{id}/config");
    let resp = http
        .send(crate::http::Req::get(&api).referer("https://vimeo.com/"))
        .await?;
    match resp.status.as_u16() {
        403 => return Err(Error::restricted("这条视频设了密码或限制了嵌入域名")),
        404 => return Err(Error::deleted("视频不存在或已删除")),
        _ => resp.error_for_status()?,
    }
    let json = resp.json()?;

    let video = util::get(&json, &["video"]).cloned().unwrap_or_default();

    // progressive 是可直接下载的 mp4，按高度降序
    let mut files: Vec<&serde_json::Value> =
        util::arr_at(&json, &["request", "files", "progressive"])
            .iter()
            .collect();
    files.sort_by_key(|f| std::cmp::Reverse(util::u32_at(f, &["height"])));

    let best = files.first().copied();
    let mut info = VideoInfo {
        video_url: best.map(|f| util::str_at(f, &["url"])).unwrap_or_default(),
        cover_url: best_thumbnail(&video),
        title: util::str_at(&video, &["title"]),
        duration: util::num_at(&video, &["duration"]),
        width: util::u32_at(&video, &["width"]),
        height: util::u32_at(&video, &["height"]),
        author: Author::new(
            util::first_id(&video, &[&["owner", "id"]]),
            util::str_at(&video, &["owner", "name"]),
            util::str_at(&video, &["owner", "img_2x"]),
        ),
        formats: files.iter().skip(1).filter_map(|f| to_format(f)).collect(),
        ..Default::default()
    };

    // 没有 progressive 的（较新的视频多半只有 DASH/HLS）退到 HLS
    if info.video_url.is_empty() {
        info.video_url = util::first_str(
            &json,
            &[
                &[
                    "request",
                    "files",
                    "hls",
                    "cdns",
                    "akfire_interconnect_quic",
                    "url",
                ],
                &["request", "files", "hls", "cdns", "fastly_skyfire", "url"],
            ],
        );
    }

    if info.is_empty() {
        return Err(Error::restricted(
            "Vimeo 没有下发可下载的地址（可能是付费或受保护的视频）",
        ));
    }
    Ok(info)
}

/// 一条 `progressive` 记录 → 一档清晰度。地址为空的跳过。
///
/// 单拎出来是因为内联在 `VideoInfo { .. }` 里会堆到七层嵌套
/// （结构体 → 迭代器 → 闭包 → 结构体 → 块 → if），改一行得先数括号。
fn to_format(f: &serde_json::Value) -> Option<Format> {
    let url = util::str_at(f, &["url"]);
    if url.is_empty() {
        return None;
    }
    let height = util::u32_at(f, &["height"]);
    Some(Format {
        label: Format::label_for(&util::str_at(f, &["quality"]), height, ""),
        url,
        ext: "mp4".into(),
        height,
        filesize: util::u64_at(f, &["size"]),
        ..Default::default()
    })
}

fn best_thumbnail(video: &serde_json::Value) -> String {
    // thumbs 是 {"640": "...", "1280": "...", "base": "..."}
    util::get(video, &["thumbs"])
        .and_then(|t| t.as_object())
        .and_then(|m| {
            m.iter()
                .filter(|(k, _)| k.bytes().all(|b| b.is_ascii_digit()))
                .max_by_key(|(k, _)| k.parse::<u32>().unwrap_or(0))
                .and_then(|(_, v)| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn video_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    // 路径里第一个纯数字段就是 id（/123456、/channels/x/123456、/video/123456）
    parsed
        .path_segments()?
        .find(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_id_from_url_shapes() {
        for url in [
            "https://vimeo.com/123456789",
            "https://vimeo.com/channels/staffpicks/123456789",
            "https://player.vimeo.com/video/123456789",
            "https://vimeo.com/123456789?share=copy",
        ] {
            assert_eq!(
                video_id_from_url(url).as_deref(),
                Some("123456789"),
                "{url}"
            );
        }
        assert_eq!(video_id_from_url("https://vimeo.com/someuser"), None);
    }

    #[test]
    fn picks_largest_thumbnail() {
        let video = json!({"thumbs": {"640": "https://t/640.jpg", "1280": "https://t/1280.jpg", "base": "https://t/base"}});
        assert_eq!(best_thumbnail(&video), "https://t/1280.jpg");
        assert_eq!(best_thumbnail(&json!({})), "");
    }
}
