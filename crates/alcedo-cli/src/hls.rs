//! HLS 下载：解析主清单 / 分片清单，并发拉分片按顺序落盘，音视频分开的用 ffmpeg 合并。
//!
//! 两个用处：
//! - 有的结果默认地址就是 HLS 清单（YouTube 的 visionOS 客户端不给音视频合一的直链），
//!   不认 HLS 的话"下载默认档"存下来的是一个几十 KB 的 `.m3u8` 文本
//! - 分轨直链被拒（403 之类）时退到同清晰度的 HLS。求原图 2026-10-21 实测：同一出口下
//!   TV 客户端的分轨读 1 KB 就 403，HLS 分片照样能下
//!
//! 只做点播（VOD）、不加密、整段分片的清单；直播、加密、按字节区间切的分片直接说不支持。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use reqwest::header::HeaderMap;

use crate::media;

/// 同时拉几个分片。YouTube 的分片 0.5~2 MB，8 路足够把带宽吃满，内存里最多压 8 片。
const CONCURRENCY: usize = 8;

/// 默认档不指定清晰度时最高拿到多少。再往上 YouTube 的 HLS 只有 VP9，
/// 不少播放器放不了，和分轨档位"1080p 以内给 H.264"一致。
pub(crate) const DEFAULT_MAX_HEIGHT: u32 = 1080;

/// 看地址像不像 HLS 清单。只看路径，不发请求。
pub(crate) fn looks_like_hls(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        let path = u.path().to_ascii_lowercase();
        Path::new(&path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("m3u8"))
            || path.contains("/hls_playlist/")
            || path.contains("/hls_variant/")
    })
}

/// 主清单里的一路画面。
#[derive(Debug, Clone, PartialEq)]
struct Variant {
    url: String,
    height: u32,
    bandwidth: u64,
    codecs: String,
    /// `AUDIO` 组名：音频在单独的清单里
    audio: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Master {
    variants: Vec<Variant>,
    /// 音频组名 → 清单地址
    audio: HashMap<String, String>,
}

#[derive(Debug, PartialEq)]
struct MediaPlaylist {
    /// fMP4 的初始化段（`EXT-X-MAP`）
    init: Option<String>,
    segments: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Playlist {
    Master(Master),
    Media(MediaPlaylist),
}

fn parse(text: &str, base: &url::Url) -> Result<Playlist, String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("不是 HLS 清单".into());
    }
    let join = |u: &str| {
        base.join(u.trim())
            .map(String::from)
            .map_err(|e| format!("清单里的地址无效: {e}"))
    };
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(text, &join)
    } else {
        parse_media(text, &join)
    }
}

fn parse_master(
    text: &str,
    join: &impl Fn(&str) -> Result<String, String>,
) -> Result<Playlist, String> {
    let mut variants = Vec::new();
    let mut audio: HashMap<String, (bool, String)> = HashMap::new();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(rest);
            let (Some("AUDIO"), Some(group), Some(uri)) = (
                a.get("TYPE").map(String::as_str),
                a.get("GROUP-ID"),
                a.get("URI"),
            ) else {
                continue;
            };
            // 同组多条时用 DEFAULT=YES 的那条
            let default = a.get("DEFAULT").is_some_and(|v| v == "YES");
            if audio.get(group).is_none_or(|(d, _)| default && !d) {
                audio.insert(group.clone(), (default, join(uri)?));
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(rest);
            let Some(uri) = lines.find(|l| !l.starts_with('#')) else {
                break;
            };
            let height = a
                .get("RESOLUTION")
                .and_then(|r| r.split_once('x'))
                .and_then(|(w, h)| {
                    Some(alcedo::model::short_side(w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or(0);
            variants.push(Variant {
                url: join(uri)?,
                height,
                bandwidth: a.get("BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0),
                codecs: a.get("CODECS").cloned().unwrap_or_default(),
                audio: a.get("AUDIO").cloned(),
            });
        }
    }
    if variants.is_empty() {
        return Err("主清单里没有画面".into());
    }
    Ok(Playlist::Master(Master {
        variants,
        audio: audio.into_iter().map(|(k, (_, v))| (k, v)).collect(),
    }))
}

fn parse_media(
    text: &str,
    join: &impl Fn(&str) -> Result<String, String>,
) -> Result<Playlist, String> {
    let mut init = None;
    let mut segments = Vec::new();
    let mut vod = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            if attrs(rest).get("METHOD").is_some_and(|m| m != "NONE") {
                return Err("分片是加密的，暂不支持".into());
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if attrs(rest).contains_key("BYTERANGE") {
                return Err("按字节区间切的分片暂不支持".into());
            }
            init = attrs(rest).get("URI").map(|u| join(u)).transpose()?;
        } else if line.starts_with("#EXT-X-BYTERANGE") {
            return Err("按字节区间切的分片暂不支持".into());
        } else if line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" {
            vod = true;
        } else if !line.starts_with('#') {
            segments.push(join(line)?);
        }
    }
    if !vod {
        return Err("这是直播流，没有结尾，没法整段下载".into());
    }
    if segments.is_empty() {
        return Err("清单里没有分片".into());
    }
    Ok(Playlist::Media(MediaPlaylist { init, segments }))
}

/// `KEY=VALUE,KEY="带, 逗号"` → map。引号里的逗号不算分隔。
fn attrs(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.split_once('"') {
                Some((v, n)) => (v, n),
                None => (quoted, ""),
            }
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        out.insert(key.trim().to_owned(), value.to_owned());
        rest = next.trim_start_matches(',').trim_start();
    }
    out
}

/// 挑一路画面：`max_height` 以内最高清的；同一清晰度优先 H.264（播放器兼容性最好），
/// 再挑码率高的。全都超过上限就拿最低的那路。
fn choose(variants: &[Variant], max_height: u32) -> Option<&Variant> {
    let fits = |v: &&Variant| v.height <= max_height;
    let avc = |v: &&Variant| v.codecs.starts_with("avc1");
    variants
        .iter()
        .filter(fits)
        .max_by_key(|v| (v.height, avc(v), v.bandwidth))
        .or_else(|| variants.iter().min_by_key(|v| (v.height, v.bandwidth)))
}

/// 下载一条 HLS，返回最终文件路径。`max_height` 只对主清单起作用。
pub(crate) async fn download(
    url: &str,
    headers: &HeaderMap,
    dir: &Path,
    stem: &str,
    max_height: u32,
) -> Result<PathBuf, String> {
    let playlist = fetch_playlist(url, headers).await?;
    let tmp = |kind: &str| dir.join(format!(".alcedo-{}-{kind}.part", std::process::id()));
    let (video_tmp, audio_tmp) = (tmp("video"), tmp("audio"));
    let result = async {
        match playlist {
            Playlist::Media(m) => {
                fetch_segments(&m, headers, &video_tmp, "视频").await?;
                finish(&video_tmp, None, dir, stem).await
            }
            Playlist::Master(master) => {
                let v = choose(&master.variants, max_height).ok_or("主清单里没有画面")?;
                println!("HLS：{}p（{}）", v.height, v.codecs);
                let audio_url = v.audio.as_ref().and_then(|g| master.audio.get(g));
                // 有单独音轨就要合并，先确认 ffmpeg 在，别下完才发现
                if audio_url.is_some() && !media::has_ffmpeg().await {
                    return Err(
                        "这条 HLS 的声音和画面是分开的，合并需要 ffmpeg，但 PATH 里没找到".into(),
                    );
                }
                let Playlist::Media(video) = fetch_playlist(&v.url, headers).await? else {
                    return Err("画面清单又套了一层主清单".into());
                };
                fetch_segments(&video, headers, &video_tmp, "视频轨").await?;
                let audio = if let Some(a) = audio_url {
                    let Playlist::Media(audio) = fetch_playlist(a, headers).await? else {
                        return Err("音频清单又套了一层主清单".into());
                    };
                    fetch_segments(&audio, headers, &audio_tmp, "音频轨").await?;
                    Some(audio_tmp.as_path())
                } else {
                    None
                };
                finish(&video_tmp, audio, dir, stem).await
            }
        }
    }
    .await;
    // 不管成败，临时文件都清掉
    let _ = tokio::fs::remove_file(&video_tmp).await;
    let _ = tokio::fs::remove_file(&audio_tmp).await;
    result
}

async fn fetch_playlist(url: &str, headers: &HeaderMap) -> Result<Playlist, String> {
    let resp = media::send_following(media::client_for(url), url, headers).await?;
    if !resp.status().is_success() {
        return Err(format!("清单返回 HTTP {}", resp.status().as_u16()));
    }
    // 跳转之后的地址才是相对路径的基准
    let base = resp.url().clone();
    let text = resp.text().await.map_err(|e| format!("读清单失败: {e}"))?;
    parse(&text, &base)
}

/// 并发拉分片、按清单顺序写进 `path`。
#[allow(clippy::cast_precision_loss)]
async fn fetch_segments(
    m: &MediaPlaylist,
    headers: &HeaderMap,
    path: &Path,
    what: &str,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("写文件 {} 失败: {e}", path.display()))?;
    let urls: Vec<&String> = m.init.iter().chain(&m.segments).collect();
    let total = urls.len();
    let mut parts = futures_util::stream::iter(urls)
        .map(|u| fetch_segment(u, headers))
        .buffered(CONCURRENCY);
    let (mut done, mut bytes, mut shown) = (0_usize, 0_u64, 0_usize);
    while let Some(part) = parts.next().await {
        let part = part.map_err(|e| format!("{what}第 {} 段: {e}", done + 1))?;
        file.write_all(&part)
            .await
            .map_err(|e| format!("写文件 {} 失败: {e}", path.display()))?;
        done += 1;
        bytes += part.len() as u64;
        let tenth = done * 10 / total;
        if tenth > shown {
            shown = tenth;
            eprint!("\r{what} {:>3}%", tenth * 10);
        }
    }
    file.flush().await.map_err(|e| format!("写文件失败: {e}"))?;
    eprintln!("\r{what} {total} 段，{:.1} MB", bytes as f64 / 1e6);
    Ok(())
}

async fn fetch_segment(url: &str, headers: &HeaderMap) -> Result<bytes::Bytes, String> {
    let once = || async {
        let resp = media::send_following(media::client_for(url), url, headers).await?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        resp.bytes().await.map_err(|e| e.to_string())
    };
    // 几百段里偶尔一段连不上，重试一次
    match once().await {
        Ok(b) => Ok(b),
        Err(_) => once().await,
    }
}

/// 拼好的分片 → 成品 mp4。有 ffmpeg 就封装成 mp4（有单独音轨就一起合进去）；
/// 没有 ffmpeg 又不用合并时，把分片原样留下（TS 也能播）。
async fn finish(
    video: &Path,
    audio: Option<&Path>,
    dir: &Path,
    stem: &str,
) -> Result<PathBuf, String> {
    let name = media::safe_filename(stem, "media");
    if audio.is_none() && !media::has_ffmpeg().await {
        let out = crate::download::unique_path(dir, &name, "ts");
        tokio::fs::rename(video, &out)
            .await
            .map_err(|e| format!("保存 {} 失败: {e}", out.display()))?;
        println!("没有 ffmpeg，分片原样保存 → {}", out.display());
        return Ok(out);
    }
    let out = crate::download::unique_path(dir, &name, "mp4");
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video);
    if let Some(a) = audio {
        cmd.arg("-i")
            .arg(a)
            .args(["-map", "0:v:0", "-map", "1:a:0"]);
    }
    let status = cmd
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&out)
        .status()
        .await
        .map_err(|e| format!("起 ffmpeg 失败: {e}"))?;
    if !status.success() {
        let _ = tokio::fs::remove_file(&out).await;
        return Err(format!("ffmpeg 封装失败（退出码 {:?}）", status.code()));
    }
    println!("完成 → {}", out.display());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> url::Url {
        url::Url::parse(
            "https://manifest.googlevideo.com/api/manifest/hls_variant/id/x/file/index.m3u8",
        )
        .unwrap()
    }

    /// YouTube 真实主清单的骨架（地址换成了相对 / 假的）
    const MASTER: &str = r#"#EXTM3U
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-MEDIA:URI="audio/233.m3u8",TYPE=AUDIO,GROUP-ID="233",NAME="Default",DEFAULT=YES,AUTOSELECT=YES
#EXT-X-MEDIA:URI="audio/234.m3u8",TYPE=AUDIO,GROUP-ID="234",NAME="Default",DEFAULT=YES,AUTOSELECT=YES
#EXT-X-MEDIA:URI="subs/en.m3u8",TYPE=SUBTITLES,GROUP-ID="vtt",LANGUAGE="en",NAME="English"
#EXT-X-STREAM-INF:BANDWIDTH=300694,CODECS="avc1.4D4015,mp4a.40.5",RESOLUTION=426x240,AUDIO="233",SUBTITLES="vtt"
v/229.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1248435,CODECS="avc1.4D401F,mp4a.40.2",RESOLUTION=1280x720,AUDIO="234",SUBTITLES="vtt"
v/232.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1100000,CODECS="vp09.00.31.08,mp4a.40.2",RESOLUTION=1280x720,AUDIO="234"
v/609.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4688074,CODECS="avc1.640028,mp4a.40.2",RESOLUTION=1920x1080,AUDIO="234"
v/270.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=9000000,CODECS="vp09.00.50.08,mp4a.40.2",RESOLUTION=3840x2160,AUDIO="234"
v/625.m3u8
"#;

    #[test]
    fn master_playlist_lists_variants_and_audio_groups() {
        let Playlist::Master(m) = parse(MASTER, &base()).unwrap() else {
            panic!("应当是主清单");
        };
        assert_eq!(m.variants.len(), 5);
        let v = &m.variants[1];
        assert_eq!(
            (v.height, v.bandwidth, v.codecs.as_str(), v.audio.as_deref()),
            (720, 1_248_435, "avc1.4D401F,mp4a.40.2", Some("234")),
            "引号里的逗号不能把属性切断"
        );
        assert_eq!(
            v.url,
            "https://manifest.googlevideo.com/api/manifest/hls_variant/id/x/file/v/232.m3u8"
        );
        assert_eq!(m.audio.len(), 2, "字幕组不算音频");
        assert!(m.audio["234"].ends_with("/audio/234.m3u8"));
    }

    #[test]
    fn choose_prefers_h264_within_the_height_limit() {
        let Playlist::Master(m) = parse(MASTER, &base()).unwrap() else {
            panic!();
        };
        let pick = |h| {
            choose(&m.variants, h)
                .unwrap()
                .url
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(pick(1080), "270.m3u8");
        assert_eq!(
            pick(720),
            "232.m3u8",
            "同为 720p 选 H.264，不选码率更高的 VP9"
        );
        assert_eq!(pick(2160), "625.m3u8", "1080p 以上只有 VP9 时照给");
        assert_eq!(pick(100), "229.m3u8", "全都超了拿最低的");
    }

    #[test]
    fn media_playlist_segments_and_init() {
        let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:5.2,\nseg/1.ts\n#EXTINF:5.1,\nhttps://cdn.example/seg/2.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(text, &base()).unwrap() else {
            panic!("应当是分片清单");
        };
        assert!(m.init.unwrap().ends_with("/file/init.mp4"));
        assert_eq!(m.segments.len(), 2);
        assert!(m.segments[0].ends_with("/file/seg/1.ts"));
        assert_eq!(m.segments[1], "https://cdn.example/seg/2.ts");
    }

    #[test]
    fn unsupported_playlists_say_why() {
        let live = "#EXTM3U\n#EXTINF:4,\na.ts\n";
        assert!(parse(live, &base()).unwrap_err().contains("直播"));
        let aes =
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(parse(aes, &base()).unwrap_err().contains("加密"));
        let plain_key = "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(parse(plain_key, &base()).is_ok(), "METHOD=NONE 就是不加密");
        let ranged = "#EXTM3U\n#EXTINF:4,\n#EXT-X-BYTERANGE:100@0\na.ts\n#EXT-X-ENDLIST\n";
        assert!(parse(ranged, &base()).unwrap_err().contains("字节区间"));
        assert!(parse("<html>", &base()).is_err());
    }

    #[test]
    fn hls_urls_are_recognized_by_path() {
        assert!(looks_like_hls(
            "https://manifest.googlevideo.com/api/manifest/hls_variant/expire/1/file/index.m3u8"
        ));
        assert!(looks_like_hls(
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/id/x/itag/232"
        ));
        assert!(looks_like_hls("https://cdn.example/live/x.M3U8?token=1"));
        assert!(!looks_like_hls("https://cdn.example/a.mp4?m3u8=1"));
        assert!(!looks_like_hls("not a url"));
    }
}
