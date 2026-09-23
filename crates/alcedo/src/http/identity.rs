//! 游客身份：向平台自己要一份匿名 cookie。
//!
//! 字节系（抖音 / 西瓜）的接口对"完全没有 cookie 的请求"越来越不客气。它们
//! 同时又提供了一个**免签名**的注册端点，浏览器首次访问时就是走它拿 `ttwid`。
//! 我们照做即可——这不是绕过什么，就是按客户端该有的样子去请求。
//!
//! 思路来自 `Douyin_TikTok_Download_API` 的身份池，但刻意做得简单得多：
//! 它用无头浏览器铸造身份并维护健康分级，我们只缓存一份匿名 ttwid。理由是
//! 维护成本——无头浏览器意味着要跟着 Chrome 版本走，而 ttwid 这个端点多年没变。
//!
//! **明确不做的事**：a_bogus 这类签名算法不在这里。抖音大约每季度轮换一次，
//! 硬编进来等于给自己排了一个每三个月的逆向任务。那条路交给
//! [`crate::parsers::signer`] 的外部签名器。

use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::http::{ua, Http, Req};

/// ttwid 的有效期远不止一小时，但缓存久了万一被平台拉黑就一直用着坏的。
const TTL: Duration = Duration::from_secs(1800);

static TTWID: RwLock<Option<(String, Instant)>> = RwLock::new(None);

/// 字节系的匿名 ttwid。拿不到就返回 `None`——裸请求照样可能成功，
/// 不该因为身份端点抽风就让整条解析失败。
pub async fn bytedance_ttwid(http: &Http) -> Option<String> {
    if let Some((v, at)) = TTWID.read().as_ref() {
        if at.elapsed() < TTL {
            return Some(v.clone());
        }
    }

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
    *TTWID.write() = Some((ttwid.clone(), Instant::now()));
    tracing::debug!("领到新的 ttwid");
    Some(ttwid)
}

/// 清掉缓存的身份。被平台挡了之后调一次，下次会重新领。
pub fn forget_bytedance() {
    *TTWID.write() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_dropped_on_demand() {
        *TTWID.write() = Some(("abc".to_owned(), Instant::now()));
        assert!(TTWID.read().is_some());
        forget_bytedance();
        assert!(TTWID.read().is_none(), "被风控后必须能强制换一份身份");
    }

    #[test]
    fn expired_entries_are_not_reused() {
        // 直接塞一个过期的时间戳，验证 TTL 判断本身
        let stale = Instant::now() - TTL - Duration::from_secs(1);
        *TTWID.write() = Some(("old".to_owned(), stale));
        let fresh = TTWID
            .read()
            .as_ref()
            .and_then(|(v, at)| (at.elapsed() < TTL).then(|| v.clone()));
        assert!(fresh.is_none(), "过期的身份不该被复用");
        forget_bytedance();
    }
}
