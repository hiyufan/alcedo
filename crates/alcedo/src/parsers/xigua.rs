//! 西瓜视频。
//!
//! 西瓜已并入抖音：作品 ID 就是抖音的 aweme_id，官方分享链接也换成了
//! `www.iesdouyin.com/xg/video/<id>`。原来用的 `m.ixigua.com/douyin/share/video` 分享页
//! 其实就是抖音分享页换了个域名，和抖音一样不再在 SSR 里渲染 `videoInfoRes`
//! （求原图 2026-09 实测所有作品都拿不到）。取出 ID 后直接走抖音解析器，结果还多了
//! 清晰度档位和时长。

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::VideoInfo;
use crate::parsers::douyin;
use crate::util;

/// 解析一条西瓜视频分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let id = match video_id_in(url) {
        Some(id) => id,
        None if util::host_of(url).as_deref() == Some("v.ixigua.com") => {
            // 短链：不跟跳转，从 Location 里取 ID
            let location = http.resolve_redirect(url).await?;
            video_id_in(&location).ok_or_else(|| Error::deleted("西瓜短链没有跳转到作品页"))?
        }
        None => return Err(Error::unsupported("链接里没有西瓜视频的作品 ID")),
    };
    parse_id(http, &id).await
}

/// 已知西瓜视频作品 ID 时直接解析。
pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    douyin::parse_id(http, id).await
}

/// 路径里从后往前第一个纯数字段就是作品 ID。
///
/// `www.ixigua.com/<id>`、`m.ixigua.com/video/<id>/`、`www.iesdouyin.com/xg/video/<id>/`
/// 都是这个形状；短链跳到首页 / 活动页这类地址时返回 `None`，别拿别的段瞎猜。
fn video_id_in(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .path_segments()?
        .filter(|s| !s.is_empty())
        .rfind(|s| s.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_from_every_url_shape() {
        for url in [
            "https://www.ixigua.com/7234567890123456789",
            "https://www.ixigua.com/7234567890123456789?logTag=abc",
            "https://m.ixigua.com/video/7234567890123456789/",
            "https://www.iesdouyin.com/xg/video/7234567890123456789/",
        ] {
            assert_eq!(
                video_id_in(url).as_deref(),
                Some("7234567890123456789"),
                "{url}"
            );
        }
        assert_eq!(video_id_in("https://www.ixigua.com/"), None);
        assert_eq!(video_id_in("https://v.ixigua.com/iRkb8xYp/"), None);
    }

    #[tokio::test]
    async fn non_video_page_is_unsupported_without_network() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::XiGua),
        )
        .unwrap();
        let err = parse(&http, "https://www.ixigua.com/channel/game")
            .await
            .unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
    }
}
