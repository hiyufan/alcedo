//! SSRF 防护。
//!
//! 解析器会跟着用户给的短链一路跳转，每一跳都是"用户控制的地址"。不拦的话，
//! `https://nip.io` 这类服务能把请求引到 `127.0.0.1` 或云厂商的元数据地址
//! (169.254.169.254)，等于给了任何人一个内网探针。
//!
//! 拦截点放在 DNS 解析这一层而不是"请求前查一遍"：
//!
//! - 只解析一次，省掉一次 getaddrinfo（实测 13–200ms）
//! - 没有 TOCTOU——检查和实际连接用的是同一组 IP，中间没有窗口让 DNS 记录换掉
//! - 重定向、连接池复用、代理直连全都自动覆盖到

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Clash / Surge 一类代理的 fake-ip 段：域名一律被解析到这里，
/// 不代表真的是内网，照拦会让所有开了代理的开发机没法用。
const FAKE_IP_V4: (Ipv4Addr, u8) = (Ipv4Addr::new(198, 18, 0, 0), 15);

fn in_fake_ip(ip: Ipv4Addr) -> bool {
    let (net, prefix) = FAKE_IP_V4;
    let mask = u32::MAX << (32 - prefix);
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

/// 这个地址是否属于"不该让外部链接指过去"的范围。
///
/// 标准库里 `is_reserved` / `is_shared` / `is_unique_local` 还没稳定，
/// 这里按 RFC 自己判，免得为了几个判断去 nightly 或者拉 `ipnet`。
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if in_fake_ip(v4) {
                return false;
            }
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()      // 169.254/16，含云元数据地址
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || o[0] == 0               // 0.0.0.0/8
                || o[0] == 100 && (64..128).contains(&o[1]) // 100.64/10 CGNAT
                || o[0] >= 224 // 组播 + 240/4 保留
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
                || (seg[0] & 0xffc0) == 0xfec0 // fec0::/10 site local (废弃但仍需拦)
                || seg[0] == 0x2001 && seg[1] == 0x0db8 // 文档用
                || v6 == Ipv6Addr::UNSPECIFIED
        }
    }
}

/// 明显指向本机 / 内网的主机名，连解析都不用做。
// 这里比的是**主机名后缀**不是文件扩展名，而且 host 进来第一步就统一小写了，
// clippy 的扩展名大小写规则在这儿不适用
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn is_internal_hostname(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
        || host.ends_with(".arpa")
}

/// 这个 URL 能不能请求。只看协议和主机名，DNS 那层由 [`SafeResolver`] 兜底。
pub fn url_allowed(url: &url::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host() else { return false };
    match host {
        url::Host::Domain(d) => !is_internal_hostname(d),
        url::Host::Ipv4(ip) => !is_internal(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => !is_internal(IpAddr::V6(ip)),
    }
}

// ------------------------------------------------------------------ resolver

const CACHE_TTL: Duration = Duration::from_secs(300);
const CACHE_MAX: usize = 2048;

type CacheEntry = (Instant, Arc<Vec<SocketAddr>>);

/// 带 TTL 缓存、拒绝内网地址的 DNS 解析器。
///
/// 一次解析里同一个平台域名要查好几遍（短链跳转 + 接口 + 页面），缓存能把
/// 重复的 getaddrinfo 全省掉。
#[derive(Debug, Clone)]
pub struct SafeResolver {
    // Arc 而不是裸 Mutex: `Resolve::resolve` 拿到的是 &self, 但返回的 future 要
    // 求 'static, 只有把缓存句柄 clone 进去才能在解析完之后写回结果。
    cache: Arc<Mutex<Vec<(String, CacheEntry)>>>,
    /// 关掉之后只拦字面 IP 和本地主机名。内网自建平台镜像时可能需要。
    enforce: bool,
}

impl SafeResolver {
    pub fn new(enforce: bool) -> Self {
        Self {
            cache: Arc::new(Mutex::new(Vec::new())),
            enforce,
        }
    }

    fn cached(&self, host: &str) -> Option<Arc<Vec<SocketAddr>>> {
        let cache = self.cache.lock();
        cache
            .iter()
            .find(|(h, _)| h == host)
            .and_then(|(_, (at, addrs))| (at.elapsed() < CACHE_TTL).then(|| Arc::clone(addrs)))
    }
}

fn store(cache: &Mutex<Vec<(String, CacheEntry)>>, host: String, addrs: Arc<Vec<SocketAddr>>) {
    let mut cache = cache.lock();
    if let Some(slot) = cache.iter_mut().find(|(h, _)| *h == host) {
        slot.1 = (Instant::now(), addrs);
        return;
    }
    if cache.len() >= CACHE_MAX {
        // 满了就整体丢掉。域名基数很小（几十个平台），到不了这个量级；
        // 真到了说明有人在刷随机域名，清空比逐条 LRU 便宜也更抗滥用。
        cache.clear();
    }
    cache.push((host, (Instant::now(), addrs)));
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let enforce = self.enforce;

        if is_internal_hostname(&host) {
            return Box::pin(async move {
                Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                    "拒绝解析内网主机名: {host}"
                )))
            });
        }

        if let Some(hit) = self.cached(&host) {
            return Box::pin(async move {
                let addrs: Addrs = Box::new(hit.iter().copied().collect::<Vec<_>>().into_iter());
                Ok(addrs)
            });
        }

        let cache = Arc::clone(&self.cache);

        Box::pin(async move {
            // lookup_host 要借住 host 跨过 await，而 host 之后还要交给缓存，
            // 借一份给它用完就还
            let lookup = tokio::net::lookup_host((host.clone(), 0)).await;
            let addrs: Vec<SocketAddr> = match lookup {
                Ok(it) => it.collect(),
                Err(e) => {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                        "DNS 解析失败 {host}: {e}"
                    )))
                }
            };
            if addrs.is_empty() {
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                    "DNS 没有返回任何地址: {host}"
                )));
            }
            if enforce {
                // 必须查全部记录：只看 A 会漏掉 AAAA 指向内网的情况，那就是个洞。
                if let Some(bad) = addrs.iter().find(|a| is_internal(a.ip())) {
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                        "{host} 解析到内网地址 {}，已拒绝",
                        bad.ip()
                    )));
                }
            }
            let shared = Arc::new(addrs);
            store(&cache, host, Arc::clone(&shared));
            let addrs: Addrs = Box::new(shared.as_ref().clone().into_iter());
            Ok(addrs)
        })
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_metadata() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.0.1",
            "169.254.169.254", // 云元数据
            "0.0.0.0",
            "100.64.0.1", // CGNAT
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_internal(ip), "{s} 应当被拦下");
        }
    }

    #[test]
    fn allows_public_and_fakeip() {
        for s in ["1.1.1.1", "8.8.8.8", "198.18.0.5", "2606:4700::1111"] {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!is_internal(ip), "{s} 不该被拦");
        }
    }

    #[test]
    fn ipv4_mapped_v6_is_checked() {
        let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_internal(ip), "IPv4-mapped 的回环地址必须拦下");
    }

    #[test]
    fn hostname_rules() {
        assert!(is_internal_hostname("localhost"));
        assert!(is_internal_hostname("Foo.LOCAL"));
        assert!(is_internal_hostname("db.internal"));
        assert!(!is_internal_hostname("www.douyin.com"));
    }

    #[test]
    fn url_scheme_and_host() {
        let ok = url::Url::parse("https://www.douyin.com/video/1").unwrap();
        assert!(url_allowed(&ok));
        let bad_scheme = url::Url::parse("file:///etc/passwd").unwrap();
        assert!(!url_allowed(&bad_scheme));
        let bad_host = url::Url::parse("http://127.0.0.1:8080/x").unwrap();
        assert!(!url_allowed(&bad_host));
    }
}
