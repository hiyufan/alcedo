//! 今日头条。
//!
//! - 微头条（`m.toutiao.com/w/<id>`）：手机版页面把整条帖子放在 `<script id="RENDER_DATA">`
//!   里（URL 编码的 JSON），取全部图片、正文和作者。桌面版 `www.toutiao.com` 回的是脚本
//!   验证页，拿不到数据，所以一律按 ID 请求手机版。
//! - 头条视频（`/video/<id>`）：和西瓜一样已经并入抖音，作品 ID 就是抖音的 aweme_id，
//!   直接走抖音解析器（2026-11-27 实测小视频 `7505382061495176511` 拿到 720p / 576p 直链；
//!   页面里只有加密的 `videoId`，没有直链）。
//! - App「复制链接」给的是 `m.toutiao.com/is/<码>/` 短链，302 到上面两种地址。
//! - 头条文章（`/article/`、`/a`、`/i`、`/group/`）不是视频，直接说明。
//!
//! 图片地址带签名（`x-signature`），只有页面给的那一档能下：`largeImageList` 是帖子里
//! 最大的一档，去掉处理参数拿原图的写法全部 403（求原图 2026-11 实测）。CDN 不校验 Referer。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Image, VideoInfo};
use crate::parsers::douyin;
use crate::util;

const RENDER_DATA: &str = r#"<script id="RENDER_DATA" type="application/json">"#;
const ARTICLE_UNSUPPORTED: &str = "目前支持头条视频和微头条图文；头条文章暂不支持";

/// 解析一条今日头条分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let target = if path_of(url).starts_with("/is/") {
        // 短链：不跟跳转，从 Location 里取作品地址
        http.resolve_redirect(url).await?
    } else {
        url.to_owned()
    };
    match Link::of(&target) {
        Link::Post(id) => post(http, &id).await,
        Link::Video(id) => parse_id(http, &id).await,
        Link::Article => Err(Error::unsupported(ARTICLE_UNSUPPORTED)),
        Link::Unknown => Err(Error::unsupported("链接里没有头条作品的 ID")),
    }
}

/// 已知头条视频 ID 时直接解析。微头条只能用链接解析：两者的 ID 都是纯数字，凭 ID 分不出来。
pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    douyin::parse_id(http, id).await
}

#[derive(Debug, PartialEq)]
enum Link {
    Post(String),
    Video(String),
    Article,
    Unknown,
}

impl Link {
    fn of(url: &str) -> Self {
        let path = path_of(url);
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let id_after = |key: &str| {
            segs.iter()
                .position(|s| *s == key)
                .and_then(|i| segs.get(i + 1))
                .filter(|s| is_id(s))
                .map(|s| (*s).to_owned())
        };
        if let Some(id) = id_after("w") {
            return Link::Post(id);
        }
        if let Some(id) = id_after("video") {
            return Link::Video(id);
        }
        // /article/<id>、/group/<id>，以及 /a<id>、/i<id> 这类老写法
        let article = segs.first().is_some_and(|first| {
            matches!(*first, "article" | "group")
                || first.strip_prefix(['a', 'i']).is_some_and(is_id)
        });
        if article {
            Link::Article
        } else {
            Link::Unknown
        }
    }
}

/// 头条的作品 ID：6–25 位数字。
fn is_id(s: &str) -> bool {
    (6..=25).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn path_of(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default()
}

/// 微头条：读手机版页面里的 RENDER_DATA。
async fn post(http: &Http, id: &str) -> Result<VideoInfo> {
    let page = format!("https://m.toutiao.com/w/{id}/");
    let resp = http
        .send(Req::get(&page).header("User-Agent", ua::pick(ua::Platform::Ios)))
        .await?;
    resp.error_for_status()?;
    let data = render_data(&resp.text())?;
    let mut info = build_post(&data)?;
    info.page_url = page;
    Ok(info)
}

fn render_data(html: &str) -> Result<Value> {
    let start = html
        .find(RENDER_DATA)
        .ok_or_else(|| Error::parse("页面里没找到微头条数据（RENDER_DATA）"))?
        + RENDER_DATA.len();
    let len = html[start..]
        .find("</script>")
        .ok_or_else(|| Error::parse("RENDER_DATA 没有结束标签"))?;
    let raw = percent_encoding::percent_decode_str(&html[start..start + len]).decode_utf8_lossy();
    Ok(serde_json::from_str(&raw)?)
}

fn build_post(data: &Value) -> Result<VideoInfo> {
    let base = util::get(data, &["articleInfo", "thread", "threadBase"])
        .filter(|b| b.as_object().is_some_and(|o| !o.is_empty()))
        .ok_or_else(|| Error::deleted("这条微头条已删除或不可见"))?;

    let list = match util::arr_at(base, &["largeImageList"]) {
        [] => util::arr_at(base, &["thumbImageList"]),
        large => large,
    };
    let images: Vec<Image> = list
        .iter()
        .filter_map(|item| {
            let u = util::first_str(item, &[&["url"], &["urlList", "0", "url"]]);
            (!u.is_empty()).then(|| Image::new(u))
        })
        .collect();
    if images.is_empty() {
        // 只有视频（videoGroup）或纯文字。微头条里的视频没有单独的播放地址
        if util::get(base, &["videoGroup"]).is_some_and(|v| !v.is_null()) {
            return Err(Error::unsupported(
                "微头条里的视频暂不支持，可以打开视频本身的分享链接",
            ));
        }
        return Err(Error::empty());
    }

    let user = util::get_or_null(base, &["user", "info"]);
    let title = util::first_str(base, &[&["title"], &["content"]]);
    Ok(VideoInfo {
        cover_url: images[0].url.clone(),
        title: title.trim().chars().take(120).collect(),
        images,
        author: Author::new(
            util::id_at(user, &["userId"]),
            util::str_at(user, &["name"]),
            util::str_at(user, &["avatarUrl"]),
        ),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const IMG: &str =
        "https://p3-sign.toutiaoimg.com/tos-cn-i-x/a~tplv-shrink:1280:2773.jpeg?x-signature=s";

    #[test]
    fn link_shapes() {
        let cases = [
            (
                "https://m.toutiao.com/w/1881280705161283/?app=&share_token=x",
                Link::Post("1881280705161283".into()),
            ),
            (
                "https://www.toutiao.com/w/1881280705161283/",
                Link::Post("1881280705161283".into()),
            ),
            (
                "https://m.toutiao.com/video/7505382061495176511/",
                Link::Video("7505382061495176511".into()),
            ),
            (
                "https://www.toutiao.com/video/7505382061495176511/",
                Link::Video("7505382061495176511".into()),
            ),
            (
                "https://www.toutiao.com/article/7505382061495176511/",
                Link::Article,
            ),
            (
                "https://www.toutiao.com/a7505382061495176511/",
                Link::Article,
            ),
            (
                "https://www.toutiao.com/group/7505382061495176511/",
                Link::Article,
            ),
            ("https://www.toutiao.com/", Link::Unknown),
            // ID 太短不是作品
            ("https://www.toutiao.com/video/1/", Link::Unknown),
        ];
        for (url, want) in cases {
            assert_eq!(Link::of(url), want, "{url}");
        }
    }

    #[test]
    fn decodes_render_data() {
        let json = r#"{"articleInfo":{"thread":{"threadBase":{"title":"壁纸 #1"}}}}"#;
        let encoded =
            percent_encoding::utf8_percent_encode(json, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let html = format!("<html>{RENDER_DATA}{encoded}</script></html>");
        let data = render_data(&html).unwrap();
        assert_eq!(
            util::str_at(&data, &["articleInfo", "thread", "threadBase", "title"]),
            "壁纸 #1"
        );
        assert!(render_data("<html/>").is_err());
    }

    #[test]
    fn builds_post_with_large_images() {
        let data = json!({ "articleInfo": { "thread": { "threadBase": {
            "title": "  超高清手机壁纸  ",
            "largeImageList": [{ "url": IMG }, { "urlList": [{ "url": "https://p3-sign.toutiaoimg.com/b.jpeg" }] }],
            "thumbImageList": [{ "url": "https://p3-sign.toutiaoimg.com/thumb.jpeg" }],
            "user": { "info": { "userId": 1_240_729_732_786_071_u64, "name": "末初美图", "avatarUrl": "https://p3-sign.toutiaoimg.com/a.image" } }
        }}}});
        let info = build_post(&data).unwrap();
        assert_eq!(info.images.len(), 2);
        assert_eq!(info.images[0].url, IMG);
        assert_eq!(info.cover_url, IMG);
        assert_eq!(info.title, "超高清手机壁纸");
        assert_eq!(info.author.uid, "1240729732786071");
        assert_eq!(info.author.name, "末初美图");
    }

    #[test]
    fn falls_back_to_thumbnails() {
        let data = json!({ "articleInfo": { "thread": { "threadBase": {
            "content": "正文",
            "thumbImageList": [{ "url": "https://p3-sign.toutiaoimg.com/thumb.jpeg" }]
        }}}});
        let info = build_post(&data).unwrap();
        assert_eq!(info.images.len(), 1);
        assert_eq!(info.title, "正文");
    }

    #[test]
    fn missing_video_only_and_text_only_posts() {
        let gone = json!({ "articleInfo": { "thread": { "threadBase": {} } } });
        assert_eq!(
            build_post(&gone).unwrap_err().reason,
            crate::Reason::Deleted
        );

        let video_only = json!({ "articleInfo": { "thread": { "threadBase": {
            "title": "t", "videoGroup": { "vid": "v1" } }}}});
        assert_eq!(
            build_post(&video_only).unwrap_err().reason,
            crate::Reason::Unsupported
        );

        let text_only = json!({ "articleInfo": { "thread": { "threadBase": { "title": "t" } } } });
        assert_eq!(
            build_post(&text_only).unwrap_err().reason,
            crate::Reason::Empty
        );
    }

    #[tokio::test]
    async fn article_links_are_unsupported_without_network() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::TouTiao),
        )
        .unwrap();
        let err = parse(
            &http,
            "https://www.toutiao.com/article/7505382061495176511/",
        )
        .await
        .unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
        assert!(err.to_string().contains("文章"));
    }
}
