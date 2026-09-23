//! 解析结果的数据模型。
//!
//! 字段名和 JSON 形状与原 Python 版 (`parse_video_py.parser.base`) 保持一致，
//! 上层 web / CLI 不用改序列化逻辑就能直接换过来。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 视频来源平台。
///
/// `as_str` 的取值就是对外 JSON 里的 `source` 字段，改动会破坏前端。
// 序列化刻意手写（见文件末尾）而不是用 `rename_all = "snake_case"`：
// 那个规则会把 `DouYin` 变成 `dou_yin`，和 `as_str()` 给的 `douyin` 对不上，
// 于是同一个平台在 JSON 里和在代码里是两个名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    // ---- 国内短视频 / 社区 ----
    /// 抖音 / 抖音火山版
    DouYin,
    /// 快手
    KuaiShou,
    /// 皮皮虾
    PiPiXia,
    /// 微博
    WeiBo,
    /// 微视
    WeiShi,
    /// 绿洲（微博旗下）
    LvZhou,
    /// 最右
    ZuiYou,
    /// 度小视（原全民小视频）
    QuanMin,
    /// 西瓜视频
    XiGua,
    /// 梨视频
    LiShiPin,
    /// 皮皮搞笑
    PiPiGaoXiao,
    /// 虎牙
    HuYa,
    /// AcFun（A 站）
    AcFun,
    /// 逗拍
    DouPai,
    /// 美拍
    MeiPai,
    /// 全民 K 歌
    QuanMinKGe,
    /// 六间房
    SixRoom,
    /// 新片场
    XinPianChang,
    /// 好看视频
    HaoKan,
    /// 哔哩哔哩
    BiliBili,
    /// 小红书
    RedBook,
    /// 腾讯视频
    QQVideo,
    /// 搜狐视频
    Sohu,
    /// 央视网
    CCTV,
    // ---- 海外 ----
    /// X（原 Twitter）
    Twitter,
    /// YouTube
    YouTube,
    /// TikTok
    TikTok,
    /// Instagram
    Instagram,
    /// Threads
    Threads,
    /// Vimeo
    Vimeo,
    /// Facebook
    Facebook,
    /// Twitch（Clip 和录像）
    Twitch,
    /// Reddit
    Reddit,
    /// Pinterest
    Pinterest,
    /// Dailymotion
    DailyMotion,
}

impl Source {
    /// 对外 JSON 里 `source` 字段的取值。改动会破坏前端。
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::DouYin => "douyin",
            Source::KuaiShou => "kuaishou",
            Source::PiPiXia => "pipixia",
            Source::WeiBo => "weibo",
            Source::WeiShi => "weishi",
            Source::LvZhou => "lvzhou",
            Source::ZuiYou => "zuiyou",
            Source::QuanMin => "quanmin",
            Source::XiGua => "xigua",
            Source::LiShiPin => "lishipin",
            Source::PiPiGaoXiao => "pipigaoxiao",
            Source::HuYa => "huya",
            Source::AcFun => "acfun",
            Source::DouPai => "doupai",
            Source::MeiPai => "meipai",
            Source::QuanMinKGe => "quanminkge",
            Source::SixRoom => "sixroom",
            Source::XinPianChang => "xinpianchang",
            Source::HaoKan => "haokan",
            Source::BiliBili => "bilibili",
            Source::RedBook => "redbook",
            Source::QQVideo => "qqvideo",
            Source::Sohu => "sohu",
            Source::CCTV => "cctv",
            Source::Twitter => "twitter",
            Source::YouTube => "youtube",
            Source::TikTok => "tiktok",
            Source::Instagram => "instagram",
            Source::Threads => "threads",
            Source::Vimeo => "vimeo",
            Source::Facebook => "facebook",
            Source::Twitch => "twitch",
            Source::Reddit => "reddit",
            Source::Pinterest => "pinterest",
            Source::DailyMotion => "dailymotion",
        }
    }

    /// 中文展示名，给 CLI / 错误信息用。
    pub const fn display_name(self) -> &'static str {
        match self {
            Source::DouYin => "抖音",
            Source::KuaiShou => "快手",
            Source::PiPiXia => "皮皮虾",
            Source::WeiBo => "微博",
            Source::WeiShi => "微视",
            Source::LvZhou => "绿洲",
            Source::ZuiYou => "最右",
            Source::QuanMin => "度小视",
            Source::XiGua => "西瓜视频",
            Source::LiShiPin => "梨视频",
            Source::PiPiGaoXiao => "皮皮搞笑",
            Source::HuYa => "虎牙",
            Source::AcFun => "AcFun",
            Source::DouPai => "逗拍",
            Source::MeiPai => "美拍",
            Source::QuanMinKGe => "全民K歌",
            Source::SixRoom => "六间房",
            Source::XinPianChang => "新片场",
            Source::HaoKan => "好看视频",
            Source::BiliBili => "哔哩哔哩",
            Source::RedBook => "小红书",
            Source::QQVideo => "腾讯视频",
            Source::Sohu => "搜狐视频",
            Source::CCTV => "央视网",
            Source::Twitter => "X (Twitter)",
            Source::YouTube => "YouTube",
            Source::TikTok => "TikTok",
            Source::Instagram => "Instagram",
            Source::Threads => "Threads",
            Source::Vimeo => "Vimeo",
            Source::Facebook => "Facebook",
            Source::Twitch => "Twitch",
            Source::Reddit => "Reddit",
            Source::Pinterest => "Pinterest",
            Source::DailyMotion => "Dailymotion",
        }
    }

    /// 对海外 / 机房 IP 不友好的平台。部署在境外时这些走 `ALCEDO_PROXY_CN`。
    pub const fn is_cn(self) -> bool {
        !matches!(
            self,
            Source::Twitter
                | Source::YouTube
                | Source::TikTok
                | Source::Instagram
                | Source::Threads
                | Source::Vimeo
                | Source::Facebook
                | Source::Twitch
                | Source::Reddit
                | Source::Pinterest
                | Source::DailyMotion
        )
    }
}

impl Source {
    /// 所有平台。`as_str` / `from_str` 和序列化都靠它保持同步。
    pub const ALL: &'static [Source] = &[
        Source::DouYin,
        Source::KuaiShou,
        Source::PiPiXia,
        Source::WeiBo,
        Source::WeiShi,
        Source::LvZhou,
        Source::ZuiYou,
        Source::QuanMin,
        Source::XiGua,
        Source::LiShiPin,
        Source::PiPiGaoXiao,
        Source::HuYa,
        Source::AcFun,
        Source::DouPai,
        Source::MeiPai,
        Source::QuanMinKGe,
        Source::SixRoom,
        Source::XinPianChang,
        Source::HaoKan,
        Source::BiliBili,
        Source::RedBook,
        Source::QQVideo,
        Source::Sohu,
        Source::CCTV,
        Source::Twitter,
        Source::YouTube,
        Source::TikTok,
        Source::Instagram,
        Source::Threads,
        Source::Vimeo,
        Source::Facebook,
        Source::Twitch,
        Source::Reddit,
        Source::Pinterest,
        Source::DailyMotion,
    ];

    /// `as_str` 的逆运算。
    ///
    /// 不叫 `from_str` 是为了不和 [`std::str::FromStr`] 撞名——那个 trait 也实现了，
    /// 见下面；两个同名方法在调用点很容易选错。
    pub fn from_id(s: &str) -> Option<Source> {
        Source::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

/// 让 `"douyin".parse::<Source>()` 能用。
impl std::str::FromStr for Source {
    type Err = UnknownSource;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Source::from_id(s).ok_or_else(|| UnknownSource(s.to_owned()))
    }
}

/// 解析平台标识失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSource(pub String);

impl std::fmt::Display for UnknownSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "未知的平台标识: {}", self.0)
    }
}

impl std::error::Error for UnknownSource {}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // 必须收 String 而不是 &str：`from_value` / 带转义的输入都给不出
        // 能借用的字符串，用 &str 会在运行时报 "expected a borrowed string"
        let raw = String::deserialize(d)?;
        Source::from_id(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("未知的平台标识: {raw}")))
    }
}

/// 视频作者信息。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    /// 作者 ID
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uid: String,
    /// 作者昵称
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// 作者头像
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub avatar: String,
}

impl Author {
    /// 三个字段都给。
    pub fn new(uid: impl Into<String>, name: impl Into<String>, avatar: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            name: name.into(),
            avatar: avatar.into(),
        }
    }

    /// 只知道昵称时用。
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// 图集中的一张图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// 图片地址
    pub url: String,
    /// 实况照片 (Live Photo) 对应的短视频地址
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub live_photo_url: String,
}

impl Image {
    /// 一张普通图片（不带实况视频）。
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            live_photo_url: String::new(),
        }
    }
}

/// 一档清晰度。
///
/// `url` 非空表示可直接下载的直链；为空而 `video_url` 非空，表示音视频分离
/// （DASH，如 YouTube 1080p 以上），需要上层用 ffmpeg 合并。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Format {
    /// 展示名，如 `1080p` / `1080p H.265` / `音频`
    pub label: String,
    /// 直链；为空表示需要服务端合并
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// 容器扩展名
    pub ext: String,
    /// 视频短边高度；纯音频为 0
    pub height: u32,
    /// 预估体积（字节），未知为 0
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub filesize: u64,
    /// 编码说明，如 `H.265` / `AV1`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub codec: String,
    /// 需要合并时：视频轨地址
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub video_url: String,
    /// 需要合并时：音频轨地址
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub audio_url: String,
}

/// 用来当"清晰度"的那条边。
///
/// 竖屏视频的 `height` 是长边——1080×1920 的短视频按高度算是"1920p"，没人这么说。
/// 用户认知里的清晰度一直是短边，横屏竖屏都一样。
///
/// 这条规则原来在抖音、快手、TikTok、B 站各写了一份，改一处别处不会跟着动。
pub fn short_side(width: u32, height: u32) -> u32 {
    if width > 0 && height > 0 {
        width.min(height)
    } else {
        width.max(height)
    }
}

impl Format {
    /// 拼展示名：平台给了清晰度名就用它，没有就按短边拼 `720p`，
    /// 编码非空时缀在后面。
    pub fn label_for(quality: &str, short_side: u32, codec: &str) -> String {
        let base = if quality.is_empty() {
            if short_side > 0 {
                format!("{short_side}p")
            } else {
                "未知".to_owned()
            }
        } else {
            quality.to_owned()
        };
        if codec.is_empty() {
            base
        } else {
            format!("{base} {codec}")
        }
    }

    /// 一档可直接下载的 mp4。
    pub fn direct(label: impl Into<String>, url: impl Into<String>, height: u32) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
            ext: "mp4".into(),
            height,
            ..Default::default()
        }
    }

    /// 是否需要上层合并音视频
    pub fn needs_merge(&self) -> bool {
        self.url.is_empty() && !self.video_url.is_empty()
    }
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

/// 解析结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    /// 默认播放 / 下载地址（浏览器能直接播的那一档）
    #[serde(default)]
    pub video_url: String,
    /// 封面
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
    /// 标题 / 正文
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// 背景音乐地址
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub music_url: String,
    /// 图集
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Image>,
    /// 作者信息
    #[serde(default)]
    pub author: Author,

    /// 来源平台标识
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// 原始页面地址
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub page_url: String,
    /// 时长（秒），未知为 0
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub duration: f64,
    /// 默认档的宽，未知为 0
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub width: u32,
    /// 默认档的高，未知为 0
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub height: u32,
    /// 其余清晰度档位
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<Format>,
    /// 访问直链时必须附带的请求头（Referer / Cookie 等）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub video_headers: BTreeMap<String, String>,
}

impl VideoInfo {
    /// 一条内容都没解析出来
    pub fn is_empty(&self) -> bool {
        self.video_url.is_empty()
            && self.images.is_empty()
            && self.music_url.is_empty()
            && self.formats.is_empty()
    }

    /// 图集（没有视频、但有图）
    pub fn is_gallery(&self) -> bool {
        self.video_url.is_empty() && !self.images.is_empty()
    }

    /// 加一个访问直链时必须附带的请求头。
    pub fn set_header(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.video_headers.insert(k.into(), v.into());
    }

    /// 按短边从高到低排好序，并去掉重复档位。
    ///
    /// 各家解析器塞进来的顺序五花八门，前端要的是"第一档就是最清晰的"。
    pub fn normalize_formats(&mut self) {
        self.formats
            .retain(|f| !f.url.is_empty() || f.needs_merge());
        self.formats.sort_by(|a, b| {
            b.height
                .cmp(&a.height)
                .then_with(|| a.codec.cmp(&b.codec))
                .then_with(|| b.filesize.cmp(&a.filesize))
        });
        let mut seen: Vec<(u32, &str, &str)> = Vec::with_capacity(self.formats.len());
        let mut keep = Vec::with_capacity(self.formats.len());
        for f in &self.formats {
            let key = (f.height, f.codec.as_str(), f.ext.as_str());
            keep.push(!seen.contains(&key));
            seen.push(key);
        }
        let mut it = keep.into_iter();
        self.formats.retain(|_| it.next().unwrap_or(true));
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn source_str_roundtrip() {
        let mut seen: Vec<&str> = Vec::new();
        for s in Source::ALL.iter().copied() {
            let id = s.as_str();
            assert!(!seen.contains(&id), "重复的 source 标识: {id}");
            seen.push(id);
            assert_eq!(Source::from_id(id), Some(s));
            assert!(!s.display_name().is_empty(), "{id} 没有展示名");
        }
        assert_eq!(Source::from_id("nope"), None);
        // FromStr 也要通
        assert_eq!("douyin".parse::<Source>().unwrap(), Source::DouYin);
        assert!("nope".parse::<Source>().is_err());
    }

    #[test]
    fn serialised_name_matches_as_str() {
        // 用 serde 的 rename_all="snake_case" 时 DouYin 会变成 "dou_yin"，
        // 于是 JSON 里的名字和 as_str() 给的名字是两个东西，前端按哪个都可能错
        for s in Source::ALL.iter().copied() {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()), "{s} 的序列化名不一致");
            let back: Source = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn deserialises_from_owned_and_borrowed_input() {
        // from_value 走的是 owned 路径, 早先按 &str 收会在这里炸
        let v = serde_json::json!("douyin");
        assert_eq!(serde_json::from_value::<Source>(v).unwrap(), Source::DouYin);
        assert_eq!(
            serde_json::from_str::<Source>("\"youtube\"").unwrap(),
            Source::YouTube
        );
        assert!(serde_json::from_str::<Source>("\"nope\"").is_err());
    }

    #[test]
    fn all_list_is_complete() {
        // 漏一个的话 from_str 和反序列化就会对它失效, 但编译器不会提醒
        assert_eq!(Source::ALL.len(), 35, "新增平台后要同步 Source::ALL");
    }

    #[test]
    fn short_side_uses_the_narrow_edge() {
        // 竖屏 1080x1920 是 1080p，不是 1920p
        assert_eq!(short_side(1080, 1920), 1080);
        // 横屏也一样
        assert_eq!(short_side(1920, 1080), 1080);
        // 只给一边时用那一边，别返回 0
        assert_eq!(short_side(0, 720), 720);
        assert_eq!(short_side(720, 0), 720);
        assert_eq!(short_side(0, 0), 0);
    }

    #[test]
    fn label_prefers_platform_name_then_falls_back() {
        assert_eq!(Format::label_for("1080P60", 1080, ""), "1080P60");
        assert_eq!(Format::label_for("", 720, ""), "720p");
        assert_eq!(Format::label_for("", 720, "H.265"), "720p H.265");
        assert_eq!(Format::label_for("4K", 2160, "AV1"), "4K AV1");
        assert_eq!(Format::label_for("", 0, ""), "未知");
    }

    #[test]
    fn normalize_sorts_and_dedups() {
        let mut vi = VideoInfo {
            formats: vec![
                Format::direct("720p", "u1", 720),
                Format::direct("1080p", "u2", 1080),
                Format::direct("720p", "u3", 720),
                Format {
                    url: String::new(),
                    ..Format::direct("坏档", "", 480)
                },
            ],
            ..Default::default()
        };
        vi.normalize_formats();
        assert_eq!(vi.formats.len(), 2);
        assert_eq!(vi.formats[0].height, 1080);
        assert_eq!(vi.formats[1].url, "u1");
    }

    #[test]
    fn cn_sources_exclude_overseas() {
        assert!(Source::DouYin.is_cn());
        assert!(Source::BiliBili.is_cn());
        assert!(!Source::YouTube.is_cn());
        assert!(!Source::Twitter.is_cn());
    }
}
