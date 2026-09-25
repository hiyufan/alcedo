//! 国内出口中转。
//!
//! 境外服务器访问 B 站、小红书这些平台常被拦（B 站直接 412）。标准做法是配一个
//! 国内的 HTTP / SOCKS 代理（`ALCEDO_PROXY_CN`），但很多人手上没有常驻的国内
//! 机器。中转是另一条路：在国内的边缘函数平台（阿里云 ESA 之类）部署一个很小的
//! 转发函数，请求由它代为发出，出口就是国内 IP。
//!
//! 协议（和函数那边约定好的，改一边必须同步改另一边）：
//!
//! - 请求：`<中转地址>?url=<目标地址>`，请求头
//!   - `x-relay-token`：鉴权令牌
//!   - `x-relay-method`：真正要用的方法
//!   - `x-relay-headers`：真正要带的请求头，JSON 对象再 base64
//!   - 非 GET / HEAD 的请求体原样作为中转请求的请求体
//! - 响应：中转自身总是 200，真实结果装在
//!   - `x-relay-status`：目标的状态码；`599` 表示中转连不上目标，原因在 `x-relay-error`
//!   - `x-relay-headers`：目标的响应头，`[[名, 值], ...]` 的 JSON 再 base64
//!   - 响应体：目标的响应体，已解压
//!
//! 中转不跟重定向，3xx 原样交回来，由 [`super::Http`] 自己跟——每一跳照样过 SSRF 检查。
//! 只有解析请求（网页 / 接口，几十 KB）走中转，视频本体不经过这里。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode};

use crate::error::{Error, Reason, Result};

/// 一个中转端点。
#[derive(Debug, Clone)]
pub struct Relay {
    endpoint: url::Url,
    token: Option<String>,
}

impl Relay {
    /// 中转地址无效时返回错误，别等到第一次请求才发现。
    pub fn new(endpoint: &str, token: Option<&str>) -> Result<Self> {
        let endpoint = url::Url::parse(endpoint)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .ok_or_else(|| Error::new(Reason::Network, format!("中转地址无效: {endpoint}")))?;
        Ok(Self {
            endpoint,
            token: token.map(str::to_owned),
        })
    }

    /// 把一次对 `target` 的请求包装成对中转的请求。
    pub fn wrap(
        &self,
        client: &Client,
        method: &Method,
        target: &url::Url,
        headers: &HeaderMap,
        body: Option<&[u8]>,
    ) -> RequestBuilder {
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("url", target.as_str());

        // GET / HEAD 在函数那边不读请求体，其余方法一律用 POST 把请求体带过去
        let carrier = if matches!(*method, Method::GET | Method::HEAD) {
            Method::GET
        } else {
            Method::POST
        };

        let forwarded: serde_json::Map<String, serde_json::Value> = headers
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.into())))
            .collect();
        let encoded = B64.encode(serde_json::Value::Object(forwarded).to_string());

        let mut builder = client
            .request(carrier, url)
            .header("x-relay-method", method.as_str())
            .header("x-relay-headers", encoded);
        if let Some(t) = &self.token {
            builder = builder.header("x-relay-token", t);
        }
        if let Some(b) = body {
            builder = builder.body(b.to_vec());
        }
        builder
    }
}

/// 从中转的响应里拆出目标的状态码和响应头。响应体原样留在 `resp` 里，照常读。
pub async fn unwrap(resp: reqwest::Response) -> Result<(StatusCode, HeaderMap, reqwest::Response)> {
    let outer = resp.status();
    let status = resp
        .headers()
        .get("x-relay-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u16>().ok());

    let Some(status) = status else {
        // 没有 x-relay-status 说明是中转自己拒绝的（令牌不对、地址不对……）
        let hint = if outer == StatusCode::FORBIDDEN {
            "，检查 ALCEDO_RELAY_TOKEN 是否和中转函数里的 TOKEN 一致"
        } else {
            ""
        };
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(120).collect();
        return Err(Error::new(
            Reason::Network,
            format!("中转拒绝了请求 (HTTP {}){hint}: {body}", outer.as_u16()),
        ));
    };

    if status == 599 {
        let why = resp
            .headers()
            .get("x-relay-error")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("未知原因")
            .to_owned();
        return Err(Error::new(
            Reason::Network,
            format!("中转连不上目标: {why}"),
        ));
    }

    let status = StatusCode::from_u16(status)
        .map_err(|_| Error::new(Reason::Network, format!("中转返回了无效状态码 {status}")))?;
    let headers = resp
        .headers()
        .get("x-relay-headers")
        .and_then(|v| decode_headers(v.as_bytes()))
        .unwrap_or_default();
    Ok((status, headers, resp))
}

/// `[[名, 值], ...]` 的 JSON 再 base64 → `HeaderMap`。同名头（`set-cookie`）逐条追加。
fn decode_headers(raw: &[u8]) -> Option<HeaderMap> {
    let json = B64.decode(raw).ok()?;
    let pairs: Vec<(String, String)> = serde_json::from_slice(&json).ok()?;
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        if let (Ok(name), Ok(val)) = (
            k.parse::<HeaderName>(),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            map.append(name, val);
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        super::super::ensure_crypto_provider();
        Client::new()
    }

    #[test]
    fn wraps_target_into_relay_request() {
        let relay = Relay::new("https://relay.example/relay", Some("tok")).unwrap();
        let target = url::Url::parse("https://api.bilibili.com/x/view?bvid=BV1&a=b").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "referer",
            HeaderValue::from_static("https://www.bilibili.com/"),
        );

        let req = relay
            .wrap(&client(), &Method::GET, &target, &headers, None)
            .build()
            .unwrap();

        assert_eq!(req.method(), Method::GET);
        // 目标地址整条进 url 参数，自身的 & 不能漏出来变成中转的参数
        let got: Vec<_> = req.url().query_pairs().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, target.as_str());
        assert_eq!(req.headers()["x-relay-token"], "tok");
        assert_eq!(req.headers()["x-relay-method"], "GET");

        let json = B64
            .decode(req.headers()["x-relay-headers"].as_bytes())
            .unwrap();
        let fwd: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(fwd["referer"], "https://www.bilibili.com/");
    }

    #[test]
    fn post_body_rides_on_a_post() {
        let relay = Relay::new("https://relay.example/relay", None).unwrap();
        let target = url::Url::parse("https://example.com/api").unwrap();
        let req = relay
            .wrap(
                &client(),
                &Method::POST,
                &target,
                &HeaderMap::new(),
                Some(b"a=1"),
            )
            .build()
            .unwrap();
        assert_eq!(req.method(), Method::POST);
        assert_eq!(req.headers()["x-relay-method"], "POST");
        assert!(!req.headers().contains_key("x-relay-token"));
        assert_eq!(req.body().and_then(|b| b.as_bytes()), Some(&b"a=1"[..]));
    }

    #[test]
    fn decodes_repeated_response_headers() {
        let raw = B64.encode(
            r#"[["content-type","text/html; charset=utf-8"],["set-cookie","a=1"],["set-cookie","b=2"],["location","https://x/中"]]"#,
        );
        let map = decode_headers(raw.as_bytes()).unwrap();
        assert_eq!(map["content-type"], "text/html; charset=utf-8");
        assert_eq!(map.get_all("set-cookie").iter().count(), 2);
        assert!(map.contains_key("location"), "非 ASCII 的值也要保留");
    }

    #[test]
    fn rejects_bad_endpoint() {
        assert!(Relay::new("ftp://x/relay", None).is_err());
        assert!(Relay::new("not a url", None).is_err());
    }

    fn fake(status: u16, headers: &[(&str, String)]) -> reqwest::Response {
        let mut b = http::Response::builder().status(status);
        for (k, v) in headers {
            b = b.header(*k, v);
        }
        reqwest::Response::from(b.body("body").unwrap())
    }

    #[tokio::test]
    async fn unwraps_status_and_headers() {
        let hdrs = B64.encode(r#"[["location","https://m.bilibili.com/"]]"#);
        let resp = fake(
            200,
            &[("x-relay-status", "302".into()), ("x-relay-headers", hdrs)],
        );
        let (status, headers, _) = unwrap(resp).await.unwrap();
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(headers["location"], "https://m.bilibili.com/");
    }

    #[tokio::test]
    async fn relay_level_rejection_is_explained() {
        let e = unwrap(fake(403, &[])).await.unwrap_err();
        assert!(e.detail.contains("ALCEDO_RELAY_TOKEN"), "{e}");

        let e = unwrap(fake(
            200,
            &[
                ("x-relay-status", "599".into()),
                ("x-relay-error", "timeout".into()),
            ],
        ))
        .await
        .unwrap_err();
        assert!(e.detail.contains("timeout"), "{e}");
    }
}
