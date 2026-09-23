//! 微博。
//!
//! 三种链接形态：
//! - `show?fid=<id>` / `/tv/show/<id>`：微博视频，走 h5 组件接口
//! - `weibo.com/<uid>/<mblogid>`：普通博文，可能是图集，走 m 站接口

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::util;

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    if url.contains("show?fid=") {
        return parse_id(http, &util::query_param(url, "fid")?).await;
    }
    if let Some(rest) = url.split("/tv/show/").nth(1) {
        let id = rest
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if !id.is_empty() {
            return parse_id(http, id).await;
        }
    }

    // 普通博文：取路径最后一段当 mblogid
    let post_id = util::last_path_segment(url)
        .ok_or_else(|| Error::unsupported("不认识这个微博链接的格式"))?;
    parse_post(http, &post_id, url).await
}

/// 微博视频（oid 形式）。
pub async fn parse_id(http: &Http, video_id: &str) -> Result<VideoInfo> {
    let api = format!("https://h5.video.weibo.com/api/component?page=/show/{video_id}");
    let body = format!(r#"data={{"Component_Play_Playinfo":{{"oid":"{video_id}"}}}}"#);

    let resp = http
        .send(
            Req::post(&api)
                .referer(format!("https://h5.video.weibo.com/show/{video_id}"))
                .form_body(body),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let data = util::get(&json, &["data", "Component_Play_Playinfo"])
        .ok_or_else(|| Error::deleted("微博没有返回播放信息"))?;

    // stream_url 码率最低；urls 里第一条最高
    let mut video_url = util::str_at(data, &["stream_url"]);
    if let Some(obj) = util::get(data, &["urls"]).and_then(Value::as_object) {
        if let Some((_, v)) = obj.iter().next() {
            if let Some(u) = v.as_str() {
                video_url = with_scheme(u);
            }
        }
    }

    Ok(VideoInfo {
        video_url,
        cover_url: with_scheme(&util::str_at(data, &["cover_image"])),
        title: util::str_at(data, &["title"]),
        duration: util::num_at(data, &["duration"]),
        author: Author::new(
            util::id_at(data, &["user", "id"]),
            util::str_at(data, &["author"]),
            with_scheme(&util::str_at(data, &["avatar"])),
        ),
        ..Default::default()
    })
}

/// 普通博文（可能是图集）。
async fn parse_post(http: &Http, post_id: &str, original_url: &str) -> Result<VideoInfo> {
    // 先试 m 站接口，它直接给结构化数据
    let api = format!("https://m.weibo.cn/statuses/show?id={post_id}");
    let mobile = http
        .send(
            Req::get(&api)
                .referer("https://m.weibo.cn/")
                .header("X-Requested-With", "XMLHttpRequest"),
        )
        .await;

    if let Ok(resp) = mobile {
        if resp.status.is_success() {
            if let Ok(json) = resp.json() {
                if let Some(data) = util::get(&json, &["data"]) {
                    let info = build_status(data);
                    if !info.is_empty() {
                        return Ok(info);
                    }
                }
            }
        }
    }

    // 退回去啃桌面页里的 $render_data
    let html = http.get_text(original_url).await?;
    let raw = util::json_after(&html, "$render_data")
        .ok_or_else(|| Error::parse("页面里没有 $render_data"))?;
    let json: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    // $render_data 是个数组，取第一项
    let first = json.get(0).unwrap_or(&json);
    let status = util::get(first, &["status"]).unwrap_or(first);

    let info = build_status(status);
    if info.is_empty() {
        return Err(Error::deleted("这条微博里没有视频或图片"));
    }
    Ok(info)
}

fn build_status(status: &Value) -> VideoInfo {
    let user = util::get(status, &["user"]).cloned().unwrap_or_default();

    let images = util::arr_at(status, &["pics"])
        .iter()
        .filter_map(|pic| {
            // 挑最大的那张
            let u = util::first_str(
                pic,
                &[
                    &["large", "url"],
                    &["original", "url"],
                    &["bmiddle", "url"],
                    &["url"],
                ],
            );
            (!u.is_empty()).then(|| Image::new(u))
        })
        .collect::<Vec<_>>();

    // 博文里内嵌的视频
    let media = util::get(status, &["page_info"])
        .cloned()
        .unwrap_or_default();
    let video_url = util::first_str(
        &media,
        &[
            &["urls", "mp4_720p_mp4"],
            &["urls", "mp4_hd_url"],
            &["urls", "mp4_ld_mp4"],
            &["media_info", "stream_url_hd"],
            &["media_info", "stream_url"],
        ],
    );

    VideoInfo {
        video_url: with_scheme(&video_url),
        cover_url: with_scheme(&util::first_str(
            &media,
            &[&["page_pic", "url"], &["page_pic"]],
        )),
        title: util::strip_tags(&util::str_at(status, &["text"])),
        images,
        duration: util::num_at(&media, &["media_info", "duration"]),
        author: Author::new(
            util::id_at(&user, &["id"]),
            util::str_at(&user, &["screen_name"]),
            util::first_str(&user, &[&["avatar_large"], &["profile_image_url"]]),
        ),
        ..Default::default()
    }
}

/// 微博很多地址是 `//host/path` 形式。
fn with_scheme(u: &str) -> String {
    if u.starts_with("//") {
        format!("https:{u}")
    } else {
        u.to_owned()
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_image_post() {
        let status = json!({
            "text": "看看这个 <a href=\"x\">链接</a>",
            "user": {"id": 123, "screen_name": "某人", "avatar_large": "https://a/av.jpg"},
            "pics": [
                {"large": {"url": "https://wx.sinaimg.cn/large/1.jpg"}, "url": "https://small/1.jpg"},
                {"url": "https://wx.sinaimg.cn/2.jpg"}
            ]
        });
        let info = build_status(&status);
        assert_eq!(info.images.len(), 2);
        assert!(info.images[0].url.contains("/large/"), "要挑最大的那张");
        assert_eq!(info.title, "看看这个 链接");
        assert_eq!(info.author.uid, "123");
    }

    #[test]
    fn builds_video_post_preferring_hd() {
        let status = json!({
            "text": "视频",
            "page_info": {
                "urls": {"mp4_ld_mp4": "//v/ld.mp4", "mp4_720p_mp4": "//v/720.mp4"},
                "page_pic": {"url": "//p/cover.jpg"},
                "media_info": {"duration": 30}
            }
        });
        let info = build_status(&status);
        assert_eq!(info.video_url, "https://v/720.mp4");
        assert_eq!(info.cover_url, "https://p/cover.jpg");
        assert_eq!(info.duration, 30.0);
    }

    #[test]
    fn scheme_relative_urls_are_fixed() {
        assert_eq!(with_scheme("//a/b"), "https://a/b");
        assert_eq!(with_scheme("https://a/b"), "https://a/b");
        assert_eq!(with_scheme(""), "");
    }

    #[test]
    fn empty_status_is_empty() {
        assert!(build_status(&json!({"text": "纯文字"})).is_empty());
    }
}
