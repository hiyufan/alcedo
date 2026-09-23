//! 新片场。
//!
//! 页面只给 `appKey` + `media_id`，真正的 mp4 地址要再问一次 mod-api。

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

/// 解析一条新片场分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let http = http.clone().with_ua(ua::Platform::Desktop);

    let resp = http
        .send(
            Req::get(url)
                .referer("https://www.xinpianchang.com/")
                .header("Upgrade-Insecure-Requests", "1"),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    let next = util::script_by_id(&html, "__NEXT_DATA__")
        .ok_or_else(|| Error::parse("页面里没有 __NEXT_DATA__"))?;
    let json: serde_json::Value = serde_json::from_str(next)?;

    let detail = util::get(&json, &["props", "pageProps", "detail"])
        .ok_or_else(|| Error::deleted("页面数据里没有作品详情"))?;

    let app_key = util::str_at(detail, &["video", "appKey"]);
    let media_id = util::first_id(detail, &[&["media_id"], &["id"]]);
    if app_key.is_empty() || media_id.is_empty() {
        return Err(Error::parse("缺少 appKey 或 media_id"));
    }

    let api = format!(
        "https://mod-api.xinpianchang.com/mod/api/v2/media/{media_id}\
         ?appKey={app_key}&extend=userInfo%2CuserStatus"
    );
    let media = http.get_json(&api).await?;

    let progressive = util::arr_at(&media, &["data", "resource", "progressive"]);
    let best = progressive
        .first()
        .ok_or_else(|| Error::restricted("新片场没有下发可下载的地址"))?;

    // 其余档位
    let formats = progressive
        .iter()
        .skip(1)
        .filter_map(|p| {
            let u = util::str_at(p, &["url"]);
            (!u.is_empty()).then(|| Format {
                label: util::first_str(p, &[&["name"], &["quality"]]),
                url: u,
                ext: "mp4".into(),
                height: util::u32_at(p, &["height"]),
                filesize: util::u64_at(p, &["filesize"]),
                ..Default::default()
            })
        })
        .collect();

    Ok(VideoInfo {
        video_url: util::str_at(best, &["url"]),
        cover_url: util::str_at(detail, &["cover"]),
        title: util::str_at(detail, &["title"]),
        duration: util::num_at(detail, &["duration"]),
        width: util::u32_at(best, &["width"]),
        height: util::u32_at(best, &["height"]),
        formats,
        author: Author::new(
            util::id_at(detail, &["author", "userinfo", "id"]),
            util::str_at(detail, &["author", "userinfo", "username"]),
            util::str_at(detail, &["author", "userinfo", "avatar"]),
        ),
        ..Default::default()
    })
}
