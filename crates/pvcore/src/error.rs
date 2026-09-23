//! 结构化的解析错误。
//!
//! 前端要的是"能说人话"：平台挂了、要登录、内容被删，用户该做的事完全不同，
//! 统一一句"解析失败"等于把排障成本全丢给站长。所以每个错误都带一个
//! [`Reason`]，文案由它决定，`detail` 只是补充。

use std::fmt;

/// 失败原因。文案面向终端用户，别写成给开发者看的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// 内容已删除 / 私密 / 链接过期
    Deleted,
    /// 平台要求登录，需要站长配 cookie
    Login,
    /// 被风控 / 限流 / 403
    Blocked,
    /// 不支持这个链接
    Unsupported,
    /// 网络连不上
    Network,
    /// 对方响应太慢
    Timeout,
    /// 解析成功但没拿到任何内容
    Empty,
    /// 平台不对外提供这条内容的数据
    Restricted,
    /// 页面结构变了，解析器需要更新
    Parse,
}

impl Reason {
    /// 给用户看的一句话。
    pub const fn message(self) -> &'static str {
        match self {
            Reason::Deleted => "这条内容已经被删除、设为私密，或者链接过期了",
            Reason::Login => "这个平台现在要求登录才能看，需要站长给服务器配置 cookies",
            Reason::Blocked => "平台暂时限制了服务器的访问，过几分钟再试，或者换个链接",
            Reason::Unsupported => "还不支持这个链接的格式",
            Reason::Network => "连不上对方平台，稍后再试",
            Reason::Timeout => "对方平台响应太慢，稍后再试",
            Reason::Empty => "解析成功但没有拿到任何视频或图片",
            Reason::Restricted => {
                "平台没有对外提供这条内容的数据，可能是作者限制了分享，也可能要登录才能拿到。\
                 内容本身通常还在，用 App 打开一般能看"
            }
            Reason::Parse => "平台页面结构变了，解析器需要更新",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::Deleted => "deleted",
            Reason::Login => "login",
            Reason::Blocked => "blocked",
            Reason::Unsupported => "unsupported",
            Reason::Network => "network",
            Reason::Timeout => "timeout",
            Reason::Empty => "empty",
            Reason::Restricted => "restricted",
            Reason::Parse => "parse",
        }
    }

    /// 换一条路 / 重试一次有没有意义。
    ///
    /// `Deleted` 和 `Unsupported` 重试多少次都是同一个结果，别浪费用户的时间。
    pub const fn is_retryable(self) -> bool {
        matches!(
            self,
            Reason::Blocked | Reason::Network | Reason::Timeout | Reason::Parse
        )
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub reason: Reason,
    /// 补充细节，可能为空；会拼在用户文案后面的括号里
    pub detail: String,
}

impl Error {
    pub fn new(reason: Reason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    pub fn bare(reason: Reason) -> Self {
        Self {
            reason,
            detail: String::new(),
        }
    }

    pub fn deleted(detail: impl Into<String>) -> Self {
        Self::new(Reason::Deleted, detail)
    }
    pub fn login(detail: impl Into<String>) -> Self {
        Self::new(Reason::Login, detail)
    }
    pub fn blocked(detail: impl Into<String>) -> Self {
        Self::new(Reason::Blocked, detail)
    }
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::new(Reason::Unsupported, detail)
    }
    pub fn restricted(detail: impl Into<String>) -> Self {
        Self::new(Reason::Restricted, detail)
    }
    /// 页面结构不对：这是唯一需要站长看一眼的分支，detail 要写清楚哪一步断了。
    pub fn parse(detail: impl Into<String>) -> Self {
        Self::new(Reason::Parse, detail)
    }
    pub fn empty() -> Self {
        Self::bare(Reason::Empty)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason.message())?;
        if !self.detail.is_empty() {
            write!(f, "（{}）", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------- 归类

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Error::bare(Reason::Timeout);
        }
        if e.is_connect() || e.is_request() {
            return Error::new(Reason::Network, truncate(&e.to_string(), 80));
        }
        if let Some(status) = e.status() {
            return from_status(status.as_u16());
        }
        if e.is_decode() || e.is_body() {
            return Error::parse(truncate(&e.to_string(), 100));
        }
        Error::new(Reason::Network, truncate(&e.to_string(), 80))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::parse(format!("JSON 解析失败: {}", truncate(&e.to_string(), 80)))
    }
}

impl From<url::ParseError> for Error {
    fn from(e: url::ParseError) -> Self {
        Error::unsupported(format!("链接格式不对: {e}"))
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Error::bare(Reason::Timeout)
    }
}

/// HTTP 状态码 -> 原因。
///
/// 412 单独拎出来：B 站用它表示"机房 IP 被拒"，归到 blocked 才能提示站长配代理。
pub fn from_status(status: u16) -> Error {
    match status {
        401 | 403 => Error::new(Reason::Blocked, format!("HTTP {status}")),
        404 | 410 => Error::new(Reason::Deleted, format!("HTTP {status}")),
        412 => Error::new(Reason::Blocked, "HTTP 412（平台拒绝了服务器所在网络）"),
        429 => Error::new(Reason::Blocked, "HTTP 429（请求太频繁）"),
        408 | 504 => Error::new(Reason::Timeout, format!("HTTP {status}")),
        500..=599 => Error::new(Reason::Network, format!("平台返回 HTTP {status}")),
        _ => Error::new(Reason::Parse, format!("意外的 HTTP {status}")),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    // 按字符截断, 不然中文会被切成半个字
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(from_status(404).reason, Reason::Deleted);
        assert_eq!(from_status(412).reason, Reason::Blocked);
        assert_eq!(from_status(429).reason, Reason::Blocked);
        assert_eq!(from_status(503).reason, Reason::Network);
    }

    #[test]
    fn display_includes_detail() {
        let e = Error::parse("slidesinfo 没有 aweme_details");
        let s = e.to_string();
        assert!(s.contains("解析器需要更新"));
        assert!(s.contains("slidesinfo"));
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "中文".repeat(100);
        let t = truncate(&s, 10);
        assert_eq!(t.chars().count(), 10);
    }

    #[test]
    fn dead_ends_are_not_retryable() {
        assert!(!Reason::Deleted.is_retryable());
        assert!(!Reason::Unsupported.is_retryable());
        assert!(Reason::Blocked.is_retryable());
    }
}
