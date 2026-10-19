//! 直接贴进来的图片 / 视频文件地址（各平台 CDN 上的）。
//!
//! 光凭文件地址找不回原作品，也不用解析：原样给出去，下载的就是这一个文件（多半是
//! 网页上压缩过的版本），结果里提示想要原图就复制作品的分享链接。以前这种地址认不出
//! 平台，直接报"还不支持这个站点"（求原图 2026-10 有人贴抖音图片地址连试 3 次）。

use crate::model::{Image, Source, VideoInfo};

/// CDN 域名 → 所属平台。
const CDN_SOURCES: &[(&str, Source)] = &[
    ("douyinpic.com", Source::DouYin),
    ("douyinvod.com", Source::DouYin),
    ("xhscdn.com", Source::RedBook),
    ("sinaimg.cn", Source::WeiBo),
    ("weibocdn.com", Source::WeiBo),
    ("hdslb.com", Source::BiliBili),
    ("bilivideo.com", Source::BiliBili),
    ("bilivideo.cn", Source::BiliBili),
    ("twimg.com", Source::Twitter),
    ("cdninstagram.com", Source::Instagram),
    ("yximgs.com", Source::KuaiShou),
    ("kwimgs.com", Source::KuaiShou),
    ("kwaicdn.com", Source::KuaiShou),
];

/// 这些域名上的是视频；其余 CDN 上没有扩展名的地址（小红书、抖音图片常这样）当图片。
const VIDEO_HOSTS: &[&str] = &[
    "douyinvod.com",
    "bilivideo.com",
    "bilivideo.cn",
    "video.twimg.com",
    "weibocdn.com",
    "kwaicdn.com",
];
const VIDEO_EXTS: &[&str] = &[".mp4", ".mov", ".m4v", ".webm"];

/// 是已知平台 CDN 上的文件地址就直接给出结果，不是返回 `None`。不发请求。
pub fn media(url: &str) -> Option<(Source, VideoInfo)> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let source = CDN_SOURCES
        .iter()
        .find(|(d, _)| host_is(&host, d))
        .map(|(_, s)| *s)?;

    let path = parsed.path().to_ascii_lowercase();
    let is_video = VIDEO_EXTS.iter().any(|e| path.ends_with(e))
        || VIDEO_HOSTS.iter().any(|d| host_is(&host, d));
    let kind = if is_video { "视频" } else { "图片" };
    let mut info = VideoInfo {
        title: kind.to_owned(),
        notice: format!(
            "这是{kind}文件的地址，下载的就是这一个（多半是网页上压缩过的版本）；\
             想要原图和整条作品，复制作品的分享链接再来解析。"
        ),
        ..Default::default()
    };
    if is_video {
        info.video_url = url.to_owned();
    } else {
        info.cover_url = url.to_owned();
        info.images = vec![Image::new(url)];
    }
    Some((source, info))
}

fn host_is(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|h| h.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdn_images_and_videos_are_passed_through() {
        let (src, info) =
            media("https://p11-sign.douyinpic.com/tos-cn-i/abc~tplv-dy-aweme-images:q75.webp?x=1")
                .unwrap();
        assert_eq!(src, Source::DouYin);
        assert_eq!(info.images.len(), 1);
        assert!(info.video_url.is_empty());
        assert!(info.notice.contains("分享链接"));

        let (src, info) =
            media("https://video.twimg.com/ext_tw_video/1/pu/vid/720x1280/a.mp4").unwrap();
        assert_eq!(src, Source::Twitter);
        assert_eq!(
            info.video_url,
            "https://video.twimg.com/ext_tw_video/1/pu/vid/720x1280/a.mp4"
        );

        // 视频 CDN 上没扩展名的也是视频
        let (_, info) = media("https://v26-web.douyinvod.com/abc/video/tos/cn/x/?a=1").unwrap();
        assert!(!info.video_url.is_empty());
    }

    #[test]
    fn unknown_hosts_and_lookalikes_are_ignored() {
        assert!(media("https://example.com/a.jpg").is_none());
        assert!(media("https://evil-twimg.com/a.jpg").is_none());
        assert!(media("not a url").is_none());
    }
}
