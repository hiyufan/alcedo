//! 哔哩哔哩。
//!
//! 比 Python 版多做一件事：直接要 DASH 流。原来那边只能拿到 `durl` 的 720p，
//! 想要 1080p 以上得另外起一个 yt-dlp 进程去列档位（慢、还多一个外部依赖）。
//! B 站的 `playurl` 接口带上 `fnval=4048` 就会返回分离的音视频轨，我们自己
//! 列出来交给上层合并，一次请求解决。

use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde_json::Value;

use crate::error::{Error, Reason, Result};
use crate::http::{ua, Http, Req};
use crate::model::{short_side, Author, Format, VideoInfo};
use crate::util;

const REFERER: &str = "https://www.bilibili.com/";

/// 设备指纹 cookie。机房 / 海外 IP 不带 buvid3 直接请求 API 会被 412。
///
/// 进程内缓存一份，免得每次解析都去领一次（多一个跨洋往返）。
static BUVID: RwLock<Option<(String, Instant)>> = RwLock::new(None);
const BUVID_TTL: Duration = Duration::from_secs(3600);

/// 解析一条哔哩哔哩分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let bvid = bvid_from_url(http, url).await?;
    parse_id(http, &bvid).await
}

/// 已知哔哩哔哩作品 ID 时直接解析。
pub async fn parse_id(http: &Http, bvid: &str) -> Result<VideoInfo> {
    let http = http.clone().with_fixed_ua(ua::DESKTOP_FIXED);
    let cookie = ensure_buvid(&http).await;

    // 第一步：作品信息（标题、封面、作者、分 P）
    let view = api_get(
        &http,
        &format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}"),
        cookie.as_deref(),
    )
    .await?;

    let code = util::i64_at(&view, &["code"]);
    if code != 0 {
        let msg = util::str_at(&view, &["message"]);
        return Err(match code {
            -404 | 62002 | 62004 => Error::deleted(msg),
            -403 => Error::restricted(msg),
            _ => Error::restricted(format!("B 站返回 code={code} {msg}")),
        });
    }

    let data = util::get(&view, &["data"]).ok_or_else(|| Error::parse("view 接口没有 data"))?;
    let cid = util::u64_at(data, &["pages", "0", "cid"]);
    if cid == 0 {
        return Err(Error::parse("view 接口没有返回 cid"));
    }

    // 第二步：播放地址。fnval=4048 要 DASH，fourk=1 放开 4K。
    let play = api_get(
        &http,
        &format!(
            "https://api.bilibili.com/x/player/playurl?bvid={bvid}&cid={cid}\
             &qn=127&fnval=4048&fnver=0&fourk=1&otype=json"
        ),
        cookie.as_deref(),
    )
    .await?;

    let play_code = util::i64_at(&play, &["code"]);
    if play_code != 0 {
        return Err(Error::restricted(format!(
            "B 站播放接口 code={play_code} {}",
            util::str_at(&play, &["message"])
        )));
    }
    let play_data = util::get(&play, &["data"]).ok_or_else(|| Error::parse("playurl 没有 data"))?;

    let mut info = VideoInfo {
        title: util::str_at(data, &["title"]),
        cover_url: util::str_at(data, &["pic"]),
        duration: util::num_at(data, &["duration"]),
        author: Author::new(
            util::id_at(data, &["owner", "mid"]),
            util::str_at(data, &["owner", "name"]),
            util::str_at(data, &["owner", "face"]),
        ),
        ..Default::default()
    };

    // 浏览器能直接播的那一档：durl 是音视频合一的 mp4/flv
    if let Some(durl) = util::arr_at(play_data, &["durl"]).first() {
        info.video_url = util::str_at(durl, &["url"]);
    }
    info.formats = dash_formats(play_data);

    if info.video_url.is_empty() {
        // DASH 的每一档都是分离轨，浏览器点开是播不了的，必须另外要一条合一流。
        //
        // 这一步不能因为"已经有 formats 了"就跳过：没有它前端就没有可直接播放
        // 的地址。顺带一提，html5 平台这条路不登录也能给到 720p，比 DASH 那边
        // 未登录只给 360/480 更高——所以它同时也是清晰度的补充。
        match legacy_durl(&http, bvid, cid, cookie.as_deref()).await {
            Ok(u) if !u.is_empty() => info.video_url = u,
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "B 站合一流拿不到，只剩 DASH 档位"),
        }
    }

    if info.video_url.is_empty() && info.formats.is_empty() {
        return Err(Error::bare(Reason::Empty));
    }

    // 直链必须带 Referer，否则 CDN 一律 403
    info.set_header("Referer", REFERER);
    info.set_header("User-Agent", ua::DESKTOP_FIXED);
    Ok(info)
}

/// 从各种 B 站地址里拿到 BV 号。
async fn bvid_from_url(http: &Http, url: &str) -> Result<String> {
    let host = util::host_of(url).unwrap_or_default();

    // 短链：b23.tv / bili2233.cn
    if host.ends_with("b23.tv") || host.ends_with("bili2233.cn") {
        let location = http.resolve_redirect(url).await?;
        return Box::pin(bvid_from_url(http, &location)).await;
    }

    let parsed = url::Url::parse(url)?;
    if let Some(seg) = parsed
        .path_segments()
        .and_then(|mut s| s.find(|p| p.starts_with("BV") || p.starts_with("bv")))
    {
        return Ok(seg.to_owned());
    }
    // /video/av170001 这种老地址
    if let Some(av) = parsed.path_segments().and_then(|mut s| {
        s.find(|p| p.starts_with("av") && p[2..].bytes().all(|b| b.is_ascii_digit()))
    }) {
        return Ok(av.to_owned());
    }
    Err(Error::unsupported("不是有效的 B 站视频链接（没有 BV 号）"))
}

/// 领一份 buvid cookie。拿不到就裸请求试试，别因为指纹接口抽风就整条路断掉。
async fn ensure_buvid(http: &Http) -> Option<String> {
    if let Some(c) = http.config().bilibili_cookie.clone() {
        return Some(c);
    }
    if let Some((c, at)) = BUVID.read().as_ref() {
        if at.elapsed() < BUVID_TTL {
            return Some(c.clone());
        }
    }

    let resp = http
        .send(
            Req::get("https://api.bilibili.com/x/frontend/finger/spi")
                .referer(REFERER)
                .header("User-Agent", ua::DESKTOP_FIXED),
        )
        .await
        .ok()?;
    let json = resp.json().ok()?;
    let b3 = util::str_at(&json, &["data", "b_3"]);
    if b3.is_empty() {
        return None;
    }
    let b4 = util::str_at(&json, &["data", "b_4"]);
    let cookie = format!("buvid3={b3}; buvid4={b4}");
    *BUVID.write() = Some((cookie.clone(), Instant::now()));
    Some(cookie)
}

/// 发一个带完整伪装头的 API 请求；撞上 412 就换一份 buvid 重来一次。
async fn api_get(http: &Http, url: &str, cookie: Option<&str>) -> Result<Value> {
    for attempt in 0..2 {
        let mut req = Req::get(url)
            .referer(REFERER)
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*");
        if let Some(c) = cookie {
            req = req.cookie_header(c);
        }
        let resp = http.send(req).await?;

        if resp.status.as_u16() == 412 {
            if attempt == 0 && http.config().bilibili_cookie.is_none() {
                // 风控：手上这份 buvid 被标记了，丢掉重领
                *BUVID.write() = None;
                let _ = ensure_buvid(http).await;
                continue;
            }
            return Err(Error::blocked(
                "B 站拒绝了服务器所在网络的访问 (412)，海外服务器请配置 ALCEDO_PROXY_CN 或 ALCEDO_BILI_COOKIE",
            ));
        }
        resp.error_for_status()?;
        return resp.json();
    }
    Err(Error::blocked("B 站连续返回 412"))
}

/// 老接口的合一流，作为 DASH 不可用时的退路。
async fn legacy_durl(http: &Http, bvid: &str, cid: u64, cookie: Option<&str>) -> Result<String> {
    let json = api_get(
        http,
        &format!(
            "https://api.bilibili.com/x/player/playurl?bvid={bvid}&cid={cid}\
             &qn=80&fnval=0&fnver=0&otype=json&platform=html5"
        ),
        cookie,
    )
    .await?;
    Ok(util::str_at(&json, &["data", "durl", "0", "url"]))
}

/// 把 DASH 的音视频轨配成一档档清晰度。
///
/// 音频固定挑码率最高的那条：它通常只有几 MB，没必要为了省那点体积让用户听个糊的。
fn dash_formats(play_data: &Value) -> Vec<Format> {
    let audios = util::arr_at(play_data, &["dash", "audio"]);
    let best_audio = audios
        .iter()
        .max_by_key(|a| util::u64_at(a, &["bandwidth"]))
        .map(|a| util::str_at(a, &["base_url"]))
        .unwrap_or_default();

    let mut out = Vec::new();
    for v in util::arr_at(play_data, &["dash", "video"]) {
        let video_url = util::first_str(v, &[&["base_url"], &["baseUrl"]]);
        if video_url.is_empty() {
            continue;
        }
        let height = util::u32_at(v, &["height"]);
        let width = util::u32_at(v, &["width"]);
        let short = short_side(width, height);
        let codec = match util::i64_at(v, &["codecid"]) {
            12 => "H.265",
            13 => "AV1",
            _ => "",
        };

        out.push(Format {
            label: quality_label(util::i64_at(v, &["id"]), short, codec),
            url: String::new(), // 音视频分离，必须合并
            ext: "mp4".into(),
            height: short,
            // bandwidth 是 bit/s，乘时长才是体积；这里拿不到时长就留 0，
            // 宁可不显示也别显示一个错的数
            filesize: 0,
            codec: codec.to_owned(),
            video_url,
            audio_url: best_audio.clone(),
        });
    }
    out
}

/// B 站的清晰度 id → 展示名。
fn quality_label(id: i64, short: u32, codec: &str) -> String {
    let base = match id {
        127 => "8K".to_string(),
        126 => "杜比视界".to_string(),
        125 => "HDR".to_string(),
        120 => "4K".to_string(),
        116 => "1080p60".to_string(),
        112 => "1080p+".to_string(),
        80 => "1080p".to_string(),
        74 => "720p60".to_string(),
        64 => "720p".to_string(),
        32 => "480p".to_string(),
        16 => "360p".to_string(),
        _ if short > 0 => format!("{short}p"),
        _ => "未知".to_string(),
    };
    if codec.is_empty() {
        base
    } else {
        format!("{base} {codec}")
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quality_labels() {
        assert_eq!(quality_label(120, 2160, ""), "4K");
        assert_eq!(quality_label(80, 1080, "AV1"), "1080p AV1");
        assert_eq!(quality_label(999, 540, ""), "540p");
    }

    #[test]
    fn dash_pairs_video_with_best_audio() {
        let data = json!({
            "dash": {
                "audio": [
                    {"base_url": "https://a/low.m4s", "bandwidth": 64000},
                    {"base_url": "https://a/high.m4s", "bandwidth": 320000}
                ],
                "video": [
                    {"base_url": "https://v/1080.m4s", "id": 80, "width": 1920, "height": 1080, "codecid": 7},
                    {"base_url": "https://v/1080av1.m4s", "id": 80, "width": 1920, "height": 1080, "codecid": 13}
                ]
            }
        });
        let fmts = dash_formats(&data);
        assert_eq!(fmts.len(), 2);
        for f in &fmts {
            assert!(f.needs_merge(), "DASH 档位必须标成需要合并");
            assert_eq!(f.audio_url, "https://a/high.m4s", "音频要挑码率最高的");
            assert_eq!(f.height, 1080);
        }
        assert_eq!(fmts[1].codec, "AV1");
    }

    #[test]
    fn dash_skips_entries_without_url() {
        let data = json!({"dash": {"audio": [], "video": [{"id": 80, "height": 1080}]}});
        assert!(dash_formats(&data).is_empty());
    }

    #[tokio::test]
    async fn bvid_extracted_from_page_urls() {
        let http = Http::new(
            std::sync::Arc::new(crate::Config::default()),
            Some(crate::Source::BiliBili),
        )
        .unwrap();
        assert_eq!(
            bvid_from_url(&http, "https://www.bilibili.com/video/BV1xx411c7mD/?spm=1")
                .await
                .unwrap(),
            "BV1xx411c7mD"
        );
        assert_eq!(
            bvid_from_url(&http, "https://m.bilibili.com/video/av170001")
                .await
                .unwrap(),
            "av170001"
        );
        assert!(bvid_from_url(&http, "https://www.bilibili.com/read/cv123")
            .await
            .is_err());
    }
}
