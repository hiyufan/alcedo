//! `alcedo download` —— 把解析结果真正落到磁盘。
//!
//! 解析只给地址，这个子命令补上"拿到手"的最后一步：带对 Referer 流式下载、
//! `--format` 选档、DASH 音视频分离的档位用 ffmpeg 合并、图集连带实况图
//! 成对落盘（`01.jpg` + `01.mov`，导入相册才会动）、背景音乐单独取。
//! 文件名按标题清洗，与 Python 侧 `safe_filename` 行为对齐。

use std::path::{Path, PathBuf};

use alcedo::{Format, Source, VideoInfo};

use crate::media;

pub(crate) struct Options {
    input: String,
    dir: String,
    format: Option<String>,
    best: bool,
    music: bool,
    list: bool,
}

pub(crate) fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        input: String::new(),
        dir: ".".to_owned(),
        format: None,
        best: false,
        music: false,
        list: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => o.dir = it.next().ok_or("--dir 后面要跟目录")?.clone(),
            "--format" => {
                o.format = Some(
                    it.next()
                        .ok_or("--format 后面要跟档位名（用 --list 看）")?
                        .clone(),
                )
            }
            "--best" => o.best = true,
            "--music" => o.music = true,
            "--list" => o.list = true,
            other => {
                if other.starts_with("--") {
                    return Err(format!("不认识的参数: {other}"));
                }
                // 分享文案带空格会被 shell 切开，拼回去
                if !o.input.is_empty() {
                    o.input.push(' ');
                }
                o.input.push_str(other);
            }
        }
    }
    if o.input.is_empty() {
        return Err("要下载什么？给一条分享链接".into());
    }
    if o.format.is_some() && o.best {
        return Err("--format 和 --best 二选一".into());
    }
    Ok(o)
}

pub(crate) async fn run(opts: Options) -> Result<(), String> {
    let client = alcedo::Client::new().map_err(|e| format!("初始化失败: {e}"))?;
    let info = client.parse(&opts.input).await.map_err(|e| e.to_string())?;
    let src = info.source.map_or("?", Source::display_name);
    let title = if info.title.is_empty() {
        "(无标题)"
    } else {
        info.title.as_str()
    };
    println!("{src} · {title}");

    if opts.list {
        print_formats(&info);
        return Ok(());
    }
    std::fs::create_dir_all(&opts.dir).map_err(|e| format!("建目录 {} 失败: {e}", opts.dir))?;

    if opts.music {
        return download_music(&info, &opts.dir).await;
    }
    if info.is_gallery() {
        return download_gallery(&info, &opts.dir).await;
    }
    download_video(&info, &opts).await
}

fn print_formats(info: &VideoInfo) {
    if !info.video_url.is_empty() {
        println!("默认档: 可直接下载（不带 --format / --best 时就下它）");
    }
    if info.formats.is_empty() {
        println!("没有列出更多清晰度档位");
        return;
    }
    println!("清晰度档位:");
    for f in &info.formats {
        // 只是显示成 MB，精度损失无所谓
        #[allow(clippy::cast_precision_loss)]
        let size = if f.filesize > 0 {
            format!("  {:.1} MB", f.filesize as f64 / 1e6)
        } else {
            String::new()
        };
        let merge = if f.needs_merge() {
            "  [需 ffmpeg 合并]"
        } else {
            ""
        };
        println!("  - {}{size}{merge}", f.label);
    }
}

/// 一个下载目标：直链 + 落盘用的词干（扩展名等响应头到了再定）。
enum Pick {
    Direct {
        url: String,
        label: String,
    },
    Merge {
        video: String,
        audio: String,
        label: String,
    },
}

fn to_pick(f: &Format) -> Pick {
    if f.needs_merge() {
        Pick::Merge {
            video: f.video_url.clone(),
            audio: f.audio_url.clone(),
            label: f.label.clone(),
        }
    } else {
        Pick::Direct {
            url: f.url.clone(),
            label: f.label.clone(),
        }
    }
}

fn pick(info: &VideoInfo, opts: &Options) -> Result<Pick, String> {
    if let Some(label) = &opts.format {
        let f = info
            .formats
            .iter()
            .find(|f| &f.label == label)
            .ok_or_else(|| {
                let labels: Vec<&str> = info.formats.iter().map(|f| f.label.as_str()).collect();
                if labels.is_empty() {
                    format!("没有列出任何档位，没法选「{label}」")
                } else {
                    format!("没有「{label}」这一档。可选：{}", labels.join(" / "))
                }
            })?;
        return Ok(to_pick(f));
    }
    if opts.best && !info.formats.is_empty() {
        return Ok(to_pick(&info.formats[0]));
    }
    if !info.video_url.is_empty() {
        return Ok(Pick::Direct {
            url: info.video_url.clone(),
            label: "默认档".into(),
        });
    }
    if let Some(f) = info.formats.first() {
        return Ok(to_pick(f));
    }
    Err("解析结果里没有可下载的媒体".into())
}

/// 直链请求头：按 CDN 域名补的 UA / Referer 之上，再叠解析结果里声明的
/// `video_headers`——解析器知道自己那家平台要什么，以它为准。
fn headers_for(info: &VideoInfo, url: &str) -> reqwest::header::HeaderMap {
    let mut h = media::headers_for(url);
    for (k, v) in &info.video_headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            h.insert(name, val);
        }
    }
    h
}

fn stem_of(info: &VideoInfo) -> String {
    if info.title.is_empty() {
        "media".to_owned()
    } else {
        info.title.clone()
    }
}

async fn download_video(info: &VideoInfo, opts: &Options) -> Result<(), String> {
    match pick(info, opts)? {
        Pick::Direct { url, label } => {
            println!("下载 {label}");
            fetch_to_file(
                &url,
                headers_for(info, &url),
                Path::new(&opts.dir),
                &stem_of(info),
                "mp4",
                "视频",
            )
            .await
            .map(|_| ())
        }
        Pick::Merge {
            video,
            audio,
            label,
        } => merge_download(info, &video, &audio, &label, opts).await,
    }
}

async fn download_music(info: &VideoInfo, dir: &str) -> Result<(), String> {
    if info.music_url.is_empty() {
        return Err("这个链接没有单独的背景音乐地址".into());
    }
    let url = info.music_url.as_str();
    fetch_to_file(
        url,
        headers_for(info, url),
        Path::new(dir),
        &stem_of(info),
        "m4a",
        "音乐",
    )
    .await
    .map(|_| ())
}

async fn download_gallery(info: &VideoInfo, dir: &str) -> Result<(), String> {
    let folder = Path::new(dir).join(media::safe_filename(&info.title, "gallery"));
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("建目录 {} 失败: {e}", folder.display()))?;
    let n = info.images.len();
    println!("图集 {n} 张 → {}", folder.display());
    for (i, img) in info.images.iter().enumerate() {
        // 序号命名的另一层用意：实况图成对落盘（01.jpg + 01.mov）
        let stem = format!("{:02}", i + 1);
        let what = format!("图片 {}/{}", i + 1, n);
        fetch_to_file(
            &img.url,
            headers_for(info, &img.url),
            &folder,
            &stem,
            "jpg",
            &what,
        )
        .await?;
        if !img.live_photo_url.is_empty() {
            fetch_to_file(
                &img.live_photo_url,
                headers_for(info, &img.live_photo_url),
                &folder,
                &stem,
                "mov",
                &format!("{what} 的实况视频"),
            )
            .await?;
        }
    }
    Ok(())
}

async fn merge_download(
    info: &VideoInfo,
    video_url: &str,
    audio_url: &str,
    label: &str,
    opts: &Options,
) -> Result<(), String> {
    let dir = Path::new(&opts.dir);
    // 先确认 ffmpeg 在不在，别两条轨都下完了才发现没法合
    let has_ffmpeg = tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    if !has_ffmpeg {
        return Err(format!(
            "「{label}」是音视频分离（DASH）档位，合并需要 ffmpeg，但 PATH 里没找到；\
             装好后重试，或用 --format 选一档直链（--list 看档位）"
        ));
    }

    println!("下载 {label}（视频轨 + 音频轨，ffmpeg 合并）");
    let video = fetch_to_file(
        video_url,
        headers_for(info, video_url),
        dir,
        &format!("{} [视频轨]", stem_of(info)),
        "mp4",
        "视频轨",
    )
    .await?;
    let audio = match fetch_to_file(
        audio_url,
        headers_for(info, audio_url),
        dir,
        &format!("{} [音频轨]", stem_of(info)),
        "m4a",
        "音频轨",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let _ = tokio::fs::remove_file(&video).await;
            return Err(e);
        }
    };

    let out = unique_path(dir, &media::safe_filename(&stem_of(info), "media"), "mp4");
    let status = tokio::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(&video)
        .arg("-i")
        .arg(&audio)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&out)
        .status()
        .await;
    // 不管成败，临时轨都清掉
    let _ = tokio::fs::remove_file(&video).await;
    let _ = tokio::fs::remove_file(&audio).await;
    match status {
        Ok(s) if s.success() => {
            println!("合并完成 → {}", out.display());
            Ok(())
        }
        Ok(s) => Err(format!("ffmpeg 合并失败（退出码 {:?}）", s.code())),
        Err(e) => Err(format!("起 ffmpeg 失败: {e}")),
    }
}

/// 流式下载到 `dir` 里，文件名 = 清洗后的 `stem` + 按 Content-Type / URL 定的
/// 扩展名。进度按 10% 一档打到 stderr——长流不给反馈就像卡死。
#[allow(clippy::cast_precision_loss)]
async fn fetch_to_file(
    url: &str,
    headers: reqwest::header::HeaderMap,
    dir: &Path,
    stem: &str,
    fallback_ext: &str,
    what: &str,
) -> Result<PathBuf, String> {
    let resp = media::client()
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("{what}下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "{what}下载失败: CDN 返回 HTTP {}",
            resp.status().as_u16()
        ));
    }
    let ext = media::pick_ext(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        url,
        fallback_ext,
    );
    let path = unique_path(dir, &media::safe_filename(stem, "media"), &ext);
    let total = resp.content_length();
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| format!("写文件 {} 失败: {e}", path.display()))?;

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let mut stream = resp.bytes_stream();
    let mut got: u64 = 0;
    let mut shown: u8 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("{what}下载中断: {e}"))?;
        file.write_all(chunk.as_ref())
            .await
            .map_err(|e| format!("写文件 {} 失败: {e}", path.display()))?;
        got += chunk.len() as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            let tenth = u8::try_from(got * 10 / total).unwrap_or(9).min(9);
            if tenth > shown {
                shown = tenth;
                eprint!("\r{what} {:>3}%", tenth * 10);
            }
        }
    }
    file.flush().await.map_err(|e| format!("写文件失败: {e}"))?;
    if shown > 0 {
        eprintln!();
    }
    println!("{what} → {}（{:.1} MB）", path.display(), got as f64 / 1e6);
    Ok(path)
}

/// 重名避让：`名.ext` 存在就试 `名 (2).ext`、`名 (3).ext`……
fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let candidate = |n: Option<u32>| match n {
        None => dir.join(format!("{stem}.{ext}")),
        Some(n) => dir.join(format!("{stem} ({n}).{ext}")),
    };
    let mut path = candidate(None);
    if !path.exists() {
        return path;
    }
    for n in 2..=9999 {
        path = candidate(Some(n));
        if !path.exists() {
            return path;
        }
    }
    path // 9999 个重名还没让开：就用最后一个，覆盖总比拒绝用户强
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parse_and_join_share_text() {
        let o = parse_args(&[
            "8SK 分享了一条视频".to_owned(),
            "https://v.douyin.com/abc/".to_owned(),
            "--dir".to_owned(),
            "out".to_owned(),
            "--best".to_owned(),
        ])
        .unwrap();
        assert_eq!(o.input, "8SK 分享了一条视频 https://v.douyin.com/abc/");
        assert_eq!(o.dir, "out");
        assert!(o.best);

        assert!(parse_args(&[]).is_err(), "没给链接要报错");
        assert!(
            parse_args(&["--what".to_owned(), "x".to_owned()]).is_err(),
            "不认识的参数要报错"
        );
        assert!(
            parse_args(&[
                "x".to_owned(),
                "--best".to_owned(),
                "--format".to_owned(),
                "1080p".to_owned()
            ])
            .is_err(),
            "--best 和 --format 互斥"
        );
    }

    #[test]
    fn unique_path_avoids_collisions() {
        let dir = std::env::temp_dir().join(format!("alcedo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "t", "mp4");
        assert_eq!(first, dir.join("t.mp4"));
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "t", "mp4");
        assert_eq!(second, dir.join("t (2).mp4"), "重名要让开");
        std::fs::remove_dir_all(&dir).ok();
    }
}
