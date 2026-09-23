//! 限流 + 熔断：按平台隔离，别让一个挂掉的平台拖垮全部解析。
//!
//! 每个平台一个令牌桶 + 一个熔断器。有一处判断需要特别说明：
//!
//! **只有平台侧的故障才计入熔断。** 用户连着贴 5 条失效链接会得到 5 个
//! `deleted`，那是内容没了，不是平台挂了——拿它去开熔断器，等于让用户的手滑
//! 把整个平台停掉几分钟。真正该计数的是 `blocked` / `network` / `timeout`
//! 这类"对面不让我们进"的信号。
//!
//! 限流同理：抖音被限流不该影响正在解析 YouTube 的请求，所以桶是按平台分的。

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::error::Reason;
use crate::model::Source;

/// 令牌桶。
#[derive(Debug)]
struct Bucket {
    /// 当前令牌数（浮点是为了能按经过时间连续补充）
    tokens: f64,
    capacity: f64,
    /// 每秒补充多少
    refill_per_sec: f64,
    last: Instant,
}

impl Bucket {
    fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec,
            last: Instant::now(),
        }
    }

    /// 取一个令牌；不够就返回还要等多久。
    fn take(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let need = (1.0 - self.tokens) / self.refill_per_sec;
            Some(Duration::from_secs_f64(need))
        }
    }
}

/// 熔断器状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breaker {
    /// 正常放行
    Closed,
    /// 已熔断，到点之前一律快速失败
    Open,
    /// 冷却结束，放一个探针进去试水
    HalfOpen,
}

#[derive(Debug)]
struct Circuit {
    state: Breaker,
    /// 连续的平台侧失败次数
    consecutive_failures: u32,
    /// Open 状态的解除时间
    opened_until: Option<Instant>,
    /// 连续熔断次数，用来指数退避
    trips: u32,
}

impl Circuit {
    const fn new() -> Self {
        Self {
            state: Breaker::Closed,
            consecutive_failures: 0,
            opened_until: None,
            trips: 0,
        }
    }
}

/// 一个平台的闸门。
#[derive(Debug)]
struct Gate {
    source: Source,
    bucket: Bucket,
    circuit: Circuit,
}

/// 限流 / 熔断的可调参数。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 突发容量
    pub burst: f64,
    /// 稳态速率（每秒请求数）
    pub rate_per_sec: f64,
    /// 连续多少次平台侧失败后熔断
    pub failure_threshold: u32,
    /// 首次熔断的冷却时长，之后按次数翻倍
    pub cooldown: Duration,
    /// 冷却的上限
    pub max_cooldown: Duration,
    /// 等令牌最多等多久，超过就直接报限流（别让用户干等）
    pub max_wait: Duration,
}

impl Limits {
    /// `Default` 的 const 版本：全局 [`Governor`] 是 `static`，构造必须是常量求值。
    pub const fn const_default() -> Self {
        Self {
            burst: 12.0,
            rate_per_sec: 6.0,
            failure_threshold: 5,
            cooldown: Duration::from_secs(15),
            max_cooldown: Duration::from_secs(300),
            max_wait: Duration::from_secs(3),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::const_default()
    }
}

/// 全部平台的闸门。
#[derive(Debug)]
pub struct Governor {
    gates: Mutex<Vec<Gate>>,
    limits: Limits,
}

/// 闸门放行的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 放行
    Go,
    /// 需要先等这么久（令牌不够）
    Wait(Duration),
    /// 熔断中，还有这么久解除
    Tripped(Duration),
}

impl Governor {
    /// 按给定参数建一个闸门集合。
    pub const fn new(limits: Limits) -> Self {
        Self {
            gates: Mutex::new(Vec::new()),
            limits,
        }
    }

    fn with_gate<T>(&self, source: Source, f: impl FnOnce(&mut Gate, &Limits) -> T) -> T {
        let mut gates = self.gates.lock();
        let idx = gates
            .iter()
            .position(|g| g.source == source)
            .unwrap_or_else(|| {
                gates.push(Gate {
                    source,
                    bucket: Bucket::new(self.limits.burst, self.limits.rate_per_sec),
                    circuit: Circuit::new(),
                });
                gates.len() - 1
            });
        f(&mut gates[idx], &self.limits)
    }

    /// 发请求之前问一下。
    pub fn check(&self, source: Source) -> Decision {
        self.with_gate(source, |gate, limits| {
            // 先看熔断：熔断中就没必要消耗令牌
            if gate.circuit.state == Breaker::Open {
                match gate.circuit.opened_until {
                    Some(until) if Instant::now() < until => {
                        return Decision::Tripped(until - Instant::now());
                    }
                    _ => {
                        // 冷却到点，放一个探针
                        gate.circuit.state = Breaker::HalfOpen;
                    }
                }
            }

            match gate.bucket.take() {
                None => Decision::Go,
                Some(wait) if wait <= limits.max_wait => Decision::Wait(wait),
                Some(wait) => Decision::Tripped(wait),
            }
        })
    }

    /// 请求成功了。
    pub fn on_success(&self, source: Source) {
        self.with_gate(source, |gate, _| {
            gate.circuit.state = Breaker::Closed;
            gate.circuit.consecutive_failures = 0;
            gate.circuit.trips = 0;
            gate.circuit.opened_until = None;
        });
    }

    /// 请求失败了。只有平台侧的故障才会推动熔断。
    pub fn on_failure(&self, source: Source, reason: Reason) {
        if !counts_toward_tripping(reason) {
            // 内容没了 / 不支持这个链接，是这一条的事，跟平台健康无关。
            // 半开状态下这也算"平台能正常响应"，所以顺手恢复。
            if reason == Reason::Deleted || reason == Reason::Unsupported {
                self.on_success(source);
            }
            return;
        }

        self.with_gate(source, |gate, limits| {
            gate.circuit.consecutive_failures += 1;
            let half_open_probe_failed = gate.circuit.state == Breaker::HalfOpen;

            if half_open_probe_failed
                || gate.circuit.consecutive_failures >= limits.failure_threshold
            {
                gate.circuit.trips = gate.circuit.trips.saturating_add(1);
                // 指数退避：平台没缓过来时别一直去戳
                let factor = 1u32 << (gate.circuit.trips - 1).min(5);
                let cooldown = (limits.cooldown * factor).min(limits.max_cooldown);
                gate.circuit.state = Breaker::Open;
                gate.circuit.opened_until = Some(Instant::now() + cooldown);
                gate.circuit.consecutive_failures = 0;
                tracing::warn!(
                    source = source.as_str(),
                    cooldown_secs = cooldown.as_secs(),
                    "平台连续失败，熔断"
                );
            }
        });
    }

    /// 给 `/health` 之类的接口看的快照。
    pub fn snapshot(&self) -> Vec<(Source, &'static str, u64)> {
        let now = Instant::now();
        self.gates
            .lock()
            .iter()
            .map(|g| {
                let state = match g.circuit.state {
                    Breaker::Closed => "closed",
                    Breaker::Open => "open",
                    Breaker::HalfOpen => "half-open",
                };
                let remain = g
                    .circuit
                    .opened_until
                    .and_then(|u| u.checked_duration_since(now))
                    .map_or(0, |d| d.as_secs());
                (g.source, state, remain)
            })
            .collect()
    }
}

/// 这个失败原因说明"平台整体不让我们进"吗？
///
/// 关键判断。归错了的后果很具体：把 `deleted` 算进去，用户连贴几条失效链接
/// 就能把平台熔断，之后正常链接也解析不了。
const fn counts_toward_tripping(reason: Reason) -> bool {
    matches!(
        reason,
        Reason::Blocked | Reason::Network | Reason::Timeout | Reason::Login
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_limits() -> Limits {
        Limits {
            burst: 3.0,
            rate_per_sec: 1000.0, // 补充快，测试里不真等
            failure_threshold: 3,
            cooldown: Duration::from_millis(50),
            max_cooldown: Duration::from_millis(200),
            max_wait: Duration::from_millis(10),
        }
    }

    #[test]
    fn bucket_allows_burst_then_throttles() {
        let mut b = Bucket::new(3.0, 0.0001); // 几乎不补充
        assert!(b.take().is_none());
        assert!(b.take().is_none());
        assert!(b.take().is_none());
        assert!(b.take().is_some(), "第 4 次该被限住");
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = Bucket::new(1.0, 1000.0);
        assert!(b.take().is_none());
        std::thread::sleep(Duration::from_millis(5));
        assert!(b.take().is_none(), "补充之后该放行");
    }

    #[test]
    fn platform_failures_trip_the_breaker() {
        let g = Governor::new(fast_limits());
        for _ in 0..3 {
            assert_eq!(g.check(Source::DouYin), Decision::Go);
            g.on_failure(Source::DouYin, Reason::Blocked);
        }
        assert!(
            matches!(g.check(Source::DouYin), Decision::Tripped(_)),
            "连续被风控该熔断"
        );
    }

    #[test]
    fn dead_links_do_not_trip_the_breaker() {
        // 这是最重要的一条：用户连贴失效链接不该把平台停掉
        let g = Governor::new(fast_limits());
        for _ in 0..20 {
            g.on_failure(Source::DouYin, Reason::Deleted);
        }
        assert_eq!(
            g.check(Source::DouYin),
            Decision::Go,
            "内容不存在跟平台健康无关，不该熔断"
        );
    }

    #[test]
    fn unsupported_links_do_not_trip_either() {
        let g = Governor::new(fast_limits());
        for _ in 0..20 {
            g.on_failure(Source::BiliBili, Reason::Unsupported);
        }
        assert_eq!(g.check(Source::BiliBili), Decision::Go);
    }

    #[test]
    fn platforms_are_isolated() {
        let g = Governor::new(fast_limits());
        for _ in 0..3 {
            g.on_failure(Source::DouYin, Reason::Blocked);
        }
        assert!(matches!(g.check(Source::DouYin), Decision::Tripped(_)));
        assert_eq!(
            g.check(Source::YouTube),
            Decision::Go,
            "抖音挂了不该影响 YouTube"
        );
    }

    #[test]
    fn success_resets_the_failure_count() {
        let g = Governor::new(fast_limits());
        g.on_failure(Source::DouYin, Reason::Network);
        g.on_failure(Source::DouYin, Reason::Network);
        g.on_success(Source::DouYin);
        g.on_failure(Source::DouYin, Reason::Network);
        assert_eq!(
            g.check(Source::DouYin),
            Decision::Go,
            "中间成功过就该重新计数"
        );
    }

    #[test]
    fn half_open_probe_recovers_on_success() {
        let g = Governor::new(fast_limits());
        for _ in 0..3 {
            g.on_failure(Source::DouYin, Reason::Timeout);
        }
        assert!(matches!(g.check(Source::DouYin), Decision::Tripped(_)));

        std::thread::sleep(Duration::from_millis(60)); // 等冷却
        assert_eq!(g.check(Source::DouYin), Decision::Go, "冷却后该放探针进去");

        g.on_success(Source::DouYin);
        assert_eq!(g.check(Source::DouYin), Decision::Go, "探针成功就该恢复");
    }

    #[test]
    fn half_open_probe_failure_reopens_immediately() {
        let g = Governor::new(fast_limits());
        for _ in 0..3 {
            g.on_failure(Source::DouYin, Reason::Blocked);
        }
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(g.check(Source::DouYin), Decision::Go); // 探针

        // 探针也失败：不该等攒够 threshold 次才重新熔断
        g.on_failure(Source::DouYin, Reason::Blocked);
        assert!(
            matches!(g.check(Source::DouYin), Decision::Tripped(_)),
            "半开探针失败应当立刻重新熔断"
        );
    }

    #[test]
    fn cooldown_backs_off_but_is_capped() {
        let g = Governor::new(fast_limits());
        let mut last = Duration::ZERO;
        for round in 0..6 {
            for _ in 0..3 {
                g.on_failure(Source::DouYin, Reason::Blocked);
            }
            if let Decision::Tripped(d) = g.check(Source::DouYin) {
                if round > 0 {
                    assert!(
                        d >= last || d >= Duration::from_millis(190),
                        "应当递增或已封顶"
                    );
                }
                last = d;
            }
            std::thread::sleep(Duration::from_millis(210)); // 超过 max_cooldown
            let _ = g.check(Source::DouYin);
        }
        assert!(last <= Duration::from_millis(200), "冷却必须有上限");
    }

    #[test]
    fn snapshot_reports_state() {
        let g = Governor::new(fast_limits());
        g.on_success(Source::Vimeo);
        let snap = g.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, Source::Vimeo);
        assert_eq!(snap[0].1, "closed");
    }
}
