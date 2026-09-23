//! 央视网。
//!
//! 页面里嵌着一个 `guid`，拿它去换 HLS 地址。
//!
//! 注意别用 manifest 里的 `h5e/enc/enc2` 高码率流：那几条在 H.264 帧级做了加扰，
//! 播出来一片花屏。只有 `hls_url` 是能正常播的。

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Author, VideoInfo};
use crate::util;

/// 解析一条央视网分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let html = http.get_text(url).await?;
    let guid = guid_from_html(&html)?;
    parse_id(http, &guid).await
}

/// 已知央视网作品 ID 时直接解析。
pub async fn parse_id(http: &Http, guid: &str) -> Result<VideoInfo> {
    let json = http
        .get_json(&format!(
            "https://vdn.apps.cntv.cn/api/getHttpVideoInfo.do?pid={guid}"
        ))
        .await?;

    let status = util::str_at(&json, &["status"]);
    if status != "001" {
        return Err(Error::restricted(format!(
            "央视网返回 status={status} title={}",
            util::str_at(&json, &["title"])
        )));
    }

    let video_url = util::str_at(&json, &["hls_url"]);
    if video_url.is_empty() {
        return Err(Error::parse("接口没有返回 hls_url"));
    }

    Ok(VideoInfo {
        video_url,
        cover_url: util::str_at(&json, &["image"]),
        title: util::str_at(&json, &["title"]),
        duration: util::num_at(&json, &["video", "totalLength"]),
        author: Author::named(util::str_at(&json, &["play_channel"])),
        ..Default::default()
    })
}

fn guid_from_html(html: &str) -> Result<String> {
    // var guid = "xxx";
    let at = html
        .find("var guid")
        .ok_or_else(|| Error::parse("页面里没有 guid"))?;
    let rest = &html[at..];
    let start = rest
        .find('"')
        .ok_or_else(|| Error::parse("guid 格式不对"))?
        + 1;
    let end = rest[start..]
        .find('"')
        .ok_or_else(|| Error::parse("guid 没有闭合引号"))?
        + start;
    let guid = rest[start..end].trim();
    if guid.is_empty() {
        return Err(Error::parse("guid 是空的"));
    }
    Ok(guid.to_owned())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn extracts_guid() {
        let html = r#"<script>var guid = "a1b2c3d4e5";var other=1;</script>"#;
        assert_eq!(guid_from_html(html).unwrap(), "a1b2c3d4e5");
    }

    #[test]
    fn missing_guid_is_parse_error() {
        assert_eq!(
            guid_from_html("<html>nothing</html>").unwrap_err().reason,
            crate::Reason::Parse
        );
    }
}
