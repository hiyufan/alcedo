//! 游客身份：向平台自己要一份匿名 cookie。
//!
//! 字节系（抖音 / 西瓜）的接口对"完全没有 cookie 的请求"越来越不客气。它们
//! 同时又提供了一个**免签名**的注册端点，浏览器首次访问时就是走它拿 `ttwid`。
//! 我们照做即可——这不是绕过什么，就是按客户端该有的样子去请求。
//!
//! 刻意做得很简单：只缓存一份匿名 ttwid，不搞身份池、不用无头浏览器。理由是
//! 维护成本——无头浏览器意味着要跟着 Chrome 版本走，而 ttwid 这个端点多年没变。
//!
//! **明确不做的事**：a_bogus 这类签名算法不在这里。抖音大约每季度轮换一次，
//! 硬编进来等于给自己排了一个每三个月的逆向任务。那条路交给
//! [`crate::parsers::signer`] 的外部签名器。

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::http::{ua, Http, Req};

/// 一份进程内共享的匿名身份，过了新鲜期还有一段宽限期。
///
/// 宽限期内照常返回旧值，同时在后台换一份新的。低流量服务上缓存几乎每次都是
/// 过期状态，不这么做的话，每隔一个 TTL 就有一个用户要替大家排队领身份（实测
/// ttwid 一次 ~150ms，而整次抖音解析热连接下才 ~200ms）。
///
/// 旧身份被平台拒掉时调用方会 [`IdentitySlot::forget`]，下次就同步重领，
/// 所以宽限期不会让一份坏掉的身份一直用下去。
#[derive(Debug)]
pub(crate) struct IdentitySlot {
    value: RwLock<Option<(String, Instant)>>,
    refreshing: AtomicBool,
    ttl: Duration,
}

impl IdentitySlot {
    pub(crate) const fn new(ttl: Duration) -> Self {
        Self {
            value: RwLock::new(None),
            refreshing: AtomicBool::new(false),
            ttl,
        }
    }

    /// 取身份：新鲜的直接用；宽限期内用旧的并在后台刷新；再老或没有就同步领一份。
    pub(crate) async fn get_or_fetch<F, Fut>(&'static self, http: &Http, fetch: F) -> Option<String>
    where
        F: Fn(Http) -> Fut,
        Fut: Future<Output = Option<String>> + Send + 'static,
    {
        let cached = self.value.read().clone();
        if let Some((v, at)) = cached {
            let age = at.elapsed();
            if age < self.ttl {
                return Some(v);
            }
            if age < self.ttl * 2 {
                // 同一时刻只放一个刷新出去，别让一波并发请求各领一份
                if !self.refreshing.swap(true, Ordering::AcqRel) {
                    let job = fetch(http.clone());
                    tokio::spawn(async move {
                        if let Some(fresh) = job.await {
                            self.put(fresh);
                        }
                        self.refreshing.store(false, Ordering::Release);
                    });
                }
                return Some(v);
            }
        }
        let fresh = fetch(http.clone()).await?;
        self.put(fresh.clone());
        Some(fresh)
    }

    pub(crate) fn put(&self, v: String) {
        *self.value.write() = Some((v, Instant::now()));
    }

    /// 清掉缓存的身份。被平台挡了之后调一次，下次会同步重领。
    pub(crate) fn forget(&self) {
        *self.value.write() = None;
    }
}

/// ttwid 的有效期远不止一小时，但缓存久了万一被平台拉黑就一直用着坏的。
static TTWID: IdentitySlot = IdentitySlot::new(Duration::from_secs(1800));

/// 字节系的匿名 ttwid。拿不到就返回 `None`——裸请求照样可能成功，
/// 不该因为身份端点抽风就让整条解析失败。
pub async fn bytedance_ttwid(http: &Http) -> Option<String> {
    TTWID
        .get_or_fetch(http, |http| async move { register_ttwid(&http).await })
        .await
}

async fn register_ttwid(http: &Http) -> Option<String> {
    // 这个 body 就是网页端首次访问时发的那一份
    let body = serde_json::json!({
        "region": "cn",
        "aid": 1768,
        "needFid": false,
        "service": "www.ixigua.com",
        "migrate_info": { "ticket": "", "source": "node" },
        "cbUrlProtocol": "https",
        "union": true,
    });

    let resp = http
        .send(
            Req::post("https://ttwid.bytedance.com/ttwid/union/register/")
                .header("User-Agent", ua::DESKTOP_FIXED)
                .json_body(&body),
        )
        .await
        .ok()?;

    let ttwid = resp
        .set_cookies()
        .into_iter()
        .find(|(k, _)| k == "ttwid")
        .map(|(_, v)| v)?;

    if ttwid.is_empty() {
        return None;
    }
    tracing::debug!("领到新的 ttwid");
    Some(ttwid)
}

/// 清掉缓存的身份。被平台挡了之后调一次，下次会重新领。
pub fn forget_bytedance() {
    TTWID.forget();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> Http {
        Http::new(std::sync::Arc::new(crate::Config::default()), None).unwrap()
    }

    #[tokio::test]
    async fn fresh_value_is_reused_without_fetching() {
        static SLOT: IdentitySlot = IdentitySlot::new(Duration::from_secs(60));
        SLOT.put("abc".into());
        let got = SLOT
            .get_or_fetch(&http(), |_| async { panic!("新鲜的身份不该重领") })
            .await;
        assert_eq!(got.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn stale_value_is_served_while_refreshing_in_background() {
        static SLOT: IdentitySlot = IdentitySlot::new(Duration::from_secs(60));
        *SLOT.value.write() = Some(("old".into(), Instant::now() - Duration::from_secs(90)));
        let got = SLOT
            .get_or_fetch(&http(), |_| async { Some("new".to_owned()) })
            .await;
        assert_eq!(got.as_deref(), Some("old"), "宽限期内不该让用户等重领");

        // 后台任务跑完之后换成新的
        for _ in 0..50 {
            if SLOT.value.read().as_ref().is_some_and(|(v, _)| v == "new") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("后台刷新没有生效");
    }

    #[tokio::test]
    async fn expired_value_is_fetched_synchronously() {
        static SLOT: IdentitySlot = IdentitySlot::new(Duration::from_secs(60));
        *SLOT.value.write() = Some(("old".into(), Instant::now() - Duration::from_secs(200)));
        let got = SLOT
            .get_or_fetch(&http(), |_| async { Some("new".to_owned()) })
            .await;
        assert_eq!(got.as_deref(), Some("new"), "超过宽限期的身份不该再用");
    }

    #[tokio::test]
    async fn forget_forces_a_synchronous_refetch() {
        static SLOT: IdentitySlot = IdentitySlot::new(Duration::from_secs(60));
        SLOT.put("abc".into());
        SLOT.forget();
        let got = SLOT
            .get_or_fetch(&http(), |_| async { Some("new".to_owned()) })
            .await;
        assert_eq!(got.as_deref(), Some("new"), "被风控后必须能强制换一份身份");
    }
}
