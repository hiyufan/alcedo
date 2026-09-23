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
    // 样本数是手输的轮次，几十上百，转 f64 再取整没有精度问题
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let pct = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize];
    println!(
        "\n成功 {}/{rounds}  p50 {:.0}ms  p90 {:.0}ms  p99 {:.0}ms  最快 {:.0}ms  最慢 {:.0}ms",
        samples.len(),
        pct(0.50),
        pct(0.90),
        pct(0.99),
        samples[0],
        samples[samples.len() - 1],
    );
    // 尾延迟才是用户真正感觉到的。p90/p50 拉得开说明瓶颈是"偶尔一次很慢"，
    // 那时候该上对冲请求，而不是去抠中位数那几毫秒。
    println!("  p90/p50 = {:.1}×", pct(0.90) / pct(0.50));
}
