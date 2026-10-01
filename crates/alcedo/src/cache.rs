//! 解析结果缓存。
//!
//! 单次解析的耗时里约 85% 是平台自己的响应等待，改不了。唯一还能拿下的大头
//! 就是**不发这次请求**——热门内容在短时间里会被反复解析。
//!
//! ## 为什么缓存这件事在这里有风险
//!
//! 平台给的直链是**带签名、会过期**的。缓存一条已经失效的地址比慢更糟：
//! 用户拿到的是一个 403，而且他不知道该重试。
//!
//! 所以 TTL 不是拍脑袋定的，而是**从直链自己身上读出来**（见 [`url_expiry`]）：
//!
//! - 抖音把过期时刻以十六进制写在路径段里（实测总有效期约 80 分钟）
//! - B 站用 `deadline` 参数（2 小时）
//! - 其它平台多用 `x-expires` / `expire` / `oe` 这类 query 参数
//!
//! 取所有地址里**最早**的那个过期时刻，减去一点安全余量，再和配置的上限取小。
//!
//! ## 为什么上限要短
//!
//! 就算地址还活着，**内容本身可能已经被删、被改、被设为私密**。TTL 越长，
//! 用户拿到过期事实的概率越大。而收益是递减的：同一条链接每小时被解析 100 次
//! 时，5 分钟的 TTL 已经能挡掉约 92%，拉到 60 分钟也只多挡 6%。用那 6% 去换
//! "内容没了还在返回成功"的窗口，不划算。
//!
//! ## 失败也缓存（负缓存）
//!
//! `deleted` / `unsupported` / `parse` 这类失败是**稳定事实**：再解析一遍，
//! 平台还是给同一个答案。死链被转发之后，每来一个人都真打一次平台毫无意义。
//! 所以非重试类失败也进缓存，固定 [`NEGATIVE_TTL`]（60 秒）后过期——足够
//! 挡掉一条死链的转发高峰，又不至于把"配好 cookie 就能好"的状态判死太久。
//! `blocked` / `network` / `timeout` 是暂时状态，**绝不**负缓存：过会儿可能
//! 就自己好了。换了 cookie、更新了解析器之后调 [`Cache::clear`] 立即清场。

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::error::{Error, Reason, Result};
use crate::model::VideoInfo;

/// 安全余量：地址快到期时就别再从缓存给了，留出下载的时间。
const SAFETY_MARGIN: Duration = Duration::from_secs(120);

/// 负缓存的固定 TTL。错误结果没有直链可读寿命，固定一个短值就够。
const NEGATIVE_TTL: Duration = Duration::from_secs(60);

/// 缓存里的一条：要么是成功结果，要么是短命的失败（负缓存）。
/// `Ok` 装 `Box` 是因为 `VideoInfo` 比错误大一个量级，枚举按大的算。
enum Value {
    Ok(Box<VideoInfo>),
    Err { reason: Reason, detail: String },
}

struct Entry {
    key: String,
    value: Value,
    expires_at: Instant,
    /// 命中计数，淘汰时优先留热的
    hits: u32,
}

/// 带 TTL 的结果缓存。容量满了按"命中少 + 快过期"淘汰。
pub struct Cache {
    entries: Mutex<Vec<Entry>>,
    ttl: Duration,
    /// 失败结果（负缓存）自己的 TTL，和 `ttl` 分开算
    negative_ttl: Duration,
    capacity: usize,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("len", &self.entries.lock().len())
            .field("ttl", &self.ttl)
            .field("negative_ttl", &self.negative_ttl)
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl Cache {
    /// `ttl` 为 0 表示不缓存（负缓存也一起关）。
    pub const fn new(ttl: Duration, capacity: usize) -> Self {
        Self::with_negative_ttl(ttl, NEGATIVE_TTL, capacity)
    }

    /// 负缓存 TTL 单独指定的版本。测试用来把 60 秒缩到毫秒级。
    pub const fn with_negative_ttl(ttl: Duration, negative_ttl: Duration, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            ttl,
            negative_ttl,
            capacity,
        }
    }

    /// 配置上是否启用（TTL 和容量都不为 0）。
    pub const fn is_enabled(&self) -> bool {
        !self.ttl.is_zero() && self.capacity > 0
    }

    /// 取一条还没过期的记录；负缓存命中时返回带同样原因的 [`Error`]。
    pub fn get(&self, key: &str) -> Option<Result<VideoInfo>> {
        if !self.is_enabled() {
            return None;
        }
        let mut entries = self.entries.lock();
        let now = Instant::now();
        let hit = entries.iter_mut().find(|e| e.key == key)?;
        if hit.expires_at <= now {
            return None; // 留着让 put 覆盖，不在读路径上做清理
        }
        hit.hits = hit.hits.saturating_add(1);
        Some(match &hit.value {
            Value::Ok(info) => Ok(info.as_ref().clone()),
            Value::Err { reason, detail } => Err(Error::new(*reason, detail.clone())),
        })
    }

    /// 存一条成功结果。TTL 取「配置上限」和「直链自身剩余寿命」里更短的那个。
    ///
    /// 直链已经快过期、或者压根读不出寿命又没有可用地址时，不缓存。
    pub fn put(&self, key: &str, info: &VideoInfo) {
        if !self.is_enabled() {
            return;
        }
        let Some(ttl) = self.effective_ttl(info) else {
            return;
        };
        self.insert(key, Value::Ok(Box::new(info.clone())), ttl);
    }

    /// 存一条失败结果（负缓存），取舍见模块文档。重试类原因直接忽略。
    pub fn put_error(&self, key: &str, err: &Error) {
        if !self.is_enabled() || self.negative_ttl.is_zero() || err.reason.is_retryable() {
            return;
        }
        self.insert(
            key,
            Value::Err {
                reason: err.reason,
                detail: err.detail.clone(),
            },
            self.negative_ttl,
        );
    }

    /// 插入或覆盖一条，带容量淘汰。成功和失败走同一套，谁后写谁说了算。
    fn insert(&self, key: &str, value: Value, ttl: Duration) {
        let now = Instant::now();
        let mut entries = self.entries.lock();

        // 顺手清掉过期的，省得单开一个清理任务
        entries.retain(|e| e.expires_at > now);

        if let Some(slot) = entries.iter_mut().find(|e| e.key == key) {
            slot.value = value;
            slot.expires_at = now + ttl;
            return;
        }

        if entries.len() >= self.capacity {
            // 淘汰命中最少的；同样冷的里面挑最快过期的
            if let Some((i, _)) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| (e.hits, e.expires_at))
            {
                entries.swap_remove(i);
            }
        }
        entries.push(Entry {
            key: key.to_owned(),
            value,
            expires_at: now + ttl,
            hits: 0,
        });
    }

    /// 这条结果能缓存多久。`None` = 别缓存。
    fn effective_ttl(&self, info: &VideoInfo) -> Option<Duration> {
        let now_unix = unix_now()?;
        match earliest_expiry(info) {
            Some(exp) => {
                // 已经过期或快到期的，缓存了也是给用户一个 403
                let remain = exp.saturating_sub(now_unix);
                let usable = Duration::from_secs(remain).checked_sub(SAFETY_MARGIN)?;
                if usable.is_zero() {
                    None
                } else {
                    Some(usable.min(self.ttl))
                }
            }
            // 读不出过期时间（图集、HLS 清单这类）：按配置上限来。
            // 这些地址通常没有短签名，风险主要在"内容变了"，上限已经压住了。
            None => Some(self.ttl),
        }
    }

    /// 当前条目数，给 `/health` 之类用。
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// 缓存里一条都没有。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空。平台改版、换了 cookie 之后该调一次。
    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}

fn unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// 结果里所有直链中最早的那个过期时刻（Unix 秒）。
///
/// 主播封面、图集这类没有直链的结果返回 `None`——那不是"马上过期"，而是
/// "读不出"，调用方自己决定按什么兜底。[`crate::model::VideoInfo`] 的
/// `expires_at` 字段就是这个值。
pub fn earliest_expiry(info: &VideoInfo) -> Option<u64> {
    std::iter::once(info.video_url.as_str())
        .chain(
            info.formats
                .iter()
                .flat_map(|f| [f.url.as_str(), f.video_url.as_str(), f.audio_url.as_str()]),
        )
        .filter(|u| !u.is_empty())
        .filter_map(url_expiry)
        .min()
}

/// 从直链里读出它自己的过期时刻（Unix 秒）。
///
/// 各家写法不一样，这里覆盖实测见过的几种。读不出来返回 `None`——那时候
/// 由调用方决定按配置上限还是干脆不缓存。
pub fn url_expiry(url: &str) -> Option<u64> {
    // 合理区间：2020-01-01 之后、当前时间 + 30 天以内。
    // 定这个界是因为下面两种来源都可能误命中——比如路径里某段十六进制
    // 恰好能解析成数字，或者某个 query 参数叫 expire 但装的是别的东西。
    let now = unix_now()?;
    let plausible = |t: u64| t > 1_577_836_800 && t < now + 30 * 86_400;

    let parsed = url::Url::parse(url).ok()?;

    // 1) query 参数。x-expires / deadline / expire / oe(YouTube, 十六进制)
    for (k, v) in parsed.query_pairs() {
        let key = k.to_ascii_lowercase();
        let t = match key.as_str() {
            "x-expires" | "expires" | "expire" | "deadline" | "valid_to" | "e" => {
                v.parse::<u64>().ok()
            }
            // YouTube 的 oe 是十六进制
            "oe" => u64::from_str_radix(&v, 16).ok(),
            _ => None,
        };
        if let Some(t) = t.filter(|t| plausible(*t)) {
            return Some(t);
        }
    }

    // 2) 路径段里的十六进制时间戳。抖音 CDN 就是这么写的：
    //    /<签名>/<过期时刻的十六进制>/video/tos/...
    //    只认 8 位十六进制，且落在合理区间，避免把别的哈希段误当成时间。
    parsed
        .path_segments()?
        .filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .filter_map(|s| u64::from_str_radix(s, 16).ok())
        .find(|t| plausible(*t))
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::model::Format;

    fn now() -> u64 {
        unix_now().unwrap()
    }

    fn info_with(url: &str) -> VideoInfo {
        VideoInfo {
            video_url: url.to_owned(),
            title: "t".into(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_expiry_from_query_params() {
        let t = now() + 7200;
        // B 站
        assert_eq!(
            url_expiry(&format!("https://x.bilivideo.com/a.mp4?deadline={t}&x=1")),
            Some(t)
        );
        // 小红书 / 通用
        assert_eq!(
            url_expiry(&format!("https://sns.xhscdn.com/a.jpg?x-expires={t}")),
            Some(t)
        );
    }

    #[test]
    fn reads_youtube_style_hex_param() {
        let t = now() + 3600;
        let hex = format!("{t:x}");
        assert_eq!(
            url_expiry(&format!("https://r1.googlevideo.com/v?oe={hex}&x=2")),
            Some(t)
        );
    }

    #[test]
    fn reads_douyin_style_hex_path_segment() {
        // 抖音把过期时刻写成路径里的一段十六进制
        let t = now() + 4800;
        let hex = format!("{t:x}");
        let u = format!("https://v11-default.365yg.com/abcdef0123456789abcdef0123456789/{hex}/video/tos/cn/x/?a=0");
        assert_eq!(url_expiry(&u), Some(t));
    }

    #[test]
    fn ignores_hash_segments_that_are_not_timestamps() {
        // 签名段也是十六进制，但换算出来不是合理时间，不能误当过期时刻
        let u = "https://v11.365yg.com/82b540c91294c68c7054b96bfa2b80d3/deadbeef/video/x/";
        // 0xdeadbeef = 3735928559，远在 30 天之后
        assert_eq!(url_expiry(u), None);
    }

    #[test]
    fn ignores_already_absurd_values() {
        assert_eq!(url_expiry("https://x/a.mp4?deadline=1"), None);
        assert_eq!(url_expiry("https://x/a.mp4?deadline=99999999999"), None);
        assert_eq!(url_expiry("https://x/a.mp4"), None);
        assert_eq!(url_expiry("不是个链接"), None);
    }

    #[test]
    fn ttl_is_capped_by_config() {
        let cache = Cache::new(Duration::from_secs(300), 8);
        // 地址还能活 2 小时，但配置上限是 5 分钟
        let t = now() + 7200;
        let info = info_with(&format!("https://x/a.mp4?deadline={t}"));
        assert_eq!(
            cache.effective_ttl(&info),
            Some(Duration::from_secs(300)),
            "上限必须压住长寿命地址——地址活着不代表内容还在"
        );
    }

    #[test]
    fn ttl_is_capped_by_url_lifetime() {
        let cache = Cache::new(Duration::from_secs(3600), 8);
        // 地址只剩 5 分钟，减掉 2 分钟余量 = 3 分钟
        let t = now() + 300;
        let info = info_with(&format!("https://x/a.mp4?deadline={t}"));
        let ttl = cache.effective_ttl(&info).expect("应当可缓存");
        assert!(
            ttl <= Duration::from_secs(181) && ttl >= Duration::from_secs(175),
            "TTL 应当约等于剩余寿命减去安全余量, 实际 {ttl:?}"
        );
    }

    #[test]
    fn about_to_expire_is_not_cached() {
        let cache = Cache::new(Duration::from_secs(3600), 8);
        // 只剩 1 分钟，比安全余量还短——缓存它等于给用户发 403
        let info = info_with(&format!("https://x/a.mp4?deadline={}", now() + 60));
        assert_eq!(cache.effective_ttl(&info), None);
    }

    #[test]
    fn earliest_expiry_among_all_urls_wins() {
        let cache = Cache::new(Duration::from_secs(3600), 8);
        let mut info = info_with(&format!("https://x/a.mp4?deadline={}", now() + 7200));
        info.formats.push(Format {
            // 这一档只剩 5 分钟，整条结果就只能按它算
            url: format!("https://x/b.mp4?deadline={}", now() + 300),
            ..Format::direct("720p", "", 720)
        });
        let ttl = cache.effective_ttl(&info).expect("应当可缓存");
        assert!(
            ttl < Duration::from_secs(200),
            "该按最早过期的那个算, 实际 {ttl:?}"
        );
    }

    #[test]
    fn urls_without_expiry_use_the_configured_cap() {
        let cache = Cache::new(Duration::from_secs(300), 8);
        let info = info_with("https://x/playlist.m3u8");
        assert_eq!(cache.effective_ttl(&info), Some(Duration::from_secs(300)));
    }

    #[test]
    fn round_trip_and_miss() {
        let cache = Cache::new(Duration::from_secs(300), 8);
        let info = info_with("https://x/a.mp4");
        assert!(cache.get("k").is_none());
        cache.put("k", &info);
        assert_eq!(
            cache.get("k").unwrap().unwrap().video_url,
            "https://x/a.mp4"
        );
        assert!(cache.get("别的key").is_none());
    }

    #[test]
    fn negative_entry_round_trips_the_error() {
        let cache = Cache::new(Duration::from_secs(300), 8);
        cache.put_error("k", &Error::deleted("HTTP 404"));
        let err = cache.get("k").unwrap().unwrap_err();
        assert_eq!(err.reason, Reason::Deleted);
        assert_eq!(err.detail, "HTTP 404");
        // 成功结果随后覆盖负缓存：内容可能被恢复（私密转公开）
        cache.put("k", &info_with("https://x/a.mp4"));
        assert!(cache.get("k").unwrap().is_ok());
    }

    #[test]
    fn error_over_a_success_slot_follows_the_same_rule() {
        // 反向覆盖也要成立：内容被删后，负缓存顶掉旧的成功结果
        let cache = Cache::new(Duration::from_secs(300), 8);
        cache.put("k", &info_with("https://x/a.mp4"));
        cache.put_error("k", &Error::deleted("没了"));
        assert_eq!(cache.get("k").unwrap().unwrap_err().reason, Reason::Deleted);
    }

    #[test]
    fn retryable_failures_are_never_negative_cached() {
        // 风控 / 断网 / 超时是暂时状态，缓存它们等于把"过会儿就好了"判成死
        let cache = Cache::new(Duration::from_secs(300), 8);
        cache.put_error("k", &Error::new(Reason::Blocked, "HTTP 429"));
        cache.put_error("k", &Error::bare(Reason::Timeout));
        cache.put_error("k", &Error::new(Reason::Network, "refused"));
        assert!(cache.get("k").is_none());
    }

    #[test]
    fn negative_entries_expire_on_their_own_ttl() {
        let cache =
            Cache::with_negative_ttl(Duration::from_secs(300), Duration::from_millis(30), 8);
        cache.put_error("k", &Error::deleted("没了"));
        assert!(cache.get("k").is_some(), "刚存的错误应当立刻可读");
        std::thread::sleep(Duration::from_millis(60));
        assert!(cache.get("k").is_none(), "负缓存按自己的 TTL 过期");
    }

    #[test]
    fn zero_negative_ttl_disables_it_without_touching_success() {
        let cache = Cache::with_negative_ttl(Duration::from_secs(300), Duration::ZERO, 8);
        cache.put_error("k", &Error::deleted("没了"));
        assert!(cache.get("k").is_none());
        cache.put("k", &info_with("https://x/a.mp4"));
        assert!(cache.get("k").unwrap().is_ok());
    }

    #[test]
    fn disabled_cache_never_stores() {
        let cache = Cache::new(Duration::ZERO, 8);
        assert!(!cache.is_enabled());
        cache.put("k", &info_with("https://x/a.mp4"));
        assert!(cache.get("k").is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn capacity_evicts_the_coldest() {
        let cache = Cache::new(Duration::from_secs(300), 2);
        cache.put("hot", &info_with("https://x/1.mp4"));
        cache.put("cold", &info_with("https://x/2.mp4"));
        // 让 hot 真的热起来
        for _ in 0..5 {
            let _ = cache.get("hot");
        }
        cache.put("new", &info_with("https://x/3.mp4"));

        assert_eq!(cache.len(), 2);
        assert!(cache.get("hot").is_some(), "命中多的那条不该被淘汰");
        assert!(cache.get("new").is_some());
        assert!(cache.get("cold").is_none(), "最冷的那条该被淘汰");
    }

    #[test]
    fn earliest_expiry_takes_the_min_and_reports_none_when_unreadable() {
        let mut info = info_with(&format!("https://x/a.mp4?deadline={}", now() + 7200));
        info.formats.push(Format {
            url: format!("https://x/b.mp4?deadline={}", now() + 300),
            ..Format::direct("720p", "", 720)
        });
        let exp = earliest_expiry(&info).expect("应当读得出");
        assert!(
            (exp as i64 - (now() + 300) as i64).abs() <= 1,
            "该取最早的那个"
        );

        // 图集没有带签名的直链，读不出就是 None，调用方自己兜底
        let gallery = VideoInfo {
            images: vec![crate::model::Image::new("https://x/1.jpg")],
            ..Default::default()
        };
        assert_eq!(earliest_expiry(&gallery), None);
    }

    #[test]
    fn clear_empties_everything() {
        let cache = Cache::new(Duration::from_secs(300), 8);
        cache.put("k", &info_with("https://x/a.mp4"));
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }
}
