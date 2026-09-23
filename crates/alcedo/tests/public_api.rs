//! 对外契约的集成测试：从 crate 外面看到的行为。
//!
//! 这里不发网络请求——真正打平台接口的冒烟测试在 `smoke.rs`，默认 `#[ignore]`。

use alcedo::{Client, Config, Reason, Source};

#[test]
fn every_source_is_reachable_from_some_domain() {
    // 注册表里漏登记域名的平台等于永远不会被调用到
    for s in alcedo::registry::supported() {
        assert!(
            !alcedo::registry::domains_of(s).is_empty(),
            "{s} 没有登记任何域名，永远不会被识别出来"
        );
    }
}

#[test]
fn source_identifiers_are_unique_and_stable() {
    // as_str 是对外 JSON 契约，重名会让上层把两个平台混起来
    let all = alcedo::registry::supported();
    let mut seen: Vec<&str> = Vec::new();
    for s in &all {
        let id = s.as_str();
        assert!(!seen.contains(&id), "重复的 source 标识: {id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "source 标识只能用小写字母和数字: {id}"
        );
        seen.push(id);
    }
    assert!(all.len() >= 30, "支持的平台数量不对: {}", all.len());
}

#[test]
fn detection_covers_the_url_shapes_users_actually_paste() {
    let cases = [
        ("https://v.douyin.com/iRNBho6u/", Source::DouYin),
        (
            "https://www.douyin.com/video/7424432820954598707",
            Source::DouYin,
        ),
        ("https://v.kuaishou.com/abc", Source::KuaiShou),
        ("https://www.xiaohongshu.com/explore/abc", Source::RedBook),
        ("https://xhslink.com/a/abcdef", Source::RedBook),
        ("https://b23.tv/abcdef", Source::BiliBili),
        (
            "https://www.bilibili.com/video/BV1xx411c7mD",
            Source::BiliBili,
        ),
        ("https://youtu.be/dQw4w9WgXcQ", Source::YouTube),
        (
            "https://www.youtube.com/shorts/abcdefghijk",
            Source::YouTube,
        ),
        ("https://www.tiktok.com/@user/video/123", Source::TikTok),
        ("https://vm.tiktok.com/ZSabcdef/", Source::TikTok),
        ("https://x.com/user/status/123", Source::Twitter),
        (
            "https://www.instagram.com/reel/CxYzAbCdEfG/",
            Source::Instagram,
        ),
        (
            "https://www.reddit.com/r/videos/comments/abc/x/",
            Source::Reddit,
        ),
        ("https://vimeo.com/76979871", Source::Vimeo),
        ("https://www.twitch.tv/videos/123456", Source::Twitch),
    ];
    for (url, want) in cases {
        assert_eq!(alcedo::registry::detect(url), Some(want), "识别错了: {url}");
    }
}

#[test]
fn lookalike_domains_are_not_matched() {
    // 子串匹配会把这些认成真平台，然后把平台的请求头发给攻击者
    for url in [
        "https://evil.com/?next=https://www.douyin.com/video/1",
        "https://douyin.com.evil.net/video/1",
        "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
        "https://bilibili.com.cn.attacker.io/video/BV1",
    ] {
        assert_eq!(alcedo::registry::detect(url), None, "不该被识别: {url}");
    }
}

#[tokio::test]
async fn unsupported_and_malformed_input_fails_fast_without_network() {
    let c = Client::with_config(Config::default());

    for input in ["https://example.com/watch/1", "今天天气不错", ""] {
        let err = c.parse(input).await.unwrap_err();
        assert_eq!(err.reason, Reason::Unsupported, "输入: {input:?}");
    }
}

#[tokio::test]
async fn internal_addresses_are_refused() {
    // 解析器会跟着用户给的链接走，指向内网的地址必须在发请求之前就被拒
    let c = Client::with_config(Config::default());
    // 这些主机名注册表认不出来，先被 Unsupported 挡掉；真正的 SSRF 防线在
    // http::ssrf 的单元测试里覆盖。这里确认的是它们绝不会被当成某个平台。
    for url in [
        "http://127.0.0.1/video/1",
        "http://169.254.169.254/latest/meta-data/",
        "http://localhost:8080/x",
    ] {
        let err = c.parse(url).await.unwrap_err();
        assert!(
            matches!(err.reason, Reason::Unsupported | Reason::Network),
            "{url} 得到了意外的原因: {}",
            err.reason
        );
    }
}

#[tokio::test]
async fn id_based_parsing_rejects_platforms_that_need_a_full_link() {
    let c = Client::with_config(Config::default());
    for s in [Source::RedBook, Source::KuaiShou, Source::Reddit] {
        let err = c.parse_id(s, "someid").await.unwrap_err();
        assert_eq!(err.reason, Reason::Unsupported, "{s}");
        assert!(err.detail.contains("完整分享链接"), "{s} 的提示不够明确");
    }
}

#[test]
fn error_reasons_all_have_user_facing_text() {
    let reasons = [
        Reason::Deleted,
        Reason::Login,
        Reason::Blocked,
        Reason::Unsupported,
        Reason::Network,
        Reason::Timeout,
        Reason::Empty,
        Reason::Restricted,
        Reason::Parse,
    ];
    for r in reasons {
        assert!(!r.message().is_empty(), "{r} 没有给用户看的文案");
        assert!(!r.as_str().is_empty());
    }
    // 只有 parse 代表"解析器该更新了"，别的都是外部状况；站长只需要盯这一个
    assert!(!Reason::Deleted.is_retryable());
    assert!(!Reason::Unsupported.is_retryable());
}

#[test]
fn video_info_serialises_to_the_documented_shape() {
    let info = alcedo::VideoInfo {
        video_url: "https://cdn/v.mp4".into(),
        title: "标题".into(),
        source: Some(Source::DouYin),
        formats: vec![alcedo::Format {
            label: "1080p".into(),
            url: String::new(),
            ext: "mp4".into(),
            height: 1080,
            video_url: "https://cdn/v.m4s".into(),
            audio_url: "https://cdn/a.m4s".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let json: serde_json::Value = serde_json::to_value(&info).unwrap();

    assert_eq!(json["source"], "douyin");
    assert_eq!(json["video_url"], "https://cdn/v.mp4");
    // 空字段不该出现在输出里，前端按"有没有这个 key"判断分支
    assert!(json.get("music_url").is_none());
    assert!(json.get("images").is_none());
    // 需要合并的档位：url 缺席，video_url / audio_url 在
    let f = &json["formats"][0];
    assert!(f.get("url").is_none());
    assert_eq!(f["video_url"], "https://cdn/v.m4s");

    // 能原样读回来
    let back: alcedo::VideoInfo = serde_json::from_value(json).unwrap();
    assert_eq!(back, info);
}
