//! YouTube。
//!
//! 走 InnerTube 的 `youtubei/v1/player`，不解析网页、不跑 JS。
//!
//! ## 为什么不用 web 客户端
//!
//! web 客户端返回的地址带 `signatureCipher` / `n` 参数，必须执行 YouTube 下发的
//! 那段混淆 JS 才能还原，等于要在解析器里塞一个 JS 引擎；2024 年之后 web 还额外
//! 要求 PO Token（BotGuard 产物）。而 **移动端 / TV 客户端返回的是已经签好名的
//! 直链**——同样的内容，零 JS、零 token。
//!
//! 客户端按"不需要 JS 且当前仍免 PO Token"的顺序试：`visionos` 是 yt-dlp 2026
//! 之后的默认首选，`ios` / `android_vr` / `tv` 作为后备。哪天 YouTube 对某个客户端
//! 加了限制，只要挪一下 [`CLIENTS`] 的顺序，不用改解析逻辑。

use serde_json::{json, Value};

use crate::error::{Error, Reason, Result};
use crate::http::{Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

const INNERTUBE: &str = "https://www.youtube.com/youtubei/v1/player";

/// 一个 InnerTube 客户端的身份。
struct ClientSpec {
    name: &'static str,
    /// `X-YouTube-Client-Name` 用的数字
    id: u16,
    version: &'static str,
    user_agent: &'static str,
    /// 塞进 `context.client` 的额外字段
    extra: &'static str,
}

/// 按优先级排列。前面的都是"不需要 JS 播放器"的客户端。
static CLIENTS: &[ClientSpec] = &[
    ClientSpec {
        name: "VISIONOS",
        id: 101,
        version: "1.02",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
        extra: r#""deviceMake":"Apple","deviceModel":"RealityDevice17,1","osName":"visionOS","osVersion":"26.5.23O471""#,
    },
    ClientSpec {
        name: "IOS",
        id: 5,
        version: "21.26.4",
        user_agent: "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
        extra: r#""deviceMake":"Apple","deviceModel":"iPhone16,2","osName":"iPhone","osVersion":"18.3.2.22D82""#,
    },
    ClientSpec {
        name: "ANDROID_VR",
        id: 28,
        version: "1.65.10",
        user_agent: "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
        extra: r#""deviceMake":"Oculus","deviceModel":"Quest 3","androidSdkVersion":32,"osName":"Android","osVersion":"12L""#,
    },
    ClientSpec {
        name: "TVHTML5",
        id: 7,
        version: "7.20260707.07.00",
        user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko) Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)",
        extra: "",
    },
];

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let id =
        video_id_from_url(url).ok_or_else(|| Error::unsupported("链接里没有 YouTube 视频 ID"))?;
    parse_id(http, &id).await
}

pub async fn parse_id(http: &Http, video_id: &str) -> Result<VideoInfo> {
    if !is_valid_id(video_id) {
        return Err(Error::unsupported(format!(
            "不是合法的 YouTube 视频 ID: {video_id}"
        )));
    }

    let mut last: Option<Error> = None;
    // 只拿到 HLS 清单、没有可下载档位的结果。能播但没法选清晰度、没法下载，
    // 所以先扣下来继续问下一个客户端，全都不行了再拿它顶上。
    let mut degraded: Option<VideoInfo> = None;

    for client in CLIENTS {
        let outcome = match player(http, client, video_id).await {
            Ok(resp) => build(&resp, video_id),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(info) if !info.formats.is_empty() => return Ok(info),
            Ok(info) => {
                tracing::debug!(client = client.name, "只拿到 HLS，继续问下一个客户端");
                degraded.get_or_insert(info);
            }
            // 视频真的没了，换几个客户端也是一样的答案，别让用户干等
            Err(e) if e.reason == Reason::Deleted || e.reason == Reason::Unsupported => {
                return Err(e)
            }
            // 其余情况（PO Token、机器人校验、限流）都是**针对这个客户端**的，
            // 换一个很可能就过了——YouTube 的风控是按客户端下发的。
            Err(e) => {
                tracing::debug!(client = client.name, error = %e, "换下一个 InnerTube 客户端");
                last = Some(e);
            }
        }
    }

    if let Some(info) = degraded {
        return Ok(info);
    }
    Err(last.unwrap_or_else(|| Error::parse("所有 InnerTube 客户端都没拿到播放数据")))
}

async fn player(http: &Http, client: &ClientSpec, video_id: &str) -> Result<Value> {
    // 手拼 context 里的 client 段：extra 是各客户端特有的字段，
    // 拼字符串比为每个客户端造一个 struct 省事得多。
    //
    // hl 固定 en：`playabilityStatus.reason` 的文案要拿来分类，跟着语言变的话
    // 匹配规则就得跟着 YouTube 的每一种翻译走。面向用户的中文文案是我们自己出的，
    // 这里只需要一份稳定的英文原文。
    let client_json = format!(
        r#"{{"clientName":"{}","clientVersion":"{}","hl":"en","gl":"US"{}{}}}"#,
        client.name,
        client.version,
        if client.extra.is_empty() { "" } else { "," },
        client.extra
    );
    let client_value: Value = serde_json::from_str(&client_json)?;

    let body = json!({
        "context": { "client": client_value },
        "videoId": video_id,
        // 这两个是"我知道内容可能敏感，照给"，少了会对部分视频返回空
        "contentCheckOk": true,
        "racyCheckOk": true,
    });

    let mut req = Req::post(INNERTUBE)
        .header("User-Agent", client.user_agent)
        .header("X-YouTube-Client-Name", client.id.to_string())
        .header("X-YouTube-Client-Version", client.version)
        .header("Origin", "https://www.youtube.com")
        .header("Content-Type", "application/json")
        .json_body(&body);
    if let Some(c) = http.config().youtube_cookie.as_deref() {
        req = req.cookie_header(c);
    }

    let resp = http.send(req).await?;
    resp.error_for_status()?;
    resp.json()
}

fn build(resp: &Value, video_id: &str) -> Result<VideoInfo> {
    check_playability(resp)?;

    let details = util::get(resp, &["videoDetails"])
        .cloned()
        .unwrap_or_default();
    let streaming = util::get(resp, &["streamingData"])
        .cloned()
        .unwrap_or_default();

    // 直播：只有 HLS，没有可下载的分段
    let hls = util::str_at(&streaming, &["hlsManifestUrl"]);

    // progressive：音视频合一，浏览器能直接播。YouTube 现在只给到 360p/720p。
    let progressive: Vec<&Value> = util::arr_at(&streaming, &["formats"])
        .iter()
        .filter(|f| has_plain_url(f))
        .collect();

    let best_progressive = progressive
        .iter()
        .max_by_key(|f| util::u32_at(f, &["height"]))
        .copied();

    let mut info = VideoInfo {
        video_url: best_progressive
            .map(|f| util::str_at(f, &["url"]))
            .unwrap_or_else(|| hls.clone()),
        cover_url: best_thumbnail(&details, video_id),
        title: util::str_at(&details, &["title"]),
        duration: util::num_at(&details, &["lengthSeconds"]),
        width: best_progressive
            .map(|f| util::u32_at(f, &["width"]))
            .unwrap_or(0),
        height: best_progressive
            .map(|f| util::u32_at(f, &["height"]))
            .unwrap_or(0),
        author: Author::new(
            util::str_at(&details, &["channelId"]),
            util::str_at(&details, &["author"]),
            String::new(),
        ),
        ..Default::default()
    };

    info.formats = collect_formats(&streaming, &progressive);

    if info.is_empty() {
        // 走到这儿说明这个客户端被要求 PO Token 了：换下一个客户端还有戏
        return Err(Error::bare(Reason::Empty));
    }
    Ok(info)
}

/// `playabilityStatus` 决定了这条内容到底能不能看。
fn check_playability(resp: &Value) -> Result<()> {
    let status = util::str_at(resp, &["playabilityStatus", "status"]);
    if status.is_empty() || status == "OK" {
        return Ok(());
    }
    let reason = util::first_str(
        resp,
        &[
            &["playabilityStatus", "reason"],
            &["playabilityStatus", "messages", "0"],
            &[
                "playabilityStatus",
                "errorScreen",
                "playerErrorMessageRenderer",
                "reason",
                "simpleText",
            ],
        ],
    );

    Err(match status.as_str() {
        "LOGIN_REQUIRED" => {
            // 这个状态底下是三种完全不同的情况，只能靠文案区分——所以 context 里的
            // hl 必须固定成 en（见 `player`），跟着界面语言变就没法匹配了。
            if reason.contains("not a bot") || reason.contains("confirm you're not") {
                // 出口 IP 被判成机器人。换个客户端常常就过了，真过不了才需要 cookie
                Error::blocked(format!(
                    "YouTube 要求确认服务器 IP 不是机器人（{reason}），换个出口或配置 PV_YOUTUBE_COOKIE"
                ))
            } else if reason.contains("age") || reason.contains("Sign in to confirm your age") {
                Error::restricted(format!("年龄限制视频，需要登录：{reason}"))
            } else {
                Error::login(reason)
            }
        }
        "UNPLAYABLE" => Error::restricted(reason),
        "ERROR" => Error::deleted(reason),
        "LIVE_STREAM_OFFLINE" => Error::restricted("直播还没开始或已结束"),
        "AGE_CHECK_REQUIRED" => Error::restricted(format!("需要年龄验证：{reason}")),
        other => Error::restricted(format!("YouTube 返回 {other}：{reason}")),
    })
}

/// 这条 format 是不是拿来就能用的直链。
///
/// 带 `signatureCipher` 的要跑 JS 解签，我们直接跳过——用对客户端就不会遇到。
fn has_plain_url(f: &Value) -> bool {
    !util::str_at(f, &["url"]).is_empty()
        && util::get(f, &["signatureCipher"]).is_none()
        && util::get(f, &["cipher"]).is_none()
}

fn collect_formats(streaming: &Value, progressive: &[&Value]) -> Vec<Format> {
    let adaptive = util::arr_at(streaming, &["adaptiveFormats"]);

    // 音频轨：挑码率最高的一条，给所有需要合并的清晰度共用
    let best_audio = adaptive
        .iter()
        .filter(|f| has_plain_url(f) && util::str_at(f, &["mimeType"]).starts_with("audio/"))
        .max_by_key(|f| util::u64_at(f, &["bitrate"]));

    let mut out = Vec::new();

    // 合一的档位：直接能下
    for f in progressive {
        let h = util::u32_at(f, &["height"]);
        out.push(Format {
            label: quality_label(f, h, ""),
            url: util::str_at(f, &["url"]),
            ext: ext_of(f),
            height: h,
            filesize: util::u64_at(f, &["contentLength"]),
            ..Default::default()
        });
    }

    // 分离的档位：需要上层合并
    if let Some(audio) = best_audio {
        let audio_url = util::str_at(audio, &["url"]);
        for f in adaptive {
            if !has_plain_url(f) || !util::str_at(f, &["mimeType"]).starts_with("video/") {
                continue;
            }
            let h = util::u32_at(f, &["height"]);
            let codec = codec_name(&util::str_at(f, &["mimeType"]));
            out.push(Format {
                label: quality_label(f, h, codec),
                url: String::new(),
                ext: "mp4".into(),
                height: h,
                filesize: util::u64_at(f, &["contentLength"])
                    + util::u64_at(audio, &["contentLength"]),
                codec: codec.to_owned(),
                video_url: util::str_at(f, &["url"]),
                audio_url: audio_url.clone(),
            });
        }

        // 纯音频也给一档，很多人只想要声音
        out.push(Format {
            label: "仅音频".into(),
            url: audio_url,
            ext: if util::str_at(audio, &["mimeType"]).contains("mp4a") {
                "m4a".into()
            } else {
                "webm".into()
            },
            height: 0,
            filesize: util::u64_at(audio, &["contentLength"]),
            codec: String::new(),
            ..Default::default()
        });
    }

    out
}

fn quality_label(f: &Value, height: u32, codec: &str) -> String {
    let base = {
        let q = util::str_at(f, &["qualityLabel"]);
        if q.is_empty() && height > 0 {
            format!("{height}p")
        } else if q.is_empty() {
            "未知".to_string()
        } else {
            q
        }
    };
    if codec.is_empty() {
        base
    } else {
        format!("{base} {codec}")
    }
}

fn codec_name(mime: &str) -> &'static str {
    if mime.contains("av01") {
        "AV1"
    } else if mime.contains("vp9") || mime.contains("vp09") {
        "VP9"
    } else {
        ""
    }
}

fn ext_of(f: &Value) -> String {
    let mime = util::str_at(f, &["mimeType"]);
    if mime.contains("webm") {
        "webm".into()
    } else {
        "mp4".into()
    }
}

fn best_thumbnail(details: &Value, video_id: &str) -> String {
    util::arr_at(details, &["thumbnail", "thumbnails"])
        .iter()
        .max_by_key(|t| util::u32_at(t, &["width"]))
        .map(|t| util::str_at(t, &["url"]))
        .filter(|u| !u.is_empty())
        // 缩略图接口给不出来时，maxres 这个地址对绝大多数视频都在
        .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{video_id}/maxresdefault.jpg"))
}

/// 从各种形态的 YouTube 地址里取出 11 位视频 ID。
fn video_id_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();

    // youtu.be/<id>
    if host == "youtu.be" {
        return parsed
            .path_segments()?
            .find(|s| !s.is_empty())
            .map(str::to_owned)
            .filter(|s| is_valid_id(s));
    }

    // watch?v=<id>
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "v") {
        if is_valid_id(&v) {
            return Some(v.into_owned());
        }
    }

    // /shorts/<id>, /embed/<id>, /live/<id>, /v/<id>
    let segs: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    for (i, s) in segs.iter().enumerate() {
        if matches!(*s, "shorts" | "embed" | "live" | "v") {
            return segs
                .get(i + 1)
                .map(|s| s.to_string())
                .filter(|s| is_valid_id(s));
        }
    }
    None
}

/// YouTube 的视频 ID 固定 11 位 base64url 字符。
fn is_valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_id_from_every_url_shape() {
        let cases = [
            ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            ("https://youtu.be/dQw4w9WgXcQ?t=30", "dQw4w9WgXcQ"),
            ("https://www.youtube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            ("https://www.youtube.com/embed/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            ("https://www.youtube.com/live/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            (
                "https://m.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1",
                "dQw4w9WgXcQ",
            ),
        ];
        for (url, want) in cases {
            assert_eq!(video_id_from_url(url).as_deref(), Some(want), "{url}");
        }
    }

    #[test]
    fn rejects_non_video_urls() {
        assert_eq!(video_id_from_url("https://www.youtube.com/@channel"), None);
        assert_eq!(
            video_id_from_url("https://www.youtube.com/watch?v=tooshort"),
            None
        );
    }

    #[test]
    fn id_validation() {
        assert!(is_valid_id("dQw4w9WgXcQ"));
        assert!(is_valid_id("_-aBcDeFgH1"));
        assert!(!is_valid_id("dQw4w9WgXc")); // 10 位
        assert!(!is_valid_id("dQw4w9WgXc!"));
    }

    #[test]
    fn ciphered_formats_are_skipped() {
        let f = serde_json::json!({"url": "https://x/a", "signatureCipher": "s=..."});
        assert!(
            !has_plain_url(&f),
            "带 signatureCipher 的要跑 JS, 不能当直链用"
        );
        let ok = serde_json::json!({"url": "https://x/a"});
        assert!(has_plain_url(&ok));
    }

    #[test]
    fn playability_maps_to_reasons() {
        let cases = [
            (
                r#"{"playabilityStatus":{"status":"ERROR","reason":"Video unavailable"}}"#,
                Reason::Deleted,
            ),
            (
                r#"{"playabilityStatus":{"status":"UNPLAYABLE","reason":"Private video"}}"#,
                Reason::Restricted,
            ),
            (
                r#"{"playabilityStatus":{"status":"LOGIN_REQUIRED","reason":"Sign in to confirm your age"}}"#,
                Reason::Restricted,
            ),
            // 机器人校验是针对出口 IP 的, 归成 blocked 才能提示换出口而不是"内容受限"
            (
                r#"{"playabilityStatus":{"status":"LOGIN_REQUIRED","reason":"Sign in to confirm you're not a bot"}}"#,
                Reason::Blocked,
            ),
            (
                r#"{"playabilityStatus":{"status":"LOGIN_REQUIRED","reason":"This video is private"}}"#,
                Reason::Login,
            ),
            (
                r#"{"playabilityStatus":{"status":"LIVE_STREAM_OFFLINE"}}"#,
                Reason::Restricted,
            ),
        ];
        for (raw, want) in cases {
            let v: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(check_playability(&v).unwrap_err().reason, want, "{raw}");
        }
        let ok: Value = serde_json::from_str(r#"{"playabilityStatus":{"status":"OK"}}"#).unwrap();
        assert!(check_playability(&ok).is_ok());
    }

    #[test]
    fn builds_formats_with_merge_entries() {
        let resp = serde_json::json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {
                "title": "测试视频", "lengthSeconds": "212", "author": "频道名",
                "channelId": "UC123",
                "thumbnail": {"thumbnails": [
                    {"url": "https://i/small.jpg", "width": 120},
                    {"url": "https://i/big.jpg", "width": 1920}
                ]}
            },
            "streamingData": {
                "formats": [
                    {"url": "https://v/360.mp4", "height": 360, "width": 640,
                     "qualityLabel": "360p", "mimeType": "video/mp4", "contentLength": "1000"}
                ],
                "adaptiveFormats": [
                    {"url": "https://v/1080.mp4", "height": 1080, "qualityLabel": "1080p",
                     "mimeType": "video/mp4; codecs=\"av01\"", "contentLength": "8000"},
                    {"url": "https://a/low.m4a", "mimeType": "audio/mp4; codecs=\"mp4a\"",
                     "bitrate": 64000, "contentLength": "500"},
                    {"url": "https://a/high.m4a", "mimeType": "audio/mp4; codecs=\"mp4a\"",
                     "bitrate": 128000, "contentLength": "900"},
                    {"signatureCipher": "s=xx", "height": 2160, "mimeType": "video/mp4"}
                ]
            }
        });

        let info = build(&resp, "dQw4w9WgXcQ").unwrap();
        assert_eq!(
            info.video_url, "https://v/360.mp4",
            "默认给能直接播的合一档"
        );
        assert_eq!(info.title, "测试视频");
        assert_eq!(info.duration, 212.0);
        assert_eq!(info.cover_url, "https://i/big.jpg", "封面要挑最大的");

        let merged: Vec<_> = info.formats.iter().filter(|f| f.needs_merge()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].height, 1080);
        assert_eq!(merged[0].codec, "AV1");
        assert_eq!(
            merged[0].audio_url, "https://a/high.m4a",
            "音频挑码率最高的"
        );
        assert_eq!(merged[0].filesize, 8900, "体积是音视频之和");

        assert!(info.formats.iter().any(|f| f.label == "仅音频"));
        assert!(
            !info.formats.iter().any(|f| f.height == 2160),
            "带 signatureCipher 的 4K 档要跳过"
        );
    }

    #[test]
    fn missing_streams_is_empty_so_next_client_is_tried() {
        let resp = serde_json::json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"title": "x"},
            "streamingData": {"adaptiveFormats": [{"signatureCipher": "s=1"}]}
        });
        assert_eq!(
            build(&resp, "dQw4w9WgXcQ").unwrap_err().reason,
            Reason::Empty
        );
    }

    #[test]
    fn fallback_thumbnail_is_used() {
        let details = serde_json::json!({});
        assert_eq!(
            best_thumbnail(&details, "dQw4w9WgXcQ"),
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg"
        );
    }
}
