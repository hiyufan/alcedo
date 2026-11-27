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

/// App 分享出来的 `mapp.api.weibo.cn/fx/<编号>.html`，302 到 `m.weibo.cn/status/<mid>`。
const APP_SHARE_HOST: &str = "mapp.api.weibo.cn";
const LIVE_UNSUPPORTED: &str = "微博直播和直播回放暂不支持解析";

/// 解析一条微博分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    if util::host_of(url).as_deref() == Some(APP_SHARE_HOST) {
        let target = http.resolve_redirect(url).await?;
        if matches!(
            util::host_of(&target).as_deref(),
            None | Some(APP_SHARE_HOST)
        ) {
            return Err(Error::unsupported("这条微博分享链接没有跳到微博内容"));
        }
        return Box::pin(parse(http, &target)).await;
    }
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

    let post_id = post_id_from_url(url)?;
    parse_post(http, &post_id, url).await
}

/// 从普通博文地址里取 mblogid / mid。
fn post_id_from_url(url: &str) -> Result<String> {
    let path = url::Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    if path.starts_with("/l/wblive/") {
        return Err(Error::unsupported(LIVE_UNSUPPORTED));
    }
    // t.cn 短链有时落到 weibo.com/p/230444<微博 mid>，是这条微博的卡片页
    if let Some(mid) = path
        .trim_end_matches('/')
        .strip_prefix("/p/230444")
        .filter(|m| m.len() == 16 && m.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(mid.to_owned());
    }
    // 普通博文：weibo.com/<用户 ID>/<微博 ID>；有的分享会在 ID 后面粘上别的字符，只取开头那段
    let last = util::last_path_segment(url).unwrap_or_default();
    let id: String = last
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    if id.is_empty() {
        return Err(Error::unsupported("不认识这个微博链接的格式"));
    }
    Ok(id)
}

/// 直播卡片的 stream_url 是播放页而不是视频文件，回放只有分段直播流，都不能当视频给出去。
fn is_live(status: &Value) -> bool {
    util::str_at(status, &["page_info", "object_type"]) == "live"
}

/// 只有直播、没有配图的微博：明确说不支持，免得报成"没有视频或图片"让人以为被删了。
fn live_without_pictures(status: &Value) -> bool {
    is_live(status)
        && util::arr_at(status, &["pics"]).is_empty()
        && util::arr_at(status, &["pic_ids"]).is_empty()
}

/// 微博视频（oid 形式）。
/// 已知微博作品 ID 时直接解析。
pub async fn parse_id(http: &Http, video_id: &str) -> Result<VideoInfo> {
    let api = format!("https://h5.video.weibo.com/api/component?page=/show/{video_id}");
    let body = format!(r#"data={{"Component_Play_Playinfo":{{"oid":"{video_id}"}}}}"#);

    let resp = http
        .send(
            // 组件接口是只读查询，只是做成了 POST
            Req::post(&api)
                .referer(format!("https://h5.video.weibo.com/show/{video_id}"))
                .form_body(body)
                .idempotent(),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let data = util::get(&json, &["data", "Component_Play_Playinfo"])
        .ok_or_else(|| Error::deleted("微博没有返回播放信息"))?;

    // stream_url 码率最低；urls 里按已知清晰度键从高到低挑。
    // urls 是个 map，serde_json 未开 preserve_order 时按键字母序迭代，
    // "第一条"并不代表最高清——{mp4_480p_mp4, mp4_hd_mp4} 会先碰到 480p。
    let mut video_url = util::str_at(data, &["stream_url"]);
    if let Some(obj) = util::get(data, &["urls"]).and_then(Value::as_object) {
        if let Some(u) = best_of_urls(obj) {
            video_url = with_scheme(u);
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
                    if live_without_pictures(data) {
                        return Err(Error::unsupported(LIVE_UNSUPPORTED));
                    }
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
    if live_without_pictures(status) {
        return Err(Error::unsupported(LIVE_UNSUPPORTED));
    }

    let info = build_status(status);
    if info.is_empty() {
        return Err(Error::deleted("这条微博里没有视频或图片"));
    }
    Ok(info)
}

fn build_status(status: &Value) -> VideoInfo {
    let user = util::get_or_null(status, &["user"]);

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
            (!u.is_empty()).then(|| Image {
                live_photo_url: live_video(pic),
                ..Image::new(u)
            })
        })
        .collect::<Vec<_>>();

    // 博文里内嵌的视频；直播卡片的地址不是视频文件，不要
    let media = util::get_or_null(status, &["page_info"]);
    let video_url = if is_live(status) {
        String::new()
    } else {
        util::first_str(
            media,
            &[
                &["urls", "mp4_720p_mp4"],
                &["urls", "mp4_hd_url"],
                &["urls", "mp4_ld_mp4"],
                &["media_info", "stream_url_hd"],
                &["media_info", "stream_url"],
            ],
        )
    };

    VideoInfo {
        video_url: with_scheme(&video_url),
        cover_url: with_scheme(&util::first_str(
            media,
            &[&["page_pic", "url"], &["page_pic"]],
        )),
        title: util::strip_tags(&util::str_at(status, &["text"])),
        images,
        duration: util::num_at(media, &["media_info", "duration"]),
        author: Author::new(
            util::id_at(user, &["id"]),
            util::str_at(user, &["screen_name"]),
            util::first_str(user, &[&["avatar_large"], &["profile_image_url"]]),
        ),
        ..Default::default()
    }
}

/// 实况图的那段视频。手机接口叫 `videoSrc`，桌面页的数据叫 `video`，都是
/// `video.weibo.com/media/play?livephoto=…` 的跳转地址：跳过去的 livephoto.us.sinaimg.cn
/// 才给文件，直接拼 `us.sinaimg.cn/<id>.mov` 会 403。`video_hd` 里的文件名可能是空的，不用。
fn live_video(pic: &Value) -> String {
    if util::str_at(pic, &["type"]) != "livephoto" {
        return String::new();
    }
    with_scheme(&util::first_str(pic, &[&["videoSrc"], &["video"]]))
}

/// 微博很多地址是 `//host/path` 形式。
fn with_scheme(u: &str) -> String {
    if u.starts_with("//") {
        format!("https:{u}")
    } else {
        u.to_owned()
    }
}

/// Component_Play_Playinfo 的 `urls` map 里挑清晰度最高的地址。
///
/// 已知键从高到低显式挑；都不认识时落回 map 的第一条（字母序，老行为），
/// 平台上新清晰度档位时不至于选空。
fn best_of_urls(urls: &serde_json::Map<String, Value>) -> Option<&str> {
    ["mp4_1080p_mp4", "mp4_720p_mp4", "mp4_hd_mp4"]
        .iter()
        .find_map(|k| urls.get(*k).and_then(Value::as_str))
        .or_else(|| urls.values().find_map(Value::as_str))
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
    fn urls_pick_highest_known_tier_not_alphabetical_first() {
        let to_map = |v: serde_json::Value| v.as_object().unwrap().clone();
        // 字母序 480p 排在 hd 前面，按清晰度该选 hd
        let urls = to_map(json!({"mp4_480p_mp4": "//v/480.mp4", "mp4_hd_mp4": "//v/hd.mp4"}));
        assert_eq!(best_of_urls(&urls), Some("//v/hd.mp4"));

        let urls = to_map(json!({"mp4_720p_mp4": "//v/720.mp4", "mp4_1080p_mp4": "//v/1080.mp4"}));
        assert_eq!(best_of_urls(&urls), Some("//v/1080.mp4"));

        // 全是不认识的键：落回第一条，别选空
        let urls = to_map(json!({"m3u8_auto": "//v/auto.m3u8", "zzz": "//v/z.mp4"}));
        assert_eq!(best_of_urls(&urls), Some("//v/auto.m3u8"));
    }

    #[test]
    fn empty_status_is_empty() {
        assert!(build_status(&json!({"text": "纯文字"})).is_empty());
    }

    #[test]
    fn post_ids_from_every_url_shape() {
        let cases = [
            ("https://weibo.com/1234567890/PxYzAbCdE", "PxYzAbCdE"),
            // 分享时 ID 后面粘了别的字符
            (
                "https://weibo.com/1234567890/PxYzAbCdE%E2%80%8B",
                "PxYzAbCdE",
            ),
            (
                "https://m.weibo.cn/status/5223456789012345",
                "5223456789012345",
            ),
            // t.cn 落地的卡片页
            (
                "https://weibo.com/p/2304445223456789012345",
                "5223456789012345",
            ),
            (
                "https://weibo.com/p/2304445223456789012345/",
                "5223456789012345",
            ),
        ];
        for (url, want) in cases {
            assert_eq!(post_id_from_url(url).unwrap(), want, "{url}");
        }
    }

    #[test]
    fn live_links_are_unsupported() {
        let err =
            post_id_from_url("https://weibo.com/l/wblive/p/show/1022:2321324999").unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
        assert!(err.to_string().contains("直播"));
    }

    #[test]
    fn live_card_is_not_a_video() {
        let status = json!({
            "text": "直播中",
            "page_info": { "object_type": "live",
                           "media_info": { "stream_url": "https://weibo.com/l/wblive/p/show/1" } }
        });
        assert!(live_without_pictures(&status));
        assert!(build_status(&status).video_url.is_empty());

        // 带配图的直播微博：图照给，直播地址不给
        let with_pics = json!({
            "pics": [{ "large": { "url": "https://wx1.sinaimg.cn/large/a.jpg" } }],
            "page_info": { "object_type": "live",
                           "media_info": { "stream_url": "https://weibo.com/l/wblive/p/show/1" } }
        });
        assert!(!live_without_pictures(&with_pics));
        let info = build_status(&with_pics);
        assert!(info.video_url.is_empty());
        assert_eq!(info.images.len(), 1);
    }

    #[test]
    fn live_photos_keep_their_clip() {
        let status = json!({
            "pics": [
                { "type": "livephoto",
                  "large": { "url": "https://wx1.sinaimg.cn/large/live.jpg" },
                  "videoSrc": "https://video.weibo.com/media/play?livephoto=https%3A%2F%2Flivephoto.us.sinaimg.cn%2Fa.mov" },
                { "type": "pic", "large": { "url": "https://wx1.sinaimg.cn/large/still.jpg" },
                  "videoSrc": "https://ignored" }
            ]
        });
        let info = build_status(&status);
        assert_eq!(info.images.len(), 2);
        assert!(info.images[0]
            .live_photo_url
            .starts_with("https://video.weibo.com/media/play?livephoto="));
        assert!(
            info.images[1].live_photo_url.is_empty(),
            "普通图片不能带实况视频"
        );
    }
}
