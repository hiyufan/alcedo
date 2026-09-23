//! 端到端延迟测量。
//!
//! ```bash
//! cargo run --release -p alcedo --example bench -- <url> [次数]
//! ```
//!
//! 测的是**一个进程内重复解析同一条链接**：这是服务端的真实形态，也是连接池
//! 复用能不能生效的地方。第一次含 DNS + TLS 握手，后面几次走热连接。
//!
//! 网络抖动会盖过不少差异，所以看中位数，不看单次。

use std::time::Instant;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let url = match args.next() {
        Some(u) => u,
        None => {
            eprintln!("用法: bench <url> [次数]");
            std::process::exit(2);
        }
    };
    let rounds: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(5);

    let started = Instant::now();
    let client = alcedo::Client::new().expect("初始化失败");
    println!(
        "构造 Client: {:.1}ms",
        started.elapsed().as_secs_f64() * 1000.0
    );

    let mut samples = Vec::with_capacity(rounds);
    for i in 1..=rounds {
        let t = Instant::now();
        let result = client.parse(&url).await;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(info) => {
                samples.push(ms);
                println!(
                    "  第 {i} 次: {ms:7.1}ms  ({} 档 / {})",
                    info.formats.len(),
                    if info.title.is_empty() {
                        "无标题"
                    } else {
                        &info.title
                    }
                );
            }
            Err(e) => println!("  第 {i} 次: {ms:7.1}ms  失败 [{}] {e}", e.reason),
        }
    }

    if samples.is_empty() {
        eprintln!("没有成功的样本");
        std::process::exit(1);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = samples[samples.len() / 2];
    let first = samples.first().copied().unwrap_or(0.0);
    println!(
        "\n成功 {}/{rounds}  最快 {first:.1}ms  中位 {median:.1}ms",
        samples.len()
    );
}
