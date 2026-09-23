//! 腾讯视频。
//!
//! `getinfo` 返回的是 JSONP（`QZOutputJson={...};`），剥掉包装再解析。

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::VideoInfo;
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    parse_id(http, &vid_from_url(url)?).await
}

pub async fn parse_id(http: &Http, vid: &str) -> Result<VideoInfo> {
    if vid.is_empty() {
        return Err(Error::unsupported("视频 ID 为空"));
    }

    let api =
        format!("https://vv.video.qq.com/getinfo?vids={vid}&platform=101001&otype=json&defn=shd");
    let resp = http
        .send(crate::http::Req::get(&api).referer("https://v.qq.com/"))
        .await?;
    resp.error_for_status()?;

    let body = resp.text();
    let json: serde_json::Value = serde_json::from_str(strip_jsonp(&body))?;

    let em = util::num_at(&json, &["em"]) as i64;
    if em != 0 {
        return Err(Error::restricted(format!(
            "腾讯视频返回 em={em} {}",
            util::str_at(&json, &["msg"])
        )));
    }

    let vi = util::arr_at(&json, &["vl", "vi"])
        .first()
        .ok_or_else(|| Error::deleted("未找到视频信息，可能已被删除或设为私密"))?;

    let base = util::str_at(vi, &["ul", "ui", "0", "url"]);
    let fn_ = util::str_at(vi, &["fn"]);
    let fvkey = util::str_at(vi, &["fvkey"]);
    if base.is_empty() || fn_.is_empty() || fvkey.is_empty() {
        return Err(Error::restricted(
            "腾讯视频没有下发完整的播放地址（这条通常要会员或登录）",
        ));
    }

    let real_vid = util::str_at(vi, &["vid"]);
    Ok(VideoInfo {
        video_url: format!("{base}{fn_}?vkey={fvkey}"),
        cover_url: if real_vid.is_empty() {
            String::new()
        } else {
            format!("https://puui.qpic.cn/vpic_cover/{real_vid}/{real_vid}_hz.jpg/496")
        },
        title: util::str_at(vi, &["ti"]),
        duration: util::num_at(vi, &["td"]),
        ..Default::default()
    })
}

/// 去掉 `QZOutputJson=` 前缀和末尾分号。
fn strip_jsonp(body: &str) -> &str {
    body.trim()
        .strip_prefix("QZOutputJson=")
        .unwrap_or(body.trim())
        .trim_end_matches(';')
        .trim()
}

fn vid_from_url(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("");

    // 移动端播放页: m.v.qq.com/x/m/play?vid={vid}
    if host.starts_with("m.") {
        if let Ok(v) = util::query_param(url, "vid") {
            return Ok(v);
        }
    }
    // PC 端: /x/page/{vid}.html 或 /x/cover/{cid}/{vid}.html
    let path = parsed.path();
    if let Some(rest) = path.split("/x/").nth(1) {
        if let Some(file) = rest.rsplit('/').next() {
            if let Some(vid) = file.strip_suffix(".html") {
                if !vid.is_empty() {
                    return Ok(vid.to_owned());
                }
            }
        }
    }
    // 兜底：任何地址上的 vid 参数
    util::query_param(url, "vid")
        .map_err(|_| Error::unsupported(format!("无法从 {path} 里取出腾讯视频 ID")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_jsonp_wrapper() {
        assert_eq!(strip_jsonp(r#"QZOutputJson={"a":1};"#), r#"{"a":1}"#);
        assert_eq!(strip_jsonp(r#"{"a":1}"#), r#"{"a":1}"#);
    }

    #[test]
    fn extracts_vid_from_page_urls() {
        assert_eq!(
            vid_from_url("https://v.qq.com/x/page/a0053ry6tp3.html").unwrap(),
            "a0053ry6tp3"
        );
        assert_eq!(
            vid_from_url("https://v.qq.com/x/cover/mzc00200/w3541z1ekoe.html").unwrap(),
            "w3541z1ekoe"
        );
        assert_eq!(
            vid_from_url("https://m.v.qq.com/x/m/play?vid=x0053abcdef").unwrap(),
            "x0053abcdef"
        );
    }

    #[test]
    fn rejects_non_video_links() {
        assert!(vid_from_url("https://v.qq.com/channel/movie").is_err());
    }
}
