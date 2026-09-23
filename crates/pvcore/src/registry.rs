//! 链接 → 平台的识别。
//!
//! 和 Python 版的区别：那边用 `domain in share_url` 做子串匹配，
//! `https://evil.com/?r=www.douyin.com` 会被判成抖音，然后带着抖音的 cookie
//! 去请求攻击者的服务器。这里一律先解析出主机名，再按**域名后缀**匹配。

use crate::model::Source;

/// 域名 → 平台。后缀匹配：`bilibili.com` 同时覆盖 `www.` / `m.` / `space.`。
///
/// 顺序无所谓，匹配时取最长的那条，所以 `tv.sohu.com` 和 `sohu.com` 可以共存。
static DOMAINS: &[(&str, Source)] = &[
    // ---- 国内 ----
    ("douyin.com", Source::DouYin),
    ("iesdouyin.com", Source::DouYin),
    ("kuaishou.com", Source::KuaiShou),
    ("chenzhongtech.com", Source::KuaiShou),
    ("pipix.com", Source::PiPiXia),
    ("weibo.com", Source::WeiBo),
    ("weibo.cn", Source::LvZhou),
    ("oasis.weibo.cn", Source::LvZhou),
    ("t.cn", Source::WeiBo),
    ("weishi.qq.com", Source::WeiShi),
    ("xiaochuankeji.cn", Source::ZuiYou),
    ("izuiyou.com", Source::ZuiYou),
    ("xspshare.baidu.com", Source::QuanMin),
    ("quanmin.hao222.com", Source::QuanMin),
    ("ixigua.com", Source::XiGua),
    ("pearvideo.com", Source::LiShiPin),
    ("pipigx.com", Source::PiPiGaoXiao),
    ("ippzone.com", Source::PiPiGaoXiao),
    ("huya.com", Source::HuYa),
    ("acfun.cn", Source::AcFun),
    ("doupai.cc", Source::DouPai),
    ("meipai.com", Source::MeiPai),
    ("kg.qq.com", Source::QuanMinKGe),
    ("6.cn", Source::SixRoom),
    ("xinpianchang.com", Source::XinPianChang),
    ("haokan.baidu.com", Source::HaoKan),
    ("haokan.hao123.com", Source::HaoKan),
    ("bilibili.com", Source::BiliBili),
    ("b23.tv", Source::BiliBili),
    ("bili2233.cn", Source::BiliBili),
    ("xiaohongshu.com", Source::RedBook),
    ("xhslink.com", Source::RedBook),
    ("xhslink.cn", Source::RedBook),
    ("v.qq.com", Source::QQVideo),
    ("sohu.com", Source::Sohu),
    ("cctv.cn", Source::CCTV),
    ("cctv.com", Source::CCTV),
    // ---- 海外 ----
    ("twitter.com", Source::Twitter),
    ("x.com", Source::Twitter),
    ("t.co", Source::Twitter),
    ("fxtwitter.com", Source::Twitter),
    ("vxtwitter.com", Source::Twitter),
    ("youtube.com", Source::YouTube),
    ("youtu.be", Source::YouTube),
    ("youtube-nocookie.com", Source::YouTube),
    ("tiktok.com", Source::TikTok),
    ("instagram.com", Source::Instagram),
    ("ddinstagram.com", Source::Instagram),
    ("threads.net", Source::Threads),
    ("threads.com", Source::Threads),
    ("vimeo.com", Source::Vimeo),
    ("facebook.com", Source::Facebook),
    ("fb.watch", Source::Facebook),
    ("fb.com", Source::Facebook),
    ("twitch.tv", Source::Twitch),
    ("reddit.com", Source::Reddit),
    ("redd.it", Source::Reddit),
    ("pinterest.com", Source::Pinterest),
    ("pin.it", Source::Pinterest),
    ("dailymotion.com", Source::DailyMotion),
    ("dai.ly", Source::DailyMotion),
];

/// 主机名是不是这个域名或它的子域。
fn host_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

/// 这个链接属于哪个平台。识别不出来返回 `None`。
pub fn detect(url: &str) -> Option<Source> {
    let host = crate::util::host_of(url)?;
    // 取最长匹配：tv.sohu.com 这种更具体的条目要赢过 sohu.com
    DOMAINS
        .iter()
        .filter(|(d, _)| host_matches(&host, d))
        .max_by_key(|(d, _)| d.len())
        .map(|(_, s)| *s)
}

/// 支持的平台清单，给 CLI / 文档用。
pub fn supported() -> Vec<Source> {
    let mut all: Vec<Source> = DOMAINS.iter().map(|(_, s)| *s).collect();
    all.sort();
    all.dedup();
    all
}

/// 某个平台认哪些域名。
pub fn domains_of(source: Source) -> Vec<&'static str> {
    DOMAINS
        .iter()
        .filter(|(_, s)| *s == source)
        .map(|(d, _)| *d)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_links() {
        let cases = [
            ("https://v.douyin.com/iRNBho6u/", Source::DouYin),
            (
                "https://www.douyin.com/video/7424432820954598707",
                Source::DouYin,
            ),
            ("https://b23.tv/abcdef", Source::BiliBili),
            (
                "https://www.bilibili.com/video/BV1xx411c7mD",
                Source::BiliBili,
            ),
            ("https://www.xiaohongshu.com/explore/abc", Source::RedBook),
            ("https://youtu.be/dQw4w9WgXcQ", Source::YouTube),
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                Source::YouTube,
            ),
            ("https://x.com/user/status/123", Source::Twitter),
            ("https://www.tiktok.com/@u/video/123", Source::TikTok),
        ];
        for (url, want) in cases {
            assert_eq!(detect(url), Some(want), "识别错了: {url}");
        }
    }

    #[test]
    fn substring_attack_is_rejected() {
        // Python 版这条会被当成抖音, 然后把抖音的请求头发给攻击者的服务器
        assert_eq!(detect("https://evil.com/?redirect=www.douyin.com"), None);
        assert_eq!(detect("https://douyin.com.evil.com/x"), None);
        assert_eq!(detect("https://notbilibili.com/video/1"), None);
    }

    #[test]
    fn longest_suffix_wins() {
        // haokan.baidu.com 比裸 baidu 更具体; 没有裸 baidu 条目时也不能误伤
        assert_eq!(
            detect("https://haokan.baidu.com/v?vid=1"),
            Some(Source::HaoKan)
        );
        assert_eq!(detect("https://www.baidu.com/"), None);
    }

    #[test]
    fn weibo_and_lvzhou_are_separate_domains() {
        assert_eq!(
            detect("https://weibo.com/2543858012/Q9pcJ4S21"),
            Some(Source::WeiBo)
        );
        assert_eq!(
            detect("https://m.oasis.weibo.cn/v1/h5/share?sid=1"),
            Some(Source::LvZhou)
        );
    }

    #[test]
    fn unknown_host_returns_none() {
        assert_eq!(detect("https://example.com/video/1"), None);
        assert_eq!(detect("not a url"), None);
    }

    #[test]
    fn every_source_has_at_least_one_domain() {
        for s in supported() {
            assert!(!domains_of(s).is_empty(), "{s} 没有域名");
        }
    }
}
