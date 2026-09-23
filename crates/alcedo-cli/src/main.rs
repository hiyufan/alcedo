//! `alcedo` —— 命令行入口。
//!
//! ```text
//! alcedo <链接或分享文案>          # 人类可读摘要
//! alcedo --json <链接>             # 机器可读 JSON
//! alcedo --list                    # 支持的平台
//! ```
//!
//! 没有引 clap：参数就这几个，手写解析省掉一个中等体量的依赖和它的编译时间。

use std::process::ExitCode;

use alcedo::{Client, Source, VideoInfo};

fn main() -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("启动运行时失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(run())
}

async fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--list") {
        print_supported();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("alcedo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let as_json = args.iter().any(|a| a == "--json");
    let input: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    if input.is_empty() {
        eprintln!("要解析什么？给一条分享链接。");
        return ExitCode::FAILURE;
    }
    // 分享文案里带空格，shell 会切成多个参数，拼回去
    let input = input.join(" ");

    let client = match Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("初始化失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    match client.parse(&input).await {
        Ok(info) => {
            if as_json {
                match serde_json::to_string_pretty(&info) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        eprintln!("序列化失败: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print_human(&info);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("解析失败 [{}] {}", e.reason, e);
            ExitCode::FAILURE
        }
    }
}

fn print_human(info: &VideoInfo) {
    let src = info.source.map_or("未知", Source::display_name);
    println!("平台  : {src}");
    if !info.title.is_empty() {
        println!("标题  : {}", info.title);
    }
    if !info.author.name.is_empty() {
        println!("作者  : {}", info.author.name);
    }
    if info.duration > 0.0 {
        println!("时长  : {:.1}s", info.duration);
    }
    if info.width > 0 && info.height > 0 {
        println!("尺寸  : {}x{}", info.width, info.height);
    }
    if !info.video_url.is_empty() {
        println!("视频  : {}", info.video_url);
    }
    if !info.cover_url.is_empty() {
        println!("封面  : {}", info.cover_url);
    }
    if !info.music_url.is_empty() {
        println!("音频  : {}", info.music_url);
    }
    if !info.images.is_empty() {
        println!("图集  : {} 张", info.images.len());
        for (i, img) in info.images.iter().enumerate() {
            let live = if img.live_photo_url.is_empty() {
                ""
            } else {
                "  [实况]"
            };
            println!("  {:>2}. {}{live}", i + 1, img.url);
        }
    }
    if !info.formats.is_empty() {
        println!("清晰度:");
        for f in &info.formats {
            // 只是显示成 MB，精度损失无所谓
            #[allow(clippy::cast_precision_loss)]
            let size = if f.filesize > 0 {
                format!("  {:.1} MB", f.filesize as f64 / 1e6)
            } else {
                String::new()
            };
            let merge = if f.needs_merge() {
                "  (需合并音视频)"
            } else {
                ""
            };
            println!("  - {}{size}{merge}", f.label);
        }
    }
    if !info.video_headers.is_empty() {
        println!("请求头: {:?}", info.video_headers);
    }
}

fn print_supported() {
    let all = alcedo::registry::supported();
    println!("支持 {} 个平台：", all.len());
    for s in all {
        println!(
            "  {:<14} {:<12} {}",
            s.as_str(),
            s.display_name(),
            alcedo::registry::domains_of(s).join(", ")
        );
    }
}

fn print_help() {
    println!(
        "alcedo {ver} —— 视频平台解析器

用法:
  alcedo <链接或分享文案>     解析并打印摘要
  alcedo --json <链接>        输出 JSON
  alcedo --list               列出支持的平台
  alcedo --version            版本号

环境变量:
  ALCEDO_PROXY                所有平台的代理, 如 http://127.0.0.1:7890
  ALCEDO_PROXY_CN             只给国内平台用的代理 (境外部署时需要)
  ALCEDO_BILI_COOKIE          B 站登录 cookie, 能拿到更高清晰度
  ALCEDO_XHS_COOKIE           小红书登录 cookie
  ALCEDO_REQUEST_TIMEOUT      单个请求超时秒数 (默认 20)
  ALCEDO_TOTAL_TIMEOUT        整体解析超时秒数 (默认 45)
  ALCEDO_SSRF_DNS=0           关闭 DNS 层的内网地址拦截 (自建镜像时才需要)",
        ver = env!("CARGO_PKG_VERSION")
    );
}
