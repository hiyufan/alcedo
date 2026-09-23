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
//! - **逐跳 SSRF**：短链跳转的每一跳都过一遍内网地址检查，拦在 DNS 层，没有
//!   检查与连接之间的时间差。
//! - **结构化错误**：失败都带 [`Reason`]，前端能直接说人话。

pub mod error;
pub mod http;
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
                tracing::debug!(source = source.as_str(), ok = warm, "预热完成");
            })
        });
        futures_util::future::join_all(tasks).await;
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

        self.pass_gate(source).await?;
        let http = http::Http::new(Arc::clone(&self.cfg), Some(source))?;
        let outcome = self
            .with_timeout(parsers::dispatch(&http, source, &url))
            .await;
        self.settle(source, &url, outcome)
    }

    /// 已经知道平台和作品 ID 时直接解析。
    pub async fn parse_id(&self, source: Source, id: &str) -> Result<VideoInfo> {
        if id.trim().is_empty() {
            return Err(Error::unsupported("作品 ID 为空"));
        }
        self.pass_gate(source).await?;
        let http = http::Http::new(Arc::clone(&self.cfg), Some(source))?;
        let outcome = self
            .with_timeout(parsers::dispatch_id(&http, source, id))
            .await;
        self.settle(source, id, outcome)
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

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

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
}
