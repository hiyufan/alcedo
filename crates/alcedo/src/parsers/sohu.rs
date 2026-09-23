//! 搜狐视频。
//!
//! 两种地址形态：`tv.sohu.com/v/<base64>.html`（base64 里装着真实路径）和
//! `my.tv.sohu.com/us/<uid>/<vid>.shtml`。

use base64::Engine;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Author, VideoInfo};
use crate::util;

/// 解析一条搜狐视频分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    parse_id(http, &vid_from_url(url)?).await
}

/// 已知搜狐视频作品 ID 时直接解析。
pub async fn parse_id(http: &Http, vid: &str) -> Result<VideoInfo> {
    let api = format!(
        "https://api.tv.sohu.com/v4/video/info/{vid}.json\
         ?site=2&api_key=9854b2afa779e1a6bcdd07b217417549&sver=6.2.0"
    );
    let json = http.get_json(&api).await?;

    let status = util::i64_at(&json, &["status"]);
    if status != 200 {
        return Err(Error::restricted(format!(
            "搜狐视频返回 status={status} {}",
            util::str_at(&json, &["statusText"])
        )));
    }

    let data = util::get(&json, &["data"]).ok_or_else(|| Error::deleted("没有视频数据"))?;

    // 优先高清，回退到下载地址
    let video_url = util::first_str(
        data,
        &[&["url_high_mp4"], &["download_url"], &["url_nor_mp4"]],
    );
    if video_url.is_empty() {
        return Err(Error::parse("接口没有返回播放地址"));
    }

    Ok(VideoInfo {
        video_url,
        cover_url: util::first_str(
            data,
            &[&["originalCutCover"], &["hor_big_pic"], &["ver_big_pic"]],
        ),
        title: util::str_at(data, &["video_name"]),
        duration: util::num_at(data, &["time_length"]),
        author: Author::new(
            util::str_at(data, &["user", "user_id"]),
            util::str_at(data, &["user", "nickname"]),
            util::str_at(data, &["user", "small_pic"]),
        ),
        ..Default::default()
    })
}

fn vid_from_url(url: &str) -> Result<String> {
    // tv.sohu.com/v/<base64>.html
    if let Some(rest) = url.split("/v/").nth(1) {
        if let Some(b64) = rest.strip_suffix(".html") {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64))
                .map_err(|_| Error::unsupported("链接里的 base64 段解不开"))?;
            let decoded = String::from_utf8_lossy(&decoded).into_owned();
            return vid_from_path(&decoded);
        }
    }
    vid_from_path(url)
}

/// `us/<uid>/<vid>.shtml` 里的 vid。
fn vid_from_path(path: &str) -> Result<String> {
    let at = path
        .find("/us/")
        .map(|i| i + 4)
        .or_else(|| path.starts_with("us/").then_some(3))
        .ok_or_else(|| Error::unsupported("不是有效的搜狐视频链接"))?;

    let rest = &path[at..];
    let vid = rest
        .split('/')
        .nth(1)
        .and_then(|s| s.split('.').next())
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| Error::unsupported(format!("无法从 {path} 里取出视频 ID")))?;
    Ok(vid.to_owned())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn extracts_vid_from_user_path() {
        assert_eq!(
            vid_from_url("https://my.tv.sohu.com/us/123456/9876543.shtml").unwrap(),
            "9876543"
        );
    }

    #[test]
    fn extracts_vid_from_base64_path() {
        let inner = "us/333/4455667.shtml";
        let b64 = base64::engine::general_purpose::STANDARD.encode(inner);
        let url = format!("https://tv.sohu.com/v/{b64}.html");
        assert_eq!(vid_from_url(&url).unwrap(), "4455667");
    }

    #[test]
    fn rejects_other_links() {
        assert!(vid_from_url("https://tv.sohu.com/").is_err());
    }
}
