//! 量一下预热到底省多少：同一条链接，冷启动 vs 预热后的首次解析。
//!
//! ```bash
//! cargo run --release -p alcedo --example prewarm_effect -- <url> <平台标识>
//! ```
//!
//! 只看**第一次**解析的耗时——预热省的就是这一次，后面本来就是热连接。

use std::str::FromStr;
use std::time::Instant;

use alcedo::{Client, Source};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(url), Some(src)) = (args.next(), args.next()) else {
        eprintln!("用法: prewarm_effect <url> <平台标识, 如 douyin>");
        std::process::exit(2);
    };
    let Ok(source) = Source::from_str(&src) else {
        eprintln!("认不出这个平台: {src}");
        std::process::exit(2);
    };

    // 每一轮都用新的 Client，但连接池是进程级的静态缓存，所以这里
    // 只能在同一个进程里对比"有没有提前发过请求"。
    let cold = {
        let c = Client::new().expect("初始化失败");
        let t = Instant::now();
        let ok = c.parse(&url).await.is_ok();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "冷启动首次解析 : {ms:7.1}ms  {}",
            if ok { "成功" } else { "失败" }
        );
        ms
    };

    // 连接此刻已经热了，再量一次当基线
    let warm = {
        let c = Client::new().expect("初始化失败");
        let t = Instant::now();
        let ok = c.parse(&url).await.is_ok();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "热连接解析     : {ms:7.1}ms  {}",
            if ok { "成功" } else { "失败" }
        );
        ms
    };

    // 预热本身花多久（这部分不占用户的时间）
    let t = Instant::now();
    let c = Client::new().expect("初始化失败");
    c.prewarm(&[source]).await;
    println!(
        "预热耗时       : {:7.1}ms  (不占用户等待)",
        t.elapsed().as_secs_f64() * 1000.0
    );

    println!(
        "\n冷启动比热连接多花 {:.0}ms —— 预热就是把这段挪到用户请求之外",
        cold - warm
    );
}
