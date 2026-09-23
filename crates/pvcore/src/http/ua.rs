//! User-Agent 池。
//!
//! 替代 Python 侧的 `fake_useragent`：那个库每次构造要读一遍数据集（约 40ms），
//! 而且带一个几 MB 的 JSON 依赖。这里直接内置一张表，取值是 O(1) 且零分配。
//!
//! 重要的不是"每个请求都换 UA"，而是**一次解析里所有请求用同一个 UA**：
//! 同一个 IP 上一串对不上号的客户端本身就是风控信号，cookie 握手那类要求会话
//! 一致的流程也会直接失效。所以 UA 在 [`crate::http::Http`] 上取一次，全程复用。

use std::sync::atomic::{AtomicUsize, Ordering};

/// iOS Safari。国内平台的分享页基本都是按移动端渲染的，默认用这一组。
pub const IOS: &[&str] = &[
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 18_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.4 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPad; CPU OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1",
];

/// Android Chrome。
pub const ANDROID: &[&str] = &[
    "Mozilla/5.0 (Linux; Android 14; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Linux; Android 13; V2244A) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Mobile Safari/537.36",
];

/// 桌面 Chrome / Safari。B 站、新片场这类平台按桌面端返回的数据更全。
pub const DESKTOP: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15",
];

/// 固定的桌面 UA，给需要"UA 必须和上次一致"的场景（CDN 直链、签名接口）。
pub const DESKTOP_FIXED: &str = DESKTOP[0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Ios,
    Android,
    Desktop,
}

impl Platform {
    fn pool(self) -> &'static [&'static str] {
        match self {
            Platform::Ios => IOS,
            Platform::Android => ANDROID,
            Platform::Desktop => DESKTOP,
        }
    }
}

// 轮转而不是真随机：分布同样均匀，省掉一次 RNG 调用，而且同一进程内
// 连续两次解析拿到的 UA 必然不同——真随机反而可能连着撞同一个。
static CURSOR: AtomicUsize = AtomicUsize::new(0);

/// 取一个 UA。
pub fn pick(platform: Platform) -> &'static str {
    let pool = platform.pool();
    let i = CURSOR.fetch_add(1, Ordering::Relaxed);
    pool[i % pool.len()]
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn pools_are_non_empty_and_look_real() {
        for p in [Platform::Ios, Platform::Android, Platform::Desktop] {
            let pool = p.pool();
            assert!(!pool.is_empty());
            for ua in pool {
                assert!(ua.starts_with("Mozilla/5.0"), "不像真的 UA: {ua}");
                assert!(ua.is_ascii(), "UA 必须是 ASCII, 否则塞不进 header");
            }
        }
    }

    #[test]
    fn pick_rotates() {
        let a = pick(Platform::Ios);
        let b = pick(Platform::Ios);
        // 池子里不止一个, 连续两次必须不同
        assert_ne!(a, b);
    }
}
