//! 哔哩哔哩。
//!
//! 直接要 DASH 流：`playurl` 带上 `fnval=4048` 就会返回分离的音视频轨，
//! 自己列出档位交给上层合并，一次请求就能拿到 1080p 以上。只取 `durl` 的话
//! 封顶 720p，想要更高就得另起外部进程，既慢又多一层依赖。

use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Reason, Result};
use crate::http::identity::IdentitySlot;
use crate::http::{ua, Http, Req};
use crate::model::{short_side, Author, Format, VideoInfo};
use crate::util;

const REFERER: &str = "https://www.bilibili.com/";

/// 设备指纹 cookie。机房 / 海外 IP 不带 buvid3 直接请求 API 会被 412。
///
/// 进程内缓存一份，免得每次解析都去领一次（多一个跨洋往返）。
static BUVID: IdentitySlot = IdentitySlot::new(Duration::from_secs(3600));

/// 解析一条哔哩哔哩分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let bvid = bvid_from_url(http, url).await?;
    parse_id(http, &bvid).await
}

/// 已知哔哩哔哩作品 ID 时直接解析。
pub async fn parse_id(http: &Http, bvid: &str) -> Result<VideoInfo> {
    let http = http.clone().with_fixed_ua(ua::DESKTOP_FIXED);
    let cookie = ensure_buvid(&http).await;
    let cookie = cookie.as_deref();

    // 播放地址只依赖 cid，作品信息（标题、封面、作者）只用来填展示字段，两条线
    // 互不等待。cid 从 pagelist 拿：它只有两百来字节，比 view 快 ~27ms，这样
    // 关键路径是 pagelist → playurl，view 在旁边并行跑完。
    let view_url = format!(
        "https://api.bilibili.com/x/web-interface/view?{}",
        id_param(bvid, "aid")
    );
    let (view, early) = tokio::join!(api_get(&http, &view_url, cookie), async {
        let cid = page_cid(&http, bvid, cookie).await?;
        Some(play_urls(&http, bvid, cid, cookie).await)
    });
    let view = view?;

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
    let (play, legacy) = match early {
        Some(urls) => urls,
        // pagelist 没拿到（老 av 号之类），退回用 view 里的 cid 再要一次
        None => {
            let cid = util::u64_at(data, &["pages", "0", "cid"]);
            if cid == 0 {
                return Err(Error::parse("view 接口没有返回 cid"));
            }
            play_urls(&http, bvid, cid, cookie).await
        }
    };
    let play = play?;

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
        match legacy {
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
pub(crate) async fn ensure_buvid(http: &Http) -> Option<String> {
    if let Some(c) = http.config().bilibili_cookie.clone() {
        return Some(c);
    }
    BUVID
        .get_or_fetch(http, |http| async move { fetch_buvid(&http).await })
        .await
}

async fn fetch_buvid(http: &Http) -> Option<String> {
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
    Some(format!("buvid3={b3}; buvid4={b4}"))
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
                BUVID.forget();
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

/// 接口里的作品参数。BV 号走 `bvid=`；老的 av 号要换成纯数字——
/// 把 `av170001` 原样塞给 bvid，B 站只会回一个 -400。
///
/// av 号的参数名各接口不统一：view / pagelist 认 `aid`，playurl 只认 `avid`。
fn id_param(id: &str, aid_key: &str) -> String {
    match id.strip_prefix("av").or_else(|| id.strip_prefix("AV")) {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{aid_key}={n}")
        }
        _ => format!("bvid={id}"),
    }
}

/// 第一 P 的 cid。拿不到返回 `None`，由调用方退回 view 里的那份。
async fn page_cid(http: &Http, bvid: &str, cookie: Option<&str>) -> Option<u64> {
    let url = format!(
        "https://api.bilibili.com/x/player/pagelist?{}",
        id_param(bvid, "aid")
    );
    let json = api_get(http, &url, cookie).await.ok()?;
    Some(util::u64_at(&json, &["data", "0", "cid"])).filter(|&c| c != 0)
}

/// 两份播放地址：DASH 分轨（fnval=4048 要 DASH，fourk=1 放开 4K）和合一流。
///
/// 走 `x/player/wbi/playurl` 而不是老的 `x/player/playurl`：老接口风控严得多，
/// 同一出口连续解析几百次后稳定 412，同一时刻新接口照常返回、内容一致。
/// 新接口目前不校验 WBI 签名（w_rid / wts），哪天开始校验再补。
///
/// 两者只依赖 cid，互不等待。DASH 实际上从不带 durl，合一流几乎每次都要补，
/// 串行就是白等一个往返（实测 ~80ms），所以一起发。
async fn play_urls(
    http: &Http,
    bvid: &str,
    cid: u64,
    cookie: Option<&str>,
) -> (Result<Value>, Result<String>) {
    let mut dash_url = format!(
        "https://api.bilibili.com/x/player/wbi/playurl?{}&cid={cid}\
         &qn=127&fnval=4048&fnver=0&fourk=1&otype=json",
        id_param(bvid, "avid")
    );
    if http.config().bilibili_cookie.is_none() {
        dash_url.push_str(&guest_query());
    }
    tokio::join!(
        api_get(http, &dash_url, cookie),
        legacy_durl(http, bvid, cid, cookie),
    )
}

/// 游客也要到 720p / 1080p 的 DASH 档位，返回以 `&` 开头的查询串。
///
/// `try_look=1` 是 B 站给游客"试看高清"的开关，但只有同时带上浏览器指纹参数
/// （`dm_*`）才生效，缺一个就只给 360 / 480。实测带上之后 1080p 照给，WBI
/// 签名反而不是必须的。做法照 yt-dlp（来源是 B 站自己的
/// bili-user-fingerprint.js）：这些值本来就是前端随机造的，鼠标轨迹那两项留空也能过。
fn guest_query() -> String {
    use base64::Engine;
    use rand::Rng;

    // 对应前端 / yt-dlp 里的 string.printable（不含换行这类控制字符也不影响）
    const PRINTABLE: &[u8] =
        b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!#$%&()*+,-./:;<=>?@[]^_{|}~ ";

    let mut rng = rand::rng();
    let mut noise = |lo: usize, hi: usize| {
        let len = rng.random_range(lo..=hi);
        let raw: Vec<u8> = (0..len)
            .map(|_| PRINTABLE[rng.random_range(0..PRINTABLE.len())])
            .collect();
        let mut b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        // 前端的实现会砍掉最后两个字符，照做
        b64.truncate(b64.len().saturating_sub(2));
        b64
    };
    let img_str = noise(16, 64);
    let cover_str = noise(32, 128);

    let (r, o, top) = (
        rng.random_range(0..114_i64),
        rng.random_range(0..514_i64),
        rng.random_range(0..=100_i64),
    );
    // 屏幕 1920x1080 时的 wh / of 编码；serde_json 输出本来就是紧凑的，B 站要求不能有空格
    let inter = serde_json::json!({
        "ds": [],
        "wh": [2 * 1920 + 2 * 1080 + 3 * r, 4 * 1920 - 1080 + r, r],
        "of": [3 * top + o, 4 * top + 2 * o, o],
    })
    .to_string();

    let enc = |v: &str| {
        percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    format!(
        "&try_look=1&dm_img_list=%5B%5D&dm_img_str={}&dm_cover_img_str={}&dm_img_inter={}",
        enc(&img_str),
        enc(&cover_str),
        enc(&inter)
    )
}

/// 老接口的合一流，作为 DASH 不可用时的退路。
async fn legacy_durl(http: &Http, bvid: &str, cid: u64, cookie: Option<&str>) -> Result<String> {
    let json = api_get(
        http,
        &format!(
            "https://api.bilibili.com/x/player/wbi/playurl?{}&cid={cid}\
             &qn=80&fnval=0&fnver=0&otype=json&platform=html5",
            id_param(bvid, "avid")
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
    fn av_ids_use_the_aid_parameter() {
        assert_eq!(id_param("BV1GJ411x7h7", "aid"), "bvid=BV1GJ411x7h7");
        assert_eq!(id_param("av170001", "aid"), "aid=170001");
        assert_eq!(id_param("AV2", "avid"), "avid=2");
        // 不是纯数字的别误判
        assert_eq!(id_param("avatar", "aid"), "bvid=avatar");
    }

    #[test]
    fn guest_query_shape() {
        let q = guest_query();
        assert!(q.starts_with("&try_look=1&"), "{q}");
        let inter = q.split("dm_img_inter=").nth(1).unwrap();
        let inter = percent_encoding::percent_decode_str(inter)
            .decode_utf8()
            .unwrap();
        assert!(!inter.contains(' '), "B 站要紧凑 JSON: {inter}");
        let v: Value = serde_json::from_str(&inter).unwrap();
        assert_eq!(v["wh"].as_array().unwrap().len(), 3);
        assert_eq!(v["of"].as_array().unwrap().len(), 3);
    }

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
