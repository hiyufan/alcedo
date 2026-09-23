//! 各平台解析器。
//!
//! 分发走 `match`，不是 trait object：平台是编译期已知的闭集，静态分发让
//! 编译器能跨模块内联，解析路径上一次虚调用都没有。

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Source, VideoInfo};

// 国内平台
pub mod acfun;
pub mod bilibili;
pub mod cctv;
pub mod douyin;
pub mod huya;
pub mod kuaishou;
pub mod lishipin;
pub mod lvzhou;
pub mod meipai;
pub mod meta;
pub mod pipixia;
pub mod qqvideo;
pub mod redbook;
pub mod simple;
pub mod sohu;
pub mod weibo;
pub mod xigua;
pub mod xinpianchang;

// 海外平台
pub mod dailymotion;
pub mod facebook;
pub mod instagram;
pub mod pinterest;
pub mod reddit;
pub mod tiktok;
pub mod twitch;
pub mod twitter;
pub mod vimeo;
pub mod youtube;

/// 按平台分发分享链接。
pub async fn dispatch(http: &Http, source: Source, url: &str) -> Result<VideoInfo> {
    match source {
        Source::DouYin => douyin::parse(http, url).await,
        Source::KuaiShou => kuaishou::parse(http, url).await,
        Source::PiPiXia => pipixia::parse(http, url).await,
        Source::WeiBo => weibo::parse(http, url).await,
        Source::WeiShi => simple::weishi(http, url).await,
        Source::LvZhou => lvzhou::parse(http, url).await,
        Source::ZuiYou => simple::zuiyou(http, url).await,
        Source::QuanMin => simple::quanmin(http, url).await,
        Source::XiGua => xigua::parse(http, url).await,
        Source::LiShiPin => lishipin::parse(http, url).await,
        Source::PiPiGaoXiao => simple::pipigaoxiao(http, url).await,
        Source::HuYa => huya::parse(http, url).await,
        Source::AcFun => acfun::parse(http, url).await,
        Source::DouPai => simple::doupai(http, url).await,
        Source::MeiPai => meipai::parse(http, url).await,
        Source::QuanMinKGe => simple::quanminkge(http, url).await,
        Source::SixRoom => simple::sixroom(http, url).await,
        Source::XinPianChang => xinpianchang::parse(http, url).await,
        Source::HaoKan => simple::haokan(http, url).await,
        Source::BiliBili => bilibili::parse(http, url).await,
        Source::RedBook => redbook::parse(http, url).await,
        Source::QQVideo => qqvideo::parse(http, url).await,
        Source::Sohu => sohu::parse(http, url).await,
        Source::CCTV => cctv::parse(http, url).await,
        Source::Twitter => twitter::parse(http, url).await,
        Source::YouTube => youtube::parse(http, url).await,
        Source::TikTok => tiktok::parse(http, url).await,
        Source::Instagram => instagram::parse(http, url).await,
        Source::Threads => instagram::parse_threads(http, url).await,
        Source::Vimeo => vimeo::parse(http, url).await,
        Source::Facebook => facebook::parse(http, url).await,
        Source::Twitch => twitch::parse(http, url).await,
        Source::Reddit => reddit::parse(http, url).await,
        Source::Pinterest => pinterest::parse(http, url).await,
        Source::DailyMotion => dailymotion::parse(http, url).await,
    }
}

/// 按平台分发作品 ID。不是每个平台都能只凭 ID 还原出链接。
pub async fn dispatch_id(http: &Http, source: Source, id: &str) -> Result<VideoInfo> {
    match source {
        Source::DouYin => douyin::parse_id(http, id).await,
        Source::PiPiXia => pipixia::parse_id(http, id).await,
        Source::WeiBo => weibo::parse_id(http, id).await,
        Source::WeiShi => simple::weishi_id(http, id).await,
        Source::LvZhou => {
            lvzhou::parse(
                http,
                &format!("https://m.oasis.weibo.cn/v1/h5/share?sid={id}"),
            )
            .await
        }
        Source::ZuiYou => simple::zuiyou_id(http, id).await,
        Source::QuanMin => simple::quanmin_id(http, id).await,
        Source::XiGua => xigua::parse_id(http, id).await,
        Source::LiShiPin => lishipin::parse_id(http, id).await,
        Source::PiPiGaoXiao => simple::pipigaoxiao_id(http, id).await,
        Source::HuYa => huya::parse_id(http, id).await,
        Source::AcFun => acfun::parse(http, &format!("https://www.acfun.cn/v/{id}")).await,
        Source::DouPai => simple::doupai_id(http, id).await,
        Source::MeiPai => meipai::parse(http, &format!("https://www.meipai.com/video/{id}")).await,
        Source::QuanMinKGe => simple::quanminkge_id(http, id).await,
        Source::SixRoom => simple::sixroom_id(http, id).await,
        Source::HaoKan => simple::haokan_id(http, id).await,
        Source::BiliBili => bilibili::parse_id(http, id).await,
        Source::QQVideo => qqvideo::parse_id(http, id).await,
        Source::Sohu => sohu::parse_id(http, id).await,
        Source::CCTV => cctv::parse_id(http, id).await,
        Source::Twitter => twitter::parse_id(http, id).await,
        Source::YouTube => youtube::parse_id(http, id).await,
        Source::TikTok => tiktok::parse_id(http, id).await,
        Source::Vimeo => vimeo::parse_id(http, id).await,
        Source::DailyMotion => dailymotion::parse_id(http, id).await,
        // 这几家的作品 ID 单独拿出来没法还原成可请求的地址
        Source::KuaiShou
        | Source::RedBook
        | Source::XinPianChang
        | Source::Instagram
        | Source::Threads
        | Source::Facebook
        | Source::Twitch
        | Source::Reddit
        | Source::Pinterest => Err(Error::unsupported(format!(
            "{} 只能用完整分享链接解析",
            source.display_name()
        ))),
    }
}

/// CDN 直链要带的 Referer。缺了这个头，下面这些域名一律 403。
static REFERERS: &[(&str, &str)] = &[
    ("bilivideo.com", "https://www.bilibili.com/"),
    ("hdslb.com", "https://www.bilibili.com/"),
    ("acfun.cn", "https://www.acfun.cn/"),
    ("ak-d.tv", "https://www.acfun.cn/"),
    ("xhscdn.com", "https://www.xiaohongshu.com/"),
    ("xiaohongshu.com", "https://www.xiaohongshu.com/"),
    ("douyinvod.com", "https://www.douyin.com/"),
    ("douyinpic.com", "https://www.douyin.com/"),
    ("douyinstatic.com", "https://www.douyin.com/"),
    ("365yg.com", "https://www.douyin.com/"),
    ("snssdk.com", "https://www.douyin.com/"),
    ("bytecdn.cn", "https://www.douyin.com/"),
    ("kuaishou.com", "https://www.kuaishou.com/"),
    ("kwaicdn.com", "https://www.kuaishou.com/"),
    ("yximgs.com", "https://www.kuaishou.com/"),
    ("twimg.com", "https://x.com/"),
    ("sinaimg.cn", "https://weibo.com/"),
    ("weibocdn.com", "https://weibo.com/"),
    ("miaopai.com", "https://weibo.com/"),
    ("pipix.com", "https://h5.pipix.com/"),
    ("ixigua.com", "https://www.ixigua.com/"),
    ("ixiguavideo.com", "https://www.ixigua.com/"),
    ("tiktokcdn.com", "https://www.tiktok.com/"),
    ("tiktokcdn-us.com", "https://www.tiktok.com/"),
    ("cdninstagram.com", "https://www.instagram.com/"),
    ("fbcdn.net", "https://www.instagram.com/"),
    ("pinimg.com", "https://www.pinterest.com/"),
    ("qq.com", "https://v.qq.com/"),
    ("cntv.cn", "https://tv.cctv.com/"),
    ("pearvideo.com", "https://www.pearvideo.com/"),
    ("huya.com", "https://v.huya.com/"),
    ("huyaimg.com", "https://v.huya.com/"),
];

/// 这条直链需要的 Referer。
pub fn referer_for(url: &str) -> Option<&'static str> {
    let host = crate::util::host_of(url)?;
    REFERERS
        .iter()
        .filter(|(d, _)| {
            host == *d
                || (host.len() > d.len()
                    && host.ends_with(*d)
                    && host.as_bytes()[host.len() - d.len() - 1] == b'.')
        })
        .max_by_key(|(d, _)| d.len())
        .map(|(_, r)| *r)
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn referer_matches_by_suffix() {
        assert_eq!(
            referer_for("https://v26-web.douyinvod.com/abc/video.mp4?a=1"),
            Some("https://www.douyin.com/")
        );
        assert_eq!(
            referer_for("https://upos-sz-mirror08c.bilivideo.com/x.m4s"),
            Some("https://www.bilibili.com/")
        );
        assert_eq!(referer_for("https://example.com/a.mp4"), None);
    }

    #[test]
    fn referer_is_not_fooled_by_substring() {
        assert_eq!(
            referer_for("https://evil-bilivideo.com.attacker.net/x"),
            None
        );
    }

    #[tokio::test]
    async fn id_dispatch_rejects_unsupported_platforms() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(Source::RedBook),
        )
        .unwrap();
        let err = dispatch_id(&http, Source::RedBook, "abc")
            .await
            .unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
    }
}
