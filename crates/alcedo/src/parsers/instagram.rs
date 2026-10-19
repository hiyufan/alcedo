//! Instagram / Threads。
//!
//! 主路径：把分享链接里的 shortcode 还原成 media_id，调 `api/v1/media/{id}/info/`。
//! 这个接口对公开内容不要求登录，但认 `X-IG-App-ID`（网页版自己带的那个）。
//!
//! 这个接口现在对未登录请求经常连存在的帖子也报 404，所以接着读帖子页：未登录打开
//! `/p/{code}` 时页面内嵌的 JSON（`xig_polaris_media.if_not_gated_logged_out`）里就有
//! 整条帖子，结构和接口的 `items[0]` 一样，多图帖每张都是原图（求原图实测 3072x4096）。
//!
//! 两条都不行时还有 OG 标签兜底：分享卡片的元数据是给爬虫看的，一直公开，但只有
//! 一张图 / 一条视频。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::parsers::meta;
use crate::util;

/// 网页版 Instagram 自己用的 app id，公开值。
const IG_APP_ID: &str = "936619743392459";

/// 解析一条Instagram分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let code = shortcode_from_url(url)
        .ok_or_else(|| Error::unsupported("链接里没有 Instagram 的作品 code"))?;

    let mut api_err = None;
    if let Some(id) = media_id_from_shortcode(&code) {
        match api_info(http, &id).await {
            Ok(info) => return Ok(info),
            // 接口报什么都先别信：未登录请求它会把好端端的帖子也报成 404，
            // 401/403 则是出口被挡。两种情况 OG 兜底都可能拿得到，
            // 只有落地页也说没了，才是真没了。
            Err(e) => {
                tracing::debug!(error = %e, "IG 接口不可用，读帖子页");
                api_err = Some(e);
            }
        }
    }

    match page_post(http, &code).await {
        Ok(Some(info)) => return Ok(info),
        Ok(None) => tracing::debug!("帖子页里没有内嵌数据，退到 OG 标签"),
        Err(e) => tracing::debug!(error = %e, "帖子页打不开，退到 OG 标签"),
    }

    og_fallback(http, url, "Instagram")
        .await
        .map_err(|og| more_useful(api_err, og))
}

/// 接口和兜底都失败时报哪个。
///
/// 默认信落地页——它是用户在浏览器里看到的那个结果。例外是接口明确要求登录、
/// 落地页又没说内容不存在：这时"配 cookie"才是站长能动手的方向。
fn more_useful(api: Option<Error>, og: Error) -> Error {
    match api {
        Some(api) if api.reason == crate::Reason::Login && og.reason != crate::Reason::Deleted => {
            api
        }
        _ => og,
    }
}

/// 解析一条 Threads 帖子链接（和 Instagram 同一套后端）。
pub async fn parse_threads(http: &Http, url: &str) -> Result<VideoInfo> {
    // Threads 和 Instagram 同一套后端，post code 也是同一种编码
    if let Some(code) = shortcode_from_url(url) {
        if let Some(id) = media_id_from_shortcode(&code) {
            if let Ok(info) = api_info(http, &id).await {
                return Ok(info);
            }
        }
    }
    og_fallback(http, url, "Threads").await
}

/// 未登录访问帖子页时用的头。照浏览器发，缺了 Sec-Fetch-Mode 拿到的是精简页。
fn page_request(url: &str) -> Req {
    Req::get(url)
        .header("User-Agent", ua::DESKTOP_FIXED)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-us,en;q=0.5")
        .header("Sec-Fetch-Mode", "navigate")
}

/// 帖子页内嵌的整条帖子。页面里没有（要登录、改版）返回 `Ok(None)`。
///
/// 和 yt-dlp 一样先开一次首页拿 csrftoken / mid 这些 cookie，再开帖子页。
async fn page_post(http: &Http, code: &str) -> Result<Option<VideoInfo>> {
    let home = http
        .send(page_request("https://www.instagram.com/"))
        .await?;
    let mut req = page_request(&format!("https://www.instagram.com/p/{code}/"));
    for (k, v) in home.set_cookies() {
        req = req.cookie(k, v);
    }
    let resp = http.send(req).await?;
    if !resp.status.is_success() {
        return Ok(None);
    }
    Ok(find_post(&resp.text(), code)
        .map(|post| build(&post))
        .filter(|info| !info.is_empty()))
}

/// 在页面的 `<script type="application/json">` 里找这条帖子。
///
/// 页面里还嵌着作者的其它帖子（推荐区），按 code 认出这一条。
fn find_post(html: &str, code: &str) -> Option<Value> {
    json_scripts(html)
        .filter(|blob| blob.contains("xig_polaris_media"))
        .filter_map(|blob| serde_json::from_str::<Value>(blob).ok())
        .find_map(|json| {
            let post = find_key(&json, "xig_polaris_media")?.get("if_not_gated_logged_out")?;
            (util::str_at(post, &["code"]) == code).then(|| post.clone())
        })
}

/// 页面里所有 `<script type="application/json" ...>` 的内容。
fn json_scripts(html: &str) -> impl Iterator<Item = &str> {
    const OPEN: &str = "<script type=\"application/json\"";
    let mut rest = html;
    std::iter::from_fn(move || {
        let at = rest.find(OPEN)?;
        let after = &rest[at..];
        let body_start = after.find('>')? + 1;
        let body_len = after[body_start..].find("</script>")?;
        let body = &after[body_start..body_start + body_len];
        rest = &after[body_start + body_len..];
        Some(body)
    })
}

/// 深度优先找第一个叫 `key` 的字段。
fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}

async fn api_info(http: &Http, media_id: &str) -> Result<VideoInfo> {
    let api = format!("https://i.instagram.com/api/v1/media/{media_id}/info/");
    let req = Req::get(&api)
        .header("X-IG-App-ID", IG_APP_ID)
        .header("User-Agent", ua::pick(ua::Platform::Desktop))
        .header("Accept", "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .referer("https://www.instagram.com/");

    let resp = http.send(req).await?;
    if resp.status.as_u16() == 401 || resp.status.as_u16() == 403 {
        return Err(Error::login("Instagram 要求这个出口登录"));
    }
    resp.error_for_status()?;
    let json = resp.json()?;

    // 轮播帖的 item 可达几十 KB，借用就够，别 .cloned() 深拷贝
    let item = util::arr_at(&json, &["items"])
        .first()
        .ok_or_else(|| Error::deleted("Instagram 没有返回作品"))?;

    let info = build(item);
    if info.is_empty() {
        return Err(Error::bare(crate::Reason::Empty));
    }
    Ok(info)
}

fn build(item: &Value) -> VideoInfo {
    let mut images = Vec::new();
    let mut video_url = String::new();
    let mut width = 0;
    let mut height = 0;

    // 轮播帖：carousel_media 里每一条各自是图或视频
    let carousel = util::arr_at(item, &["carousel_media"]);
    if !carousel.is_empty() {
        for child in carousel {
            let v = best_video(child);
            if v.is_empty() {
                let img = best_image(child);
                if !img.is_empty() {
                    images.push(Image::new(img));
                }
            } else if video_url.is_empty() {
                // 轮播里混着视频时，第一条视频当主视频
                video_url = v;
                width = util::u32_at(child, &["original_width"]);
                height = util::u32_at(child, &["original_height"]);
            }
        }
    } else {
        video_url = best_video(item);
        if video_url.is_empty() {
            let img = best_image(item);
            if !img.is_empty() {
                images.push(Image::new(img));
            }
        }
        width = util::u32_at(item, &["original_width"]);
        height = util::u32_at(item, &["original_height"]);
    }

    if !video_url.is_empty() {
        images.clear();
    }

    let user = util::get_or_null(item, &["user"]);
    VideoInfo {
        video_url,
        cover_url: best_image(item),
        title: util::str_at(item, &["caption", "text"]),
        images,
        duration: util::num_at(item, &["video_duration"]),
        width,
        height,
        author: Author::new(
            util::first_id(user, &[&["pk"], &["id"]]),
            util::first_str(user, &[&["username"], &["full_name"]]),
            util::str_at(user, &["profile_pic_url"]),
        ),
        ..Default::default()
    }
}

/// `video_versions` 里分辨率最高的那条。
fn best_video(node: &Value) -> String {
    util::arr_at(node, &["video_versions"])
        .iter()
        .max_by_key(|v| util::u64_at(v, &["width"]) * util::u64_at(v, &["height"]))
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default()
}

fn best_image(node: &Value) -> String {
    util::arr_at(node, &["image_versions2", "candidates"])
        .iter()
        .max_by_key(|v| util::u64_at(v, &["width"]) * util::u64_at(v, &["height"]))
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default()
}

async fn og_fallback(http: &Http, url: &str, platform: &str) -> Result<VideoInfo> {
    let resp = http
        .send(
            Req::get(url)
                // 用爬虫 UA：这些站对已知爬虫反而会老老实实吐 OG 标签
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (compatible; facebookexternalhit/1.1; +http://www.facebook.com/externalhit_uatext.php)",
                )
                .header("Accept", "text/html,*/*")
                // 下面的消费全在文档头部（OG 标签、<title>、登录跳转标记），
                // 整页拖完纯属白等传输
                .head_bytes(64 * 1024),
        )
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    meta::from_og(&html).ok_or_else(|| {
        let title = util::html_title(&html);
        // 「This post isn't available to everyone / to certain audiences」是只对部分
        // 用户可见的内容（年龄、地区、作者设置）。页面上也带登录链接，所以要先认它，
        // 别归成"要求登录"——登录了未必看得到
        if html.contains("available to everyone") || html.contains("certain audiences") {
            Error::restricted(format!("{platform} 只对部分用户展示这条内容"))
        } else if html.contains("/accounts/login") || title.contains("Login") {
            Error::login(format!("{platform} 要求这个出口登录"))
        } else {
            Error::restricted(format!(
                "{platform} 没有对外给出这条内容（标题: {}）",
                if title.is_empty() { "无" } else { &title }
            ))
        }
    })
}

/// `/p/<code>/`、`/reel/<code>/`、`/tv/<code>/`、`/post/<code>` 里的 code。
fn shortcode_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let i = segs
        .iter()
        .position(|s| matches!(*s, "p" | "reel" | "reels" | "tv" | "post"))?;
    let code = segs.get(i + 1)?;
    (!code.is_empty()).then(|| (*code).to_owned())
}

/// shortcode → media_id。
///
/// shortcode 就是 media_id 用一套 base64 变体编码出来的：每个字符 6 bit，
/// 大端拼起来。知道这个就不用再去页面里翻 id 了，省一次请求。
fn media_id_from_shortcode(code: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // 超过 11 位就会溢出 u64；正常的 code 是 11 位
    if code.is_empty() || code.len() > 11 {
        return None;
    }
    let mut id: u64 = 0;
    for b in code.bytes() {
        let v = ALPHABET.iter().position(|a| *a == b)? as u64;
        id = id.checked_mul(64)?.checked_add(v)?;
    }
    Some(id.to_string())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn landing_page_verdict_wins_over_api_404() {
        use crate::Reason;
        // 接口 404 但落地页说要登录：报登录，不能报"已删除"
        let got = more_useful(Some(Error::deleted("api 404")), Error::login("og"));
        assert_eq!(got.reason, Reason::Login);
        // 落地页也说没了，才是真没了
        let got = more_useful(Some(Error::login("api")), Error::deleted("og 404"));
        assert_eq!(got.reason, Reason::Deleted);
        // 接口要登录、落地页只说拿不到：登录是能动手的方向
        let got = more_useful(Some(Error::login("api")), Error::restricted("og"));
        assert_eq!(got.reason, Reason::Login);
        // 没走接口（shortcode 解不出 id）就只有落地页的结论
        let got = more_useful(None, Error::restricted("og"));
        assert_eq!(got.reason, Reason::Restricted);
    }

    #[test]
    fn finds_this_post_among_embedded_ones() {
        let other = json!({"xig_polaris_media": {"if_not_gated_logged_out": {"code": "OTHER"}}});
        let this = json!({"require": [[{"__bbox": {"result": {"data": {"xig_polaris_media": {
        "if_not_gated_logged_out": {
            "code": "BsOGulcndj-",
            "caption": {"text": "egg"},
            "user": {"pk": 1, "username": "world_record_egg"},
            "carousel_media": [
                {"image_versions2": {"candidates": [
                    {"url": "https://x.cdninstagram.com/orig.jpg", "width": 3072, "height": 4096},
                    {"url": "https://x.cdninstagram.com/small.jpg", "width": 320, "height": 427}
                ]}},
                {"image_versions2": {"candidates": [
                    {"url": "https://x.cdninstagram.com/2.jpg", "width": 1080, "height": 1080}
                ]}}
            ]
        }}}}}}]]});
        let html = format!(
            r#"<html><script type="application/json" data-sjs>{other}</script>
            <script type="application/json" data-content-len="9">{this}</script>
            <script>var x = 1;</script></html>"#
        );
        let post = find_post(&html, "BsOGulcndj-").expect("该认出这一条");
        let info = build(&post);
        assert_eq!(info.images.len(), 2, "多图帖要整条拿到");
        assert_eq!(info.images[0].url, "https://x.cdninstagram.com/orig.jpg");
        assert_eq!(info.author.name, "world_record_egg");
        assert!(find_post(&html, "NOPE").is_none());
    }

    #[test]
    fn extracts_shortcode() {
        for url in [
            "https://www.instagram.com/p/CxYzAbCdEfG/",
            "https://www.instagram.com/reel/CxYzAbCdEfG/?igshid=1",
            "https://www.instagram.com/tv/CxYzAbCdEfG/",
            "https://www.threads.net/@user/post/CxYzAbCdEfG",
        ] {
            assert_eq!(
                shortcode_from_url(url).as_deref(),
                Some("CxYzAbCdEfG"),
                "{url}"
            );
        }
        assert_eq!(
            shortcode_from_url("https://www.instagram.com/someuser/"),
            None
        );
    }

    #[test]
    fn shortcode_decodes_to_media_id() {
        // B 是字母表第 1 位, 所以 "B" -> 1; "BA" -> 64
        assert_eq!(media_id_from_shortcode("B").as_deref(), Some("1"));
        assert_eq!(media_id_from_shortcode("BA").as_deref(), Some("64"));
        // 已知样例: 真实 code 长度 11
        assert!(media_id_from_shortcode("CxYzAbCdEfG").is_some());
        // 非字母表字符和超长都要拒绝
        assert_eq!(media_id_from_shortcode("has space"), None);
        assert_eq!(media_id_from_shortcode("ABCDEFGHIJKLMNOP"), None);
    }

    #[test]
    fn builds_single_video_post() {
        let item = json!({
            "caption": {"text": "正文"},
            "video_duration": 12.5,
            "original_width": 1080, "original_height": 1920,
            "video_versions": [
                {"url": "https://v/low.mp4", "width": 480, "height": 854},
                {"url": "https://v/high.mp4", "width": 1080, "height": 1920}
            ],
            "image_versions2": {"candidates": [
                {"url": "https://p/small.jpg", "width": 320, "height": 568},
                {"url": "https://p/big.jpg", "width": 1080, "height": 1920}
            ]},
            "user": {"pk": 12345, "username": "someone", "profile_pic_url": "https://a/av.jpg"}
        });
        let info = build(&item);
        assert_eq!(info.video_url, "https://v/high.mp4");
        assert_eq!(info.cover_url, "https://p/big.jpg");
        assert_eq!(info.duration, 12.5);
        assert_eq!(info.author.uid, "12345");
        assert!(info.images.is_empty());
    }

    #[test]
    fn builds_carousel_gallery() {
        let item = json!({
            "caption": {"text": "多图"},
            "carousel_media": [
                {"image_versions2": {"candidates": [{"url": "https://p/1.jpg", "width": 1080, "height": 1080}]}},
                {"image_versions2": {"candidates": [{"url": "https://p/2.jpg", "width": 1080, "height": 1080}]}}
            ],
            "image_versions2": {"candidates": [{"url": "https://p/cover.jpg", "width": 640, "height": 640}]},
            "user": {"username": "u"}
        });
        let info = build(&item);
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 2);
        assert_eq!(info.cover_url, "https://p/cover.jpg");
    }

    #[test]
    fn carousel_with_video_prefers_video() {
        let item = json!({
            "carousel_media": [
                {"image_versions2": {"candidates": [{"url": "https://p/1.jpg", "width": 100, "height": 100}]}},
                {"video_versions": [{"url": "https://v/clip.mp4", "width": 720, "height": 1280}],
                 "original_width": 720, "original_height": 1280}
            ],
            "user": {"username": "u"}
        });
        let info = build(&item);
        assert_eq!(info.video_url, "https://v/clip.mp4");
        assert!(info.images.is_empty(), "有视频时图集要清掉");
        assert_eq!(info.width, 720);
    }
}
