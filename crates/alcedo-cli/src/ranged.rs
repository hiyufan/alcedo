//! googlevideo 的分段拉取。
//!
//! YouTube 的媒体 CDN 对"一个请求拉完整个文件"限速：2026-10-22 经 WARP 实测同一条
//! 75 MB 的 720p 分轨，整段请求 0.24 MB/s，按 10 MB 一段 Range 请求 20.5 MB/s
//! （yt-dlp 默认也是 10 MB 一段）。这里把一串 Range 请求拼成一条连续的字节流，
//! 调用方（download 落盘、serve /media 转发）照旧当一条流处理。

use std::io;
use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::HeaderMap;

/// 每段多大。
pub(crate) const CHUNK: u64 = 10 << 20;

/// 拼好的字节流。
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// 一次分段拉取。
pub(crate) struct Ranged {
    /// 第一段的响应头（content-type、etag 这些）
    pub(crate) headers: HeaderMap,
    /// 文件总大小，来自第一段的 `Content-Range`
    pub(crate) total: u64,
    /// 交付的第一个字节
    pub(crate) start: u64,
    /// 交付的最后一个字节（含）
    pub(crate) end: u64,
    /// 从 `start` 到 `end` 的完整内容
    pub(crate) body: ByteStream,
}

impl Ranged {
    /// 交付多少字节。
    pub(crate) const fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// 从 `start` 拉到 `end`（含；`None` 表示到文件末尾）。
///
/// 第一段没回 206（CDN 不认 Range、403 之类）时把那个响应原样交回，
/// 调用方按普通请求处理。
pub(crate) async fn open(
    client: &'static reqwest::Client,
    url: &str,
    headers: HeaderMap,
    start: u64,
    end: Option<u64>,
) -> Result<Result<Ranged, reqwest::Response>, reqwest::Error> {
    let first_to = end.map_or(start + CHUNK - 1, |e| e.min(start + CHUNK - 1));
    let resp = send_range(client, url, &headers, start, first_to).await?;
    let Some((got_start, got_end, Some(total))) = partial_range(&resp) else {
        return Ok(Err(resp));
    };
    if got_start != start || got_end < got_start || total == 0 {
        return Ok(Err(resp));
    }
    let end = end.map_or(total - 1, |e| e.min(total - 1));
    let first_headers = resp.headers().clone();
    let state = State {
        client,
        url: url.to_owned(),
        headers,
        cur: Some(byte_stream(resp)),
        left: got_end - got_start + 1,
        next: got_end + 1,
        end,
    };
    Ok(Ok(Ranged {
        headers: first_headers,
        total,
        start,
        end,
        body: Box::pin(futures_util::stream::unfold(state, step)),
    }))
}

struct State {
    client: &'static reqwest::Client,
    url: String,
    headers: HeaderMap,
    /// 正在读的这一段
    cur: Option<ByteStream>,
    /// 这一段还差多少字节：连接提前断了不能悄悄跳到下一段，文件中间会缺一块
    left: u64,
    /// 下一段从哪个字节开始
    next: u64,
    end: u64,
}

async fn step(mut st: State) -> Option<(io::Result<Bytes>, State)> {
    loop {
        if let Some(cur) = st.cur.as_mut() {
            match cur.next().await {
                Some(Ok(b)) => {
                    st.left = st.left.saturating_sub(b.len() as u64);
                    return Some((Ok(b), st));
                }
                Some(Err(e)) => return Some(fail(st, e)),
                None if st.left > 0 => return Some(fail(st, "分段没有下完整，连接提前断了")),
                None => st.cur = None,
            }
        }
        if st.next > st.end {
            return None;
        }
        let to = st.end.min(st.next + CHUNK - 1);
        let resp = match send_range(st.client, &st.url, &st.headers, st.next, to).await {
            Ok(r) => r,
            Err(e) => return Some(fail(st, e)),
        };
        match partial_range(&resp) {
            Some((a, b, _)) if a == st.next && a <= b && b <= to => {
                st.left = b - a + 1;
                st.next = b + 1;
                st.cur = Some(byte_stream(resp));
            }
            Some(_) => return Some(fail(st, "分段响应的 Content-Range 对不上")),
            None => {
                let status = resp.status().as_u16();
                return Some(fail(st, format!("分段请求返回 HTTP {status}")));
            }
        }
    }
}

/// 出错后流就此结束：调用方拿到这个错误，不会再有下一段。
fn fail(
    mut st: State,
    err: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> (io::Result<Bytes>, State) {
    st.cur = None;
    st.left = 0;
    st.next = st.end.saturating_add(1);
    (Err(io::Error::other(err)), st)
}

async fn send_range(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    from: u64,
    to: u64,
) -> Result<reqwest::Response, reqwest::Error> {
    let request = || {
        client
            .get(url)
            .headers(headers.clone())
            .header("range", format!("bytes={from}-{to}"))
            .send()
    };
    // 长下载里偶尔一段连不上，重试一次，别让几百 MB 白下
    match request().await {
        Ok(r) => Ok(r),
        Err(_) => request().await,
    }
}

fn byte_stream(resp: reqwest::Response) -> ByteStream {
    Box::pin(resp.bytes_stream().map(|r| r.map_err(io::Error::other)))
}

/// 206 响应的 `Content-Range: bytes a-b/total` → `(a, b, total)`；total 是 `*` 时为 `None`。
fn partial_range(resp: &reqwest::Response) -> Option<(u64, u64, Option<u64>)> {
    if resp.status().as_u16() != 206 {
        return None;
    }
    parse_content_range(resp.headers().get("content-range")?.to_str().ok()?)
}

fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let (range, total) = v.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((a.parse().ok()?, b.parse().ok()?, total))
}

/// 客户端的 `Range` 头 → `(起点, 终点)`，没带就是整个文件。
///
/// 只处理会撞上限速的形态：没带 Range、`bytes=N-`、跨度超过一段的 `bytes=N-M`。
/// 后缀（`bytes=-N`）、多段、一段以内的请求返回 `None`，原样透传就行。
pub(crate) fn wanted_range(header: Option<&str>) -> Option<(u64, Option<u64>)> {
    let Some(h) = header else {
        return Some((0, None));
    };
    let (a, b) = h.trim().strip_prefix("bytes=")?.split_once('-')?;
    if b.contains(',') {
        return None;
    }
    let start: u64 = a.trim().parse().ok()?;
    if b.trim().is_empty() {
        return Some((start, None));
    }
    let end: u64 = b.trim().parse().ok()?;
    (end >= start && end - start + 1 > CHUNK).then_some((start, Some(end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_is_parsed() {
        assert_eq!(
            parse_content_range("bytes 0-10485759/75000000"),
            Some((0, 10_485_759, Some(75_000_000)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range("bytes x-1/2"), None);
    }

    #[test]
    fn only_ranges_that_hit_the_throttle_are_chunked() {
        assert_eq!(wanted_range(None), Some((0, None)), "没带 Range = 整个文件");
        assert_eq!(wanted_range(Some("bytes=0-")), Some((0, None)));
        assert_eq!(wanted_range(Some("bytes=1000-")), Some((1000, None)));
        assert_eq!(
            wanted_range(Some("bytes=0-52428799")),
            Some((0, Some(52_428_799)))
        );
        assert_eq!(wanted_range(Some("bytes=0-1023")), None, "一段以内原样透传");
        assert_eq!(wanted_range(Some("bytes=-500")), None, "后缀形式原样透传");
        assert_eq!(wanted_range(Some("bytes=0-1,5-9")), None, "多段原样透传");
        assert_eq!(wanted_range(Some("bytes=9-1")), None);
    }

    #[tokio::test]
    async fn chunks_are_stitched_into_one_stream() {
        let data: Vec<u8> = (0..(CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
        let url = serve_ranges(data.clone()).await;
        let client: &'static reqwest::Client = test_client();

        let r = open(client, &url, HeaderMap::new(), 0, None)
            .await
            .unwrap()
            .expect("应当是分段拉取");
        assert_eq!(
            (r.total, r.start, r.end),
            (data.len() as u64, 0, data.len() as u64 - 1)
        );
        let got = collect(r.body).await.unwrap();
        assert_eq!(got.len(), data.len());
        assert!(got == data, "拼起来的内容要和原文件一模一样");

        // 从中间开始、到中间结束（跨三段）
        let (from, to) = (CHUNK - 7, CHUNK * 2 + 50);
        let r = open(client, &url, HeaderMap::new(), from, Some(to))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.len(), to - from + 1);
        let got = collect(r.body).await.unwrap();
        let (from, to) = (usize::try_from(from).unwrap(), usize::try_from(to).unwrap());
        assert!(got == data[from..=to]);
    }

    #[tokio::test]
    async fn short_chunk_is_an_error_not_a_silent_gap() {
        let data = vec![7_u8; usize::try_from(CHUNK + 10).unwrap()];
        let url = serve_ranges_truncated(data).await;
        let client: &'static reqwest::Client = test_client();
        let r = open(client, &url, HeaderMap::new(), 0, None)
            .await
            .unwrap()
            .unwrap();
        assert!(collect(r.body).await.is_err(), "缺了一块要报错");
    }

    fn test_client() -> &'static reqwest::Client {
        alcedo::http::ensure_crypto_provider();
        Box::leak(Box::new(reqwest::Client::new()))
    }

    async fn collect(mut body: ByteStream) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(b) = body.next().await {
            out.extend_from_slice(&b?);
        }
        Ok(out)
    }

    /// 本机起一个只认 Range 的极简 HTTP 服务，模拟 googlevideo。
    async fn serve_ranges(data: Vec<u8>) -> String {
        serve(data, false).await
    }

    /// 同上，但每段少给几个字节、提前断开。
    async fn serve_ranges_truncated(data: Vec<u8>) -> String {
        serve(data, true).await
    }

    async fn serve(data: Vec<u8>, truncate: bool) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let data = std::sync::Arc::new(data);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let data = std::sync::Arc::clone(&data);
                tokio::spawn(async move {
                    let mut buf = vec![0_u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let range = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim().split_once('-'))
                        .and_then(|(a, b)| {
                            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                        });
                    let Some((a, b)) = range else {
                        let _ = sock
                            .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        return;
                    };
                    let b = b.min(data.len() - 1);
                    let body = &data[a..=b];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\ncontent-range: bytes {a}-{b}/{}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        data.len(),
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let send = if truncate {
                        &body[..body.len() - 5]
                    } else {
                        body
                    };
                    let _ = sock.write_all(send).await;
                });
            }
        });
        format!("http://{addr}/videoplayback")
    }
}
