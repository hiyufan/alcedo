//! 链接 → 平台的识别。
//!
//! 一律先解析出主机名，再按**域名后缀**匹配。用 `domain in url` 这种子串判断的话，
//! `https://evil.com/?r=www.douyin.com` 会被判成抖音，然后带着抖音的 cookie
//! 去请求攻击者的服务器。

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

/// 解析这个平台时**第一个**会连上的主机。
///
/// 预热连接时用。注意不是平台的门户域名，而是解析器真正请求的那个接口主机——
/// 预热 `www.douyin.com` 对随后请求 `www.iesdouyin.com` 的连接毫无帮助，反过来也一样。
///
/// 返回 `None` 表示第一跳取决于具体链接（短链跳转、页面抓取），预热没有固定目标。
pub const fn warmup_host(source: Source) -> Option<&'static str> {
    Some(match source {
        Source::DouYin => "www.douyin.com",
        Source::XiGua => "m.ixigua.com",
        Source::BiliBili => "api.bilibili.com",
        Source::YouTube => "www.youtube.com",
        Source::TikTok => "www.tiktok.com",
        Source::RedBook => "www.xiaohongshu.com",
        Source::Twitter => "cdn.syndication.twimg.com",
        Source::Instagram | Source::Threads => "i.instagram.com",
        Source::Vimeo => "player.vimeo.com",
        Source::Twitch => "gql.twitch.tv",
        Source::Reddit => "www.reddit.com",
        Source::Pinterest => "www.pinterest.com",
        Source::DailyMotion => "www.dailymotion.com",
        Source::QQVideo => "vv.video.qq.com",
        Source::Sohu => "api.tv.sohu.com",
        Source::CCTV => "vdn.apps.cntv.cn",
        Source::HuYa => "liveapi.huya.com",
        Source::HaoKan => "haokan.baidu.com",
        Source::WeiShi => "h5.weishi.qq.com",
        Source::QuanMin => "quanmin.hao222.com",
        Source::LiShiPin => "www.pearvideo.com",
        Source::PiPiXia => "api.pipix.com",
        Source::PiPiGaoXiao => "share.ippzone.com",
        Source::ZuiYou => "share.xiaochuankeji.cn",
        Source::DouPai => "v2.doupai.cc",
        Source::QuanMinKGe => "kg.qq.com",
        Source::SixRoom => "v.6.cn",
        Source::AcFun => "www.acfun.cn",
        Source::WeiBo => "h5.video.weibo.com",
        // 这几个第一跳就取决于用户给的链接，没有固定主机可以预热
        Source::KuaiShou
        | Source::LvZhou
        | Source::MeiPai
        | Source::XinPianChang
        | Source::Facebook => return None,
    })
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
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
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
        // 子串匹配会把这条当成抖音, 然后把抖音的请求头发给攻击者的服务器
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
    fn warmup_host_is_decided_for_every_source() {
        // 新增平台时必须显式表态：要么给出第一跳主机，要么写明"取决于链接"。
        // match 是穷尽的，所以漏了编译期就过不去——这条测的是给出的值本身合理。
        for s in Source::ALL.iter().copied() {
            if let Some(host) = warmup_host(s) {
                assert!(!host.is_empty(), "{s} 的预热主机是空的");
                assert!(!host.contains('/'), "{s} 的预热主机不该带路径: {host}");
                assert!(host.contains('.'), "{s} 的预热主机不像域名: {host}");
            }
        }
        // 抽查几个：预热的必须是解析器真正请求的接口主机，
        // 不是平台门户——预热错了等于没预热
        assert_eq!(warmup_host(Source::DouYin), Some("www.douyin.com"));
        assert_eq!(warmup_host(Source::BiliBili), Some("api.bilibili.com"));
        // 第一跳取决于用户给的链接，没有固定目标
        assert_eq!(warmup_host(Source::KuaiShou), None);
    }

    #[test]
    fn every_source_has_at_least_one_domain() {
        for s in supported() {
            assert!(!domains_of(s).is_empty(), "{s} 没有域名");
        }
    }
}
