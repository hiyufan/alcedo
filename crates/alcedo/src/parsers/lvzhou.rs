//! 绿洲（微博旗下）。整页都是服务端渲染，直接读 DOM。

use std::sync::LazyLock;

use scraper::{Html, Selector};

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Author, VideoInfo};

// 选择器都是编译期字面量，写错了测试立刻会挂。scraper 不缓存选择器，
// 每次请求重新 parse 一遍是纯浪费，提成 static。
static VIDEO: LazyLock<Selector> = LazyLock::new(|| Selector::parse("video").unwrap());
static VIDEO_COVER: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.video-cover").unwrap());
static STATUS_TITLE: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.status-title").unwrap());
static NICKNAME: LazyLock<Selector> = LazyLock::new(|| Selector::parse("div.nickname").unwrap());
static AVATAR_IMG: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a.avatar img").unwrap());

/// 解析一条绿洲分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let html = http.get_text(url).await?;
    build(&html)
}

fn build(html: &str) -> Result<VideoInfo> {
    let doc = Html::parse_document(html);

    let attr = |sel: &Selector, name: &str| -> String {
        doc.select(sel)
            .next()
            .and_then(|el| el.value().attr(name).map(str::to_owned))
            .unwrap_or_default()
    };
    let text = |sel: &Selector| -> String {
        doc.select(sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .unwrap_or_default()
    };

    let video_url = attr(&VIDEO, "src");
    // 封面藏在内联样式的 background-image 里
    let cover_url = cover_from_style(&attr(&VIDEO_COVER, "style"));

    let info = VideoInfo {
        video_url,
        cover_url,
        title: text(&STATUS_TITLE),
        author: Author {
            name: text(&NICKNAME),
            avatar: attr(&AVATAR_IMG, "src"),
            ..Default::default()
        },
        ..Default::default()
    };

    if info.is_empty() {
        return Err(Error::deleted(
            "绿洲页面里没有视频，动态可能已删除或需要登录",
        ));
    }
    Ok(info)
}

/// `background-image:url(https://...)` 里的地址。
fn cover_from_style(style: &str) -> String {
    let Some(at) = style.find("url(") else {
        return String::new();
    };
    let rest = &style[at + 4..];
    let Some(end) = rest.find(')') else {
        return String::new();
    };
    rest[..end].trim_matches(['"', '\'']).to_owned()
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn reads_video_and_author_from_dom() {
        let html = r#"
        <div class="video-cover" style="background-image:url(https://img/cover.jpg);"></div>
        <video src="https://v/clip.mp4"></video>
        <div class="status-title">今天的日常</div>
        <a class="avatar"><img src="https://img/av.jpg"></a>
        <div class="nickname">绿洲用户</div>"#;
        let info = build(html).unwrap();
        assert_eq!(info.video_url, "https://v/clip.mp4");
        assert_eq!(info.cover_url, "https://img/cover.jpg");
        assert_eq!(info.title, "今天的日常");
        assert_eq!(info.author.name, "绿洲用户");
    }

    #[test]
    fn quoted_style_url_is_handled() {
        assert_eq!(
            cover_from_style(r#"background-image:url("https://a/b.jpg")"#),
            "https://a/b.jpg"
        );
        assert_eq!(cover_from_style("color:red"), "");
    }

    #[test]
    fn empty_page_is_reported_as_deleted() {
        assert_eq!(
            build("<html></html>").unwrap_err().reason,
            crate::Reason::Deleted
        );
    }
}
