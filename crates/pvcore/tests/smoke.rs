//! 真打平台接口的冒烟测试。
//!
//! 全部 `#[ignore]`：这些用例依赖外部服务，会因为对方改版、限流、地区封锁而红，
//! 不该卡住 CI。但它们是唯一能证明"解析器对着**现在的**线上站点还管用"的东西——
//! 单元测试用的是固定样本，平台改了结构它们照样全绿。
//!
//! ```bash
//! cargo test -p pvcore --test smoke -- --ignored --nocapture
//! cargo test -p pvcore --test smoke -- --ignored bilibili --nocapture   # 只跑一个
//! ```
//!
//! 国内平台在境外机器上跑需要 `PV_PROXY_CN`，YouTube / X 这些在国内需要 `PV_PROXY`。

use pvcore::{Client, VideoInfo};

async fn parse(url: &str) -> VideoInfo {
    let client = Client::new().expect("初始化失败");
    match client.parse(url).await {
        Ok(info) => {
            println!("\n=== {url}");
            println!("  标题   : {}", info.title);
            println!("  作者   : {}", info.author.name);
            println!("  时长   : {:.1}s", info.duration);
            println!("  视频   : {}", truncate(&info.video_url));
            println!("  图集   : {} 张", info.images.len());
            for f in &info.formats {
                println!(
                    "  档位   : {} {}",
                    f.label,
                    if f.needs_merge() { "(需合并)" } else { "" }
                );
            }
            info
        }
        Err(e) => panic!("{url} 解析失败 [{}] {e}", e.reason),
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= 100 {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(100).collect::<String>())
    }
}

/// 一条解析结果至少要能用：有内容、有标题、地址是 http(s)。
fn assert_usable(info: &VideoInfo) {
    assert!(
        !info.video_url.is_empty() || !info.images.is_empty() || !info.formats.is_empty(),
        "什么都没解析出来"
    );
    if !info.video_url.is_empty() {
        assert!(
            info.video_url.starts_with("http"),
            "视频地址不是 http(s): {}",
            info.video_url
        );
    }
    for f in &info.formats {
        assert!(
            !f.url.is_empty() || f.needs_merge(),
            "档位 {} 既没有直链也没有分离轨",
            f.label
        );
    }
}

#[tokio::test]
#[ignore = "要联网"]
async fn bilibili() {
    let info = parse("https://www.bilibili.com/video/BV1GJ411x7h7").await;
    assert_usable(&info);
    assert!(!info.title.is_empty());
    assert!(!info.author.name.is_empty());
    // 直链不带 Referer 会被 CDN 403
    assert_eq!(
        info.video_headers.get("Referer").map(String::as_str),
        Some("https://www.bilibili.com/")
    );
    // 浏览器要能直接播，不能只给分离轨
    assert!(!info.video_url.is_empty(), "缺少可直接播放的地址");
}

#[tokio::test]
#[ignore = "要联网"]
async fn youtube() {
    let info = parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").await;
    assert_usable(&info);
    assert!(
        info.duration > 200.0 && info.duration < 220.0,
        "时长不对: {}",
        info.duration
    );
    // 不跑 JS 也该拿到完整的清晰度阶梯。
    //
    // 这条红了先看是不是出口 IP 被 YouTube 限流了：被限流时所有 InnerTube 客户端
    // 都只下发 HLS 清单，解析器会退化成"能播但没档位"。换个出口或配
    // PV_YOUTUBE_COOKIE 再跑一次，还是红才是解析器的问题。
    assert!(
        info.formats.len() > 5,
        "只拿到 {} 个档位（video_url 是否为 manifest.googlevideo.com？那就是被限流了）",
        info.formats.len()
    );
    assert!(
        info.formats.iter().any(|f| f.height >= 1080),
        "没有 1080p 以上"
    );
    assert!(
        info.formats.iter().any(|f| f.label.contains("音频")),
        "没有纯音频档"
    );
}

#[tokio::test]
#[ignore = "要联网"]
async fn acfun() {
    let info = parse("https://www.acfun.cn/v/ac36935385").await;
    assert_usable(&info);
    assert!(info.video_url.contains(".m3u8"), "AcFun 给的应该是 HLS");
    assert!(!info.author.name.is_empty());
}

#[tokio::test]
#[ignore = "要联网"]
async fn pearvideo() {
    let info = parse("https://www.pearvideo.com/detail_1742158").await;
    assert_usable(&info);
    assert!(!info.title.is_empty(), "标题是并发从详情页取的，不该为空");
    assert!(
        info.video_url.contains("cont-"),
        "时间戳没换成 cont-<id>，地址是打不开的"
    );
}

#[tokio::test]
#[ignore = "要联网"]
async fn vimeo() {
    let info = parse("https://vimeo.com/76979871").await;
    assert_usable(&info);
    assert!(!info.title.is_empty());
}

#[tokio::test]
#[ignore = "要联网"]
async fn dailymotion() {
    let info = parse("https://www.dailymotion.com/video/xb9ctgi").await;
    assert_usable(&info);
}

/// 不存在的内容要报 `deleted`，不能报成"解析器过期"。
#[tokio::test]
#[ignore = "要联网"]
async fn missing_content_is_classified_as_deleted() {
    let client = Client::new().unwrap();
    let err = client
        .parse("https://www.youtube.com/watch?v=aaaaaaaaaaa")
        .await
        .expect_err("这个 ID 不该存在");
    println!("得到: [{}] {err}", err.reason);
    assert_ne!(
        err.reason,
        pvcore::Reason::Parse,
        "内容不存在被误报成解析器过期，站长会去查一个不存在的问题"
    );
}
