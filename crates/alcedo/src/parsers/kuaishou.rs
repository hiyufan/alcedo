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

/// 解析一条快手分享链接。
///
/// 三类链接：
/// - App 分享的 `v.kuaishou.com` 短链：不跟跳转拿 `Location` 和第一份 cookie
/// - 网页版 / 落地页链接（`www.kuaishou.com/short-video/<id>`、`live.kuaishou.com/u/<用户>/<id>`、
///   `/fw/photo/<id>`……）：路径里就有作品 ID，直接请求手机落地页，不带分享参数和 cookie 也能拿到
/// - 网页版分享的 `www.kuaishou.com/f/<码>` 短链：跟着跳转找作品 ID
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let host = util::host_of(url).unwrap_or_default();
    // chenzhongtech 上也有不带 ID、要跟跳转的分享短链，和 v.kuaishou.com 一样处理
    let app_short_link = host == "v.kuaishou.com"
        || (host.ends_with("chenzhongtech.com") && photo_id_in(url).is_none());
    if !app_short_link {
        let id = photo_id(http, url).await?;
        return landing(http, &format!("https://c.kuaishou.com/fw/photo/{id}"), &[]).await;
    }

    // 第一跳：拿 Location + cookie
    let first = http
        .send(Req::get(url).referer("https://v.kuaishou.com/").no_follow())
        .await?;

    let location = first
        .location()
        .ok_or_else(|| Error::deleted("快手短链没有返回跳转地址"))?;
    // /fw/long-video/ 返回的结构和普通作品不一样，统一换成 /fw/photo/
    let location = location.replace("/fw/long-video/", "/fw/photo/");
    landing(http, &location, &first.set_cookies()).await
}

/// 请求落地页，从 `window.INIT_STATE` 里取数据。
///
/// cookie 必须传下去——快手拿它认会话，少了这一步落地页会返回空壳。落地页偶尔
/// （实测约 1/40）回一个连 INIT_STATE 都没有的空页面，马上再要一次就正常；
/// 验证页不重试，那是真被限流了。
async fn landing(http: &Http, location: &str, cookies: &[(String, String)]) -> Result<VideoInfo> {
    let mut last = Error::parse("快手落地页没有作品数据");
    for _ in 0..2 {
        let mut req = Req::get(location).referer("https://v.kuaishou.com/");
        for (k, v) in cookies {
            req = req.cookie(k, v);
        }
        let resp = http.send(req).await?;
        resp.error_for_status()?;
        let html = resp.text();

        if let Some(raw) = util::json_after(&html, "window.INIT_STATE") {
            let json: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;
            return build(&json);
        }
        last = block_error(&html);
        if last.reason == crate::Reason::Blocked {
            break;
        }
    }
    Err(last)
}

/// 网页版 / 落地页链接里的作品 ID 所在的路径前缀。`u/` 和 `profile/` 后面先跟一段用户名。
const ID_PREFIXES: [&str; 3] = ["short-video", "fw/photo", "fw/long-video"];
const USER_PREFIXES: [&str; 2] = ["u", "profile"];
/// 网页版 `/f/` 短链最多跟几跳。
const MAX_HOPS: usize = 4;

/// 网页版 / 落地页链接里的作品 ID；`www.kuaishou.com/f/` 短链跟着跳转找。
///
/// 主页这类本来就不是作品的链接不跟，直接说不支持；短链跳回首页的是失效了。
async fn photo_id(http: &Http, url: &str) -> Result<String> {
    if let Some(id) = photo_id_in(url) {
        return Ok(id);
    }
    if !is_web_short_link(url) {
        return Err(Error::unsupported(
            "快手链接里没有作品 ID，请用 App 里「分享 → 复制链接」的链接",
        ));
    }
    let mut current = url.to_owned();
    for _ in 0..MAX_HOPS {
        let Some(next) = http
            .send(
                Req::get(&current)
                    .referer("https://www.kuaishou.com/")
                    .no_follow(),
            )
            .await?
            .location()
        else {
            break;
        };
        current = next;
        if let Some(id) = photo_id_in(&current) {
            return Ok(id);
        }
        if is_home(&current) {
            return Err(Error::deleted("快手短链已失效，跳回了首页"));
        }
    }
    Err(Error::parse(format!(
        "快手短链没有跳到作品页（停在 {}）",
        current.chars().take(80).collect::<String>()
    )))
}

/// 路径里的作品 ID：`/short-video/<id>`、`/fw/photo/<id>`、`/fw/long-video/<id>`、
/// `/u/<用户>/<id>`、`/profile/<用户>/<id>`。ID 是 10 位以上的字母数字。
fn photo_id_in(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let looks_like_id = |s: &str| s.len() >= 10 && s.bytes().all(|b| b.is_ascii_alphanumeric());
    for (i, seg) in segs.iter().enumerate() {
        let pair = segs.get(i + 1).map(|next| format!("{seg}/{next}"));
        let after = if ID_PREFIXES.contains(seg) {
            segs.get(i + 1)
        } else if pair.as_deref().is_some_and(|p| ID_PREFIXES.contains(&p))
            || USER_PREFIXES.contains(seg)
        {
            segs.get(i + 2)
        } else {
            None
        };
        if let Some(id) = after.copied().filter(|s| looks_like_id(s)) {
            return Some(id.to_owned());
        }
    }
    None
}

fn is_web_short_link(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.host_str().is_some_and(|h| h.ends_with("kuaishou.com"))
            && u.path().starts_with("/f/")
            && u.path().len() > "/f/".len()
    })
}

/// 快手首页 / 推荐页：短链失效时会跳回这里。
fn is_home(url: &str) -> bool {
    url::Url::parse(url)
        .is_ok_and(|u| matches!(u.path().trim_matches('/'), "" | "new-reco" | "brilliant"))
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
    let atlas = util::get_or_null(photo, &["ext_params", "atlas"]);
    let cdns = util::arr_at(atlas, &["cdn"]);
    let list = util::arr_at(atlas, &["list"]);
    let mut images = Vec::new();
    if let Some(cdn) = cdns.first().and_then(Value::as_str) {
        for item in list {
            if let Some(path) = item.as_str() {
                images.push(Image::new(atlas_image(cdn, path)));
            }
        }
    }

    // 单图作品（photoType SINGLE_PICTURE）：没有视频也没有 atlas，那张图就是封面——
    // /upic/ 下用户传的原图，和作品宽高一致。以前这种作品报"没有拿到任何视频或图片"
    let single = images.is_empty()
        && video_url.is_empty()
        && (util::get(photo, &["singlePicture"]).and_then(Value::as_bool) == Some(true)
            || util::str_at(photo, &["photoType"]) == "SINGLE_PICTURE");
    if single {
        let cover = util::first_str(photo, &[&["coverUrls", "0", "url"], &["coverUrl"]]);
        if !cover.is_empty() {
            images.push(Image::new(cover.replacen("http://", "https://", 1)));
        }
    }
    // 图集 / 单图的配乐：ext_params 里给的是相对路径 + musicCdnList
    let music_url = {
        let from_atlas = ext_music(atlas);
        if from_atlas.is_empty() {
            ext_music(util::get_or_null(photo, &["ext_params", "single"]))
        } else {
            from_atlas
        }
    };

    let formats = manifest_formats(photo, &video_url);

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
        notice: if single && is_live_caption(&util::str_at(photo, &["caption"])) {
            LIVE_NOTICE.to_owned()
        } else {
            String::new()
        },
        title: util::str_at(photo, &["caption"]),
        music_url,
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

/// 备用清晰度：有的作品会在 manifest 里给多档，跳过和默认档重复的那条。
fn manifest_formats(photo: &Value, video_url: &str) -> Vec<Format> {
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
        // 同一清晰度常给 avc / hevc 两份，不标出来就是两个一模一样的"720p"
        let codec = if util::str_at(rep, &["videoCodec"]) == "hevc" {
            "H.265"
        } else {
            ""
        };
        formats.push(Format {
            label: Format::label_for(&quality, short, codec),
            codec: codec.to_owned(),
            url: u,
            ext: "mp4".into(),
            height: short,
            filesize: util::u64_at(rep, &["fileSize"]),
            ..Default::default()
        });
    }
    formats
}

/// 快手实况：分享页数据里只有照片和配乐（字段和普通单图作品一模一样），会动的那段
/// 分享页根本不放，网页版接口又要过滑块验证码（求原图 2026-10 实测）。只能从标题里的
/// 话题认出来，提醒用户一句，免得以为是漏了。
const LIVE_NOTICE: &str =
    "这是快手实况：快手只对外提供照片和配乐，会动的部分拿不到。要完整的实况，请在快手 App 里保存。";

fn is_live_caption(caption: &str) -> bool {
    caption.contains("实况") || caption.to_ascii_lowercase().contains("live")
}

/// 图集的一张。路径自带前导 `/`，直接拼会多出一个斜杠；`atlas.list` 给的是 `.webp`，
/// 同一路径的 `.jpg` 也有、分辨率一样（2026-10 实测 11 张图集张张都有）——换成 jpg，
/// 存下来哪儿都能打开。
fn atlas_image(cdn: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    let path = path
        .strip_suffix(".webp")
        .map_or_else(|| path.to_owned(), |p| format!("{p}.jpg"));
    format!("https://{cdn}/{path}")
}

/// 配乐地址：`music` 是相对路径，主机在 `musicCdnList[0].cdn`。
fn ext_music(block: &Value) -> String {
    let path = util::str_at(block, &["music"]);
    let cdn = util::str_at(block, &["musicCdnList", "0", "cdn"]);
    if path.is_empty() || cdn.is_empty() {
        return String::new();
    }
    format!("https://{cdn}/{}", path.trim_start_matches('/'))
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
    fn photo_id_from_web_and_landing_links() {
        let id = "3xk8vw5ahmc3a2e";
        for url in [
            format!("https://www.kuaishou.com/short-video/{id}?authorId=x"),
            format!("https://live.kuaishou.com/u/3xuser123/{id}"),
            format!("https://c.kuaishou.com/fw/photo/{id}?fid=1"),
            format!("https://v.m.chenzhongtech.com/fw/long-video/{id}"),
            format!("https://m.gifshow.com/fw/photo/{id}"),
            format!("https://www.kuaishou.com/profile/3xuser123/{id}"),
        ] {
            assert_eq!(photo_id_in(&url).as_deref(), Some(id), "{url}");
        }
        // 主页、用户页没有作品 ID，别把用户名当 ID
        assert_eq!(photo_id_in("https://www.kuaishou.com/"), None);
        assert_eq!(
            photo_id_in("https://www.kuaishou.com/profile/3xuser123"),
            None
        );
        assert_eq!(photo_id_in("https://www.kuaishou.com/f/X-abcDEF123"), None);
    }

    #[test]
    fn web_short_links_and_home_pages() {
        assert!(is_web_short_link("https://www.kuaishou.com/f/X-abcDEF123"));
        assert!(!is_web_short_link("https://www.kuaishou.com/f/"));
        assert!(!is_web_short_link("https://example.com/f/X-abc"));
        assert!(is_home("https://www.kuaishou.com/"));
        assert!(is_home("https://www.kuaishou.com/new-reco"));
        assert!(!is_home(
            "https://www.kuaishou.com/short-video/3xk8vw5ahmc3a2e"
        ));
    }

    #[tokio::test]
    async fn profile_link_is_unsupported_without_network() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::KuaiShou),
        )
        .unwrap();
        let err = parse(&http, "https://www.kuaishou.com/profile/3xuser123")
            .await
            .unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
    }

    #[test]
    fn atlas_images_are_jpg_without_double_slash() {
        let state = state_with(
            json!({
                "ext_params": {"atlas": {
                    "cdn": ["p2.a.yximgs.com"],
                    "list": ["/ufile/atlas/a_0.webp", "ufile/atlas/a_1.jpg"],
                    "music": "/bs2/music/m.m4a",
                    "musicCdnList": [{"cdn": "m.a.yximgs.com"}]
                }}
            }),
            1,
        );
        let info = build(&state).unwrap();
        assert_eq!(
            info.images[0].url,
            "https://p2.a.yximgs.com/ufile/atlas/a_0.jpg"
        );
        assert_eq!(
            info.images[1].url,
            "https://p2.a.yximgs.com/ufile/atlas/a_1.jpg"
        );
        assert_eq!(info.music_url, "https://m.a.yximgs.com/bs2/music/m.m4a");
    }

    #[test]
    fn single_picture_uses_cover_and_single_music() {
        let state = state_with(
            json!({
                "photoType": "SINGLE_PICTURE",
                "mainMvUrls": [],
                "coverUrls": [{"url": "http://p2.a.yximgs.com/upic/x.jpg"}],
                "ext_params": {"single": {
                    "music": "bs2/music/s.m4a",
                    "musicCdnList": [{"cdn": "m.a.yximgs.com"}]
                }}
            }),
            1,
        );
        let info = build(&state).unwrap();
        assert!(!info.is_empty(), "单图作品不该报没有内容");
        assert_eq!(info.images[0].url, "https://p2.a.yximgs.com/upic/x.jpg");
        assert_eq!(info.music_url, "https://m.a.yximgs.com/bs2/music/s.m4a");
    }

    #[test]
    fn live_photo_single_picture_gets_a_notice() {
        let photo = |caption: &str| {
            state_with(
                json!({
                    "photoType": "SINGLE_PICTURE",
                    "caption": caption,
                    "coverUrls": [{"url": "https://p2.a.yximgs.com/upic/x.jpg"}]
                }),
                1,
            )
        };
        assert!(build(&photo("周末 #实况照片"))
            .unwrap()
            .notice
            .contains("实况"));
        assert!(!build(&photo("#Live 生活")).unwrap().notice.is_empty());
        assert!(build(&photo("普通单图")).unwrap().notice.is_empty());
    }

    #[test]
    fn hevc_representations_are_labelled() {
        let state = state_with(
            json!({
                "mainMvUrls": [{"url": "https://v/main.mp4"}],
                "manifest": {"adaptationSet": [{"representation": [
                    {"url": "https://v/avc.mp4", "width": 720, "height": 1280, "videoCodec": "avc"},
                    {"url": "https://v/hevc.mp4", "width": 720, "height": 1280, "videoCodec": "hevc"}
                ]}]}
            }),
            1,
        );
        let info = build(&state).unwrap();
        let labels: Vec<&str> = info.formats.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.contains(&"720p"), "{labels:?}");
        assert!(labels.contains(&"720p H.265"), "{labels:?}");
    }

    #[test]
    fn captcha_page_is_classified_as_blocked() {
        let html = "<html><title>验证</title><body>请完成滑块验证</body></html>";
        assert_eq!(block_error(html).reason, crate::Reason::Blocked);
    }
}
