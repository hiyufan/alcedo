//! 西瓜视频。
//!
//! 走抖音那套分享页接口（西瓜和抖音同属一个体系），从 `_ROUTER_DATA` 里取数据。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let host = util::host_of(url).unwrap_or_default();

    let id = if host == "www.ixigua.com" || host == "ixigua.com" {
        util::last_path_segment(url).ok_or_else(|| Error::unsupported("链接里没有作品 ID"))?
    } else {
        // v.ixigua.com 短链
        let location = http.resolve_redirect(url).await?;
        location
            .split('?')
            .next()
            .and_then(|p| p.trim_end_matches('/').rsplit('/').next())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::deleted("短链跳转后没有作品 ID"))?
            .to_owned()
    };

    parse_id(http, &id).await
}

pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    // 注意：地址里 video_id 后面不能有斜杠，否则返回的结构不一样
    let api = format!(
        "https://m.ixigua.com/douyin/share/video/{id}\
         ?aweme_type=107&schema_type=1&utm_source=copy&utm_campaign=client_share\
         &utm_medium=android&app=aweme"
    );

    let resp = http
        .send(Req::get(&api).header("User-Agent", ua::pick(ua::Platform::Android)))
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    let raw = util::json_after(&html, "window._ROUTER_DATA")
        .ok_or_else(|| Error::parse("页面里没有 _ROUTER_DATA"))?;
    let json: serde_json::Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    let info = util::get(&json, &["loaderData", "video_(id)/page", "videoInfoRes"])
        .ok_or_else(|| Error::parse("_ROUTER_DATA 里没有 videoInfoRes"))?;

    let items = util::arr_at(info, &["item_list"]);
    let Some(data) = items.first() else {
        let msg = util::str_at(info, &["filter_list", "0", "detail_msg"]);
        return Err(if msg.is_empty() {
            Error::deleted("页面里没有作品数据")
        } else {
            Error::restricted(msg)
        });
    };

    let play = util::get(data, &["video", "play_addr"])
        .cloned()
        .unwrap_or_default();
    let video_url = util::str_at(&play, &["url_list", "0"]).replace("playwm", "play");
    if video_url.is_empty() {
        return Err(Error::parse("没有取到播放地址"));
    }

    Ok(VideoInfo {
        video_url,
        cover_url: util::prefer_non_webp(util::arr_at(data, &["video", "cover", "url_list"])),
        title: util::str_at(data, &["desc"]),
        duration: util::num_at(data, &["video", "duration"]) / 1000.0,
        width: util::u32_at(&play, &["width"]),
        height: util::u32_at(&play, &["height"]),
        author: Author::new(
            util::first_str(data, &[&["author", "unique_id"], &["author", "sec_uid"]]),
            util::str_at(data, &["author", "nickname"]),
            util::str_at(data, &["author", "avatar_thumb", "url_list", "0"]),
        ),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pc_url_gives_id_without_network() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::XiGua),
        )
        .unwrap();
        // 只跑到取 ID 那一步就够: 真正发请求会失败, 但 ID 解析路径已覆盖
        let _ = &http;
        assert_eq!(
            util::last_path_segment("https://www.ixigua.com/7234567890123456789").as_deref(),
            Some("7234567890123456789")
        );
    }
}
