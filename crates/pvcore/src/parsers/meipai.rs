//! 美拍。
//!
//! 播放地址藏在 `#shareMediaBtn[data-video]` 里，经过一层自定义混淆：
//! 前 4 个字符倒过来当十六进制数，它的十进制写法决定了后面那串要怎么删减，
//! 删完才是真正的 base64。逻辑是从页面 JS 里逆出来的，照抄不要"优化"。

use base64::Engine;
use scraper::{Html, Selector};

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, VideoInfo};

pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let resp = http
        .send(Req::get(url).header("User-Agent", ua::pick(ua::Platform::Desktop)))
        .await?;
    resp.error_for_status()?;
    let html = resp.text();
    build(&html)
}

fn build(html: &str) -> Result<VideoInfo> {
    let doc = Html::parse_document(html);
    let attr = |css: &str, name: &str| -> String {
        Selector::parse(css)
            .ok()
            .and_then(|s| doc.select(&s).next())
            .and_then(|el| el.value().attr(name).map(str::to_owned))
            .unwrap_or_default()
    };
    let text = |css: &str| -> String {
        Selector::parse(css)
            .ok()
            .and_then(|s| doc.select(&s).next())
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .unwrap_or_default()
    };

    let encoded = attr("#shareMediaBtn", "data-video");
    if encoded.is_empty() {
        return Err(Error::deleted("页面里没有 data-video，作品可能已删除"));
    }
    let video_url = decode_video(&encoded)?;

    let avatar = attr(".detail-avatar", "src");
    Ok(VideoInfo {
        video_url,
        cover_url: attr("#detailVideo img", "src"),
        title: text(".detail-cover-title"),
        author: Author {
            uid: attr(".detail-name a", "href")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned(),
            name: attr(".detail-avatar", "alt"),
            avatar: prefix_scheme(&avatar),
        },
        ..Default::default()
    })
}

/// 页面里的地址是 `//host/path` 形式，补上协议。
fn prefix_scheme(u: &str) -> String {
    if u.is_empty() {
        String::new()
    } else if u.starts_with("//") {
        format!("https:{u}")
    } else {
        u.to_owned()
    }
}

/// 解开 `data-video` 的混淆。
fn decode_video(encoded: &str) -> Result<String> {
    let bad = || Error::parse("data-video 的格式和已知的混淆规则对不上");

    if !encoded.is_ascii() || encoded.len() <= 4 {
        return Err(bad());
    }
    // 前 4 位倒序，当十六进制读
    let head: String = encoded[..4].chars().rev().collect();
    let tail_str = &encoded[4..];

    let n = u32::from_str_radix(&head, 16).map_err(|_| bad())?;
    let digits: Vec<usize> = n.to_string().bytes().map(|b| (b - b'0') as usize).collect();
    if digits.len() < 3 {
        return Err(bad());
    }
    let (pre, tail) = digits.split_at(digits.len() - 2);
    if pre.len() < 2 {
        return Err(bad());
    }

    let step1 = sub_str(tail_str, pre[0], pre[1]).ok_or_else(bad)?;
    // get_pos: 第一个下标换算成"从末尾数"
    let p0 = step1.len().checked_sub(tail[0] + tail[1]).ok_or_else(bad)?;
    let step2 = sub_str(&step1, p0, tail[1]).ok_or_else(bad)?;

    let raw = base64::engine::general_purpose::STANDARD
        .decode(step2.as_bytes())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(step2.as_bytes()))
        .map_err(|_| bad())?;
    let path = String::from_utf8(raw).map_err(|_| bad())?;
    Ok(format!("https:{path}"))
}

/// 取出 `s[i..i+len]` 这一段，把它从后半部分里**全部**删掉，再拼回来。
fn sub_str(s: &str, index: usize, len: usize) -> Option<String> {
    let end = index.checked_add(len)?;
    if end > s.len() {
        return None;
    }
    let head = &s[..index];
    let cut = &s[index..end];
    let rest = &s[end..];
    let cleaned = if cut.is_empty() {
        rest.to_owned()
    } else {
        rest.replace(cut, "")
    };
    Some(format!("{head}{cleaned}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_str_removes_all_occurrences() {
        // 取 [1..3) = "bc"，后半段里所有 "bc" 都要删掉
        assert_eq!(sub_str("abcXbcYbc", 1, 2).unwrap(), "aXY");
        assert_eq!(sub_str("abc", 0, 0).unwrap(), "abc");
        assert!(sub_str("abc", 2, 5).is_none());
    }

    #[test]
    fn decode_round_trips_a_constructed_payload() {
        // 按规则反向构造一份 data-video，走完整条解码链路验证结果。
        //
        // 取 head = 0x04D2 = 1234 -> digits [1,2,3,4] -> pre=[1,2], tail=[3,4]，
        // 于是解码时做的是 sub_str(tail_str, 1, 2) 再 sub_str(step1, len-7, 4)。
        let target = "//mvvideo.meipai.com/abc.mp4";
        let b64 = base64::engine::general_purpose::STANDARD.encode(target);

        // 反推 step1：在 B 的倒数第 3 个字符前插入 4 个"不会出现在剩余部分"的字符
        let cut4 = "!@#$";
        let split = b64.len() - 3;
        let step1 = format!("{}{cut4}{}", &b64[..split], &b64[split..]);
        assert_eq!(sub_str(&step1, step1.len() - 7, 4).unwrap(), b64);

        // 反推 tail_str：在 step1 的第 1 个字符后插入 2 个同样特殊的字符
        let cut2 = "%^";
        let tail_str = format!("{}{cut2}{}", &step1[..1], &step1[1..]);
        assert_eq!(sub_str(&tail_str, 1, 2).unwrap(), step1);

        // data-video 里存的是倒序的 head
        let head: String = "04D2".chars().rev().collect();
        let encoded = format!("{head}{tail_str}");

        assert_eq!(decode_video(&encoded).unwrap(), format!("https:{target}"));
    }

    #[test]
    fn garbage_input_is_a_parse_error() {
        assert_eq!(decode_video("xx").unwrap_err().reason, crate::Reason::Parse);
        assert_eq!(
            decode_video("ZZZZnotbase64").unwrap_err().reason,
            crate::Reason::Parse
        );
    }

    #[test]
    fn missing_data_video_reports_deleted() {
        assert_eq!(
            build("<html></html>").unwrap_err().reason,
            crate::Reason::Deleted
        );
    }

    #[test]
    fn scheme_is_prefixed() {
        assert_eq!(prefix_scheme("//a/b.jpg"), "https://a/b.jpg");
        assert_eq!(prefix_scheme("https://a/b.jpg"), "https://a/b.jpg");
        assert_eq!(prefix_scheme(""), "");
    }
}
