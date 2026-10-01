//! # alcedo
//!
//! 视频平台解析核心：给一条分享链接，返回无水印直链、图集、封面、作者和各档清晰度。
//!
//! ```no_run
//! # async fn demo() -> alcedo::Result<()> {
//! let client = alcedo::Client::new()?;
//! let info = client.parse("https://v.douyin.com/iRNBho6u/").await?;
//! println!("{} -> {}", info.title, info.video_url);
//! # Ok(()) }
//! ```
//!
//! ## 设计要点
//!
//! - **连接复用**：进程内按代理配置共享 [`reqwest::Client`]，一次解析里的多个
//!   请求共用连接，不会各自重新握手。
//! - **静态分发**：平台识别后走 `match`，没有 trait object，解析路径上零虚调用。
//! - **同 key 去重**：并发解析同一条链接时只放一个真正出去（single-flight），
//!   其余等它的结果——热门内容的并发不会穿透缓存把上游打一遍。
//! - **负缓存**：`deleted` / `unsupported` 这类稳定失败也缓存 60 秒，死链的
//!   突发转发不再每个都真打一次平台；暂时性失败（风控 / 断网 / 超时）不缓存。
//! - **逐跳 SSRF**：短链跳转的每一跳都过一遍内网地址检查，拦在 DNS 层，没有
//!   检查与连接之间的时间差。
//! - **结构化错误**：失败都带 [`Reason`]，前端能直接说人话。

pub mod cache;
pub mod error;
pub mod http;
mod inflight;
pub mod model;
pub mod parsers;
pub mod registry;
pub mod util;

use std::sync::Arc;

pub use error::{Error, Reason, Result};
pub use http::resilience::{Decision, Governor, Limits};
pub use http::Config;

/// 全局闸门。进程内共享——熔断状态按平台算，每个 [`Client`] 各留一份就失去意义了。
static GOVERNOR: Governor = Governor::new(Limits::const_default());

/// 全局结果缓存。同样进程内共享：分散到每个 [`Client`] 就基本命中不了。
///
/// TTL 和容量在第一次用到时从环境变量读（`ALCEDO_CACHE_TTL` 秒、
/// `ALCEDO_CACHE_CAPACITY` 条），`ALCEDO_CACHE_TTL=0` 关闭。
static CACHE: std::sync::LazyLock<cache::Cache> = std::sync::LazyLock::new(|| {
    let secs = std::env::var("ALCEDO_CACHE_TTL")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(300);
    let cap = std::env::var("ALCEDO_CACHE_CAPACITY")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(512);
    cache::Cache::new(std::time::Duration::from_secs(secs), cap)
});

/// 结果缓存的句柄，给上层做 `/health` 或手动清理用。
pub fn result_cache() -> &'static cache::Cache {
    &CACHE
}

/// 同 key 去重表。和缓存一样进程内共享：分散到每个 [`Client`] 就去不了重。
static FLIGHT: std::sync::LazyLock<inflight::Flight<Result<VideoInfo>>> =
    std::sync::LazyLock::new(inflight::Flight::new);
pub use model::{Author, Format, Image, Source, VideoInfo};

/// 解析入口。构造一次，全程复用——它持有连接池。
#[derive(Debug, Clone)]
pub struct Client {
    cfg: Arc<Config>,
}

impl Client {
    /// 用环境变量里的配置建一个。
    pub fn new() -> Result<Self> {
        Ok(Self {
            cfg: Arc::new(Config::from_env()),
        })
    }

    /// 用指定配置建一个，绕过环境变量。
    pub fn with_config(cfg: Config) -> Self {
        Self { cfg: Arc::new(cfg) }
    }

    /// 为指定平台预热连接：提前把 DNS、TCP、TLS 握手做完，顺带领好需要的匿名身份。
    ///
    /// 冷启动是实测里最大的一块固定开销——同一条链接首次解析比后续慢
    /// 160~390 ms，全花在握手上（抖音还要多领一次游客身份）。服务端场景下
    /// 这笔账由**每个平台的第一个用户**买单，而且连接池空闲超时之后还会再来一次。
    ///
    /// 启动时调一次，或者在流量低谷定期调，就能把它挪到用户请求之外：
    ///
    /// ```no_run
    /// # async fn demo() -> alcedo::Result<()> {
    /// let client = alcedo::Client::new()?;
    /// client.prewarm(&[alcedo::Source::DouYin, alcedo::Source::BiliBili]).await;
    /// # Ok(()) }
    /// ```
    ///
    /// 全程尽力而为：预热失败不返回错误，也不影响后续解析。传入的平台如果
    /// 第一跳取决于具体链接（快手、绿洲这些），会被跳过。
    pub async fn prewarm(&self, sources: &[Source]) {
        let tasks = sources.iter().filter_map(|&source| {
            let host = registry::warmup_host(source)?;
            let cfg = Arc::clone(&self.cfg);
            Some(async move {
                let Ok(http) = http::Http::new(cfg, Some(source)) else {
                    return;
                };
                // HEAD 打根路径：只为把连接建起来，不关心响应内容
                let warm = http
                    .send(http::Req::head(format!("https://{host}/")))
                    .await
                    .is_ok();
                // 字节系的匿名身份也提前领好，省掉首次解析那一次额外往返
                if matches!(source, Source::DouYin | Source::XiGua)
                    && http.config().douyin_cookie.is_none()
                {
                    let _ = http::identity::bytedance_ttwid(&http).await;
                }
                if source == Source::BiliBili {
                    let _ = parsers::bilibili::ensure_buvid(&http).await;
                }
                tracing::debug!(source = source.as_str(), ok = warm, "预热完成");
            })
        });
        futures_util::future::join_all(tasks).await;
    }

    /// 给国内中继的连接保温（没配中继时返回 `false`，什么都不做）。
    ///
    /// 探针是「带令牌、不带 `url`」的空请求，中继函数直接 400 回来，不对
    /// 任何平台产生出站流量。跨洋链路冷握手实测 1.3s、热连接 0.18s，而边缘
    /// 节点侧的空闲超时只有一两分钟，所以常驻服务要每一两分钟调一次——
    /// `alcedo serve` 已经自动这么做了，自己托管服务的话照这个频率来。
    ///
    /// ```no_run
    /// # async fn demo() -> alcedo::Result<()> {
    /// let client = alcedo::Client::new()?;
    /// if client.ping_relay().await {
    ///     println!("中继探针已发出");
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn ping_relay(&self) -> bool {
        // 任意国内平台都行：它只决定用哪个连接池，而走中继时本地唯一要焐的
        // 连接就是到边缘节点那条（平台那一跳发生在边缘函数里）
        let Ok(http) = http::Http::new(Arc::clone(&self.cfg), Some(Source::DouYin)) else {
            return false;
        };
        http.ping_relay().await
    }

    /// 当前生效的配置。
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 解析一条分享链接。
    ///
    /// `input` 可以是整段分享文案，会自动把里面的链接抠出来。
    pub async fn parse(&self, input: &str) -> Result<VideoInfo> {
        let url = util::extract_url(input)
            .map(str::to_owned)
            .or_else(|| {
                let t = input.trim();
                t.starts_with("http").then(|| t.to_owned())
            })
            .ok_or_else(|| Error::unsupported("没有在输入里找到链接"))?;

        let source = registry::detect(&url).ok_or_else(|| {
            Error::unsupported(format!(
                "还不支持 {} 这个站点",
                util::host_of(&url).unwrap_or_else(|| "该".into())
            ))
        })?;

        // 缓存键用规范化后的地址：同一条内容的不同写法应当命中同一条
        let key = cache_key(source, &url);
        self.execute(source, key, Target::Url(&url)).await
    }

    /// 已经知道平台和作品 ID 时直接解析。
    pub async fn parse_id(&self, source: Source, id: &str) -> Result<VideoInfo> {
        if id.trim().is_empty() {
            return Err(Error::unsupported("作品 ID 为空"));
        }
        let key = cache_key(source, id);
        self.execute(source, key, Target::Id(id)).await
    }

    /// 缓存查询 + 同 key 去重 + 真正执行。`parse` / `parse_id` 共用这条管线。
    ///
    /// 等待与重新排队的循环在 [`inflight::Flight::claim`] 内部：它要么带来
    /// 领队的现成结果（成功或失败），要么把执行权交出来，不会空转回来。
    async fn execute(&self, source: Source, key: String, target: Target<'_>) -> Result<VideoInfo> {
        if let Some(cached) = CACHE.get(&key) {
            return cached;
        }
        match FLIGHT.claim(&key).await {
            inflight::Claim::Wait(done) => done,
            inflight::Claim::Lead(guard) => {
                // 只有领队真正出门打请求：等待者不占限流配额，也不过闸门
                let done = self.run(source, &key, target).await;
                guard.finish(done.clone());
                done
            }
        }
    }

    /// 领队的实际解析：过闸门、发请求、收尾、进缓存。
    async fn run(&self, source: Source, key: &str, target: Target<'_>) -> Result<VideoInfo> {
        self.pass_gate(source).await?;
        let http = http::Http::new(Arc::clone(&self.cfg), Some(source))?;
        let outcome = match target {
            Target::Url(u) => self.with_timeout(parsers::dispatch(&http, source, u)).await,
            Target::Id(i) => {
                self.with_timeout(parsers::dispatch_id(&http, source, i))
                    .await
            }
        };
        let done = match self.settle(source, target.as_str(), outcome) {
            Ok(info) => info,
            Err(e) => {
                // 稳定事实类的失败进负缓存：死链的重复解析不再真打平台
                CACHE.put_error(key, &e);
                return Err(e);
            }
        };
        CACHE.put(key, &done);
        Ok(done)
    }

    async fn with_timeout(
        &self,
        fut: impl std::future::Future<Output = Result<VideoInfo>>,
    ) -> Result<VideoInfo> {
        tokio::time::timeout(self.cfg.total_timeout, fut)
            .await
            .unwrap_or_else(|_| Err(Error::new(Reason::Timeout, "整体解析超时")))
    }

    /// 把结果反馈给闸门，再做收尾。
    fn settle(
        &self,
        source: Source,
        target: &str,
        outcome: Result<VideoInfo>,
    ) -> Result<VideoInfo> {
        match outcome {
            Ok(mut info) => {
                GOVERNOR.on_success(source);
                self.finish(&mut info, source, target)?;
                Ok(info)
            }
            Err(e) => {
                GOVERNOR.on_failure(source, e.reason);
                Err(e)
            }
        }
    }

    /// 限流 / 熔断闸门。令牌差一点就等一下，差太多或熔断中就直接说清楚。
    async fn pass_gate(&self, source: Source) -> Result<()> {
        match GOVERNOR.check(source) {
            Decision::Go => Ok(()),
            Decision::Wait(d) => {
                tokio::time::sleep(d).await;
                Ok(())
            }
            Decision::Tripped(d) => Err(Error::new(
                Reason::Blocked,
                format!(
                    "{} 刚刚连续失败，已暂停请求 {} 秒（避免把风控撞得更死）",
                    source.display_name(),
                    d.as_secs().max(1)
                ),
            )),
        }
    }

    /// 收尾：补默认字段、排好清晰度、确认真的拿到了东西。
    fn finish(&self, info: &mut VideoInfo, source: Source, url: &str) -> Result<()> {
        info.source.get_or_insert(source);
        if info.page_url.is_empty() {
            info.page_url = url.to_owned();
        }
        info.normalize_formats();
        // 直链常常要带 Referer 才不 403，解析器没填的话按 CDN 域名补一个
        if !info.video_url.is_empty() && !info.video_headers.contains_key("Referer") {
            if let Some(r) = parsers::referer_for(&info.video_url) {
                info.set_header("Referer", r);
            }
        }
        if info.is_empty() {
            return Err(Error::empty());
        }
        Ok(())
    }
}

/// 这次解析的目标。`parse` 给完整链接，`parse_id` 给已知平台的作品 ID。
#[derive(Debug, Clone, Copy)]
enum Target<'a> {
    Url(&'a str),
    Id(&'a str),
}

impl<'a> Target<'a> {
    /// 收尾时补 `page_url` 用的原始地址。
    const fn as_str(self) -> &'a str {
        match self {
            Target::Url(s) | Target::Id(s) => s,
        }
    }
}

/// 各平台装着作品 ID 的 query 参数。剥跟踪参数时它们**不能**剥：
/// 剥了的话 `watch?v=A` 和 `watch?v=B` 折叠成同一个键，缓存会把 A 的结果
/// 错发给 B。不在表里的参数一律当跟踪参数剥掉。
static ID_QUERY_PARAMS: &[(Source, &str)] = &[
    (Source::YouTube, "v"),
    (Source::LvZhou, "sid"),
    (Source::WeiBo, "fid"),
    (Source::QQVideo, "vid"),
    (Source::QuanMin, "vid"),
    (Source::HaoKan, "vid"),
    (Source::SixRoom, "vid"),
    (Source::WeiShi, "id"),
    (Source::DouPai, "id"),
    (Source::ZuiYou, "pid"),
    (Source::QuanMinKGe, "s"),
];

/// 这个平台的 query 里有没有装作品 ID 的这个参数。
fn is_id_param(source: Source, key: &str) -> bool {
    ID_QUERY_PARAMS
        .iter()
        .any(|(s, k)| *s == source && *k == key)
}

/// 缓存键。带上平台是因为不同平台的 ID 命名空间是独立的。
fn cache_key(source: Source, target: &str) -> String {
    // 去掉 query 里的跟踪参数，让同一条内容的不同分享写法命中同一条缓存；
    // 装 ID 的参数要保留（见 ID_QUERY_PARAMS）
    let cleaned = url::Url::parse(target).map_or_else(
        |_| target.to_owned(),
        |mut u| {
            let kept: Vec<(String, String)> = u
                .query_pairs()
                .filter(|(k, _)| is_id_param(source, k.as_ref()))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            u.set_query(None);
            u.set_fragment(None);
            for (k, v) in kept {
                u.query_pairs_mut().append_pair(&k, &v);
            }
            u.to_string()
        },
    );
    format!("{}|{cleaned}", source.as_str())
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_keeps_id_params_and_strips_tracking() {
        // 同一视频的不同写法要命中同一条缓存
        let a = cache_key(
            Source::YouTube,
            "https://www.youtube.com/watch?v=abc&feature=share",
        );
        let b = cache_key(Source::YouTube, "https://www.youtube.com/watch?v=abc&t=30");
        assert_eq!(a, b, "同一条视频不该因为跟踪参数而分裂");
        // 不同视频绝不能折叠成同一个键——剥了 v 就会这样
        let c = cache_key(Source::YouTube, "https://www.youtube.com/watch?v=def");
        assert_ne!(a, c, "不同视频共享缓存键会把 A 的结果错发给 B");

        // 纯跟踪参数照旧剥掉
        let d = cache_key(
            Source::DouYin,
            "https://www.douyin.com/video/123?previous_page=app&share=1",
        );
        let e = cache_key(Source::DouYin, "https://www.douyin.com/video/123");
        assert_eq!(d, e);
    }

    #[tokio::test]
    async fn rejects_unknown_site_without_network() {
        let c = Client::with_config(Config::default());
        let err = c.parse("https://example.com/watch/1").await.unwrap_err();
        assert_eq!(err.reason, Reason::Unsupported);
    }

    #[tokio::test]
    async fn rejects_input_without_url() {
        let c = Client::with_config(Config::default());
        let err = c.parse("今天天气不错").await.unwrap_err();
        assert_eq!(err.reason, Reason::Unsupported);
    }

    #[tokio::test]
    async fn empty_id_is_rejected_early() {
        let c = Client::with_config(Config::default());
        let err = c.parse_id(Source::DouYin, "  ").await.unwrap_err();
        assert_eq!(err.reason, Reason::Unsupported);
    }

    #[tokio::test]
    async fn ping_relay_without_relay_is_a_noop() {
        let c = Client::with_config(Config::default());
        assert!(!c.ping_relay().await, "没配中继时不该发任何请求");
    }
}
