//! 解析器共用的小工具：URL 取参、HTML 里抠 JSON、JSON 取值。

use crate::error::{Error, Result};
use serde_json::Value;

/// 从 URL 的 query 里取一个参数。
pub fn query_param(url: &str, key: &str) -> Result<String> {
    let parsed = url::Url::parse(url)?;
    parsed
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::unsupported(format!("链接里没有 {key} 参数")))
}

/// 主机名（小写，去掉末尾的点）。
pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
}

/// 路径最后一个非空片段。
pub fn last_path_segment(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed
        .path_segments()?
        .rfind(|s| !s.is_empty())
        .map(|s| s.to_owned())
}

/// 从一段文本里抠出第一个 http(s) 链接。
///
/// 用户经常整段粘贴 App 的分享文案，形如
/// `3.14 复制打开抖音，看看…… <https://v.douyin.com/xxx/>`。
pub fn extract_url(text: &str) -> Option<&str> {
    let start = text
        .find("http://")
        .into_iter()
        .chain(text.find("https://"))
        .min()?;
    let rest = &text[start..];
    // 链接到空白、中文或常见的收尾标点为止
    let end = rest
        .char_indices()
        .find(|(_, c)| {
            c.is_whitespace() || !c.is_ascii() || matches!(c, '"' | '\'' | '<' | '>' | '，' | '。')
        })
        .map_or(rest.len(), |(i, _)| i);
    let url = rest[..end].trim_end_matches(['.', ',', ')', ']', '}', '、']);
    (url.len() > "https://".len()).then_some(url)
}

// ------------------------------------------------------------------ HTML 里的 JSON

/// 在 HTML 里找 `marker` 之后那一段 JSON，按括号配对取完整。
///
/// 比 `marker\s*=\s*(.*?)</script>` 这种正则写法稳得多：
///
/// - 正文里出现 `</script>` 字符串（转义过的）不会把 JSON 截断
/// - 后面跟着别的 JS 语句时也能正确停在 JSON 结尾，不会把 `;var x=1` 一起吞进去
/// - 不用回溯的正则，长页面上快一个量级
pub fn json_after<'a>(html: &'a str, marker: &str) -> Option<&'a str> {
    let pos = html.find(marker)? + marker.len();
    let rest = &html[pos..];
    let start = rest.find(['{', '['])?;
    balanced_json(&rest[start..])
}

/// 从开头是 `{` 或 `[` 的字符串里截出配对完整的那一段。
///
/// 会正确跳过字符串字面量和里面的转义，所以 `{"a":"}"}` 不会被提前截断。
pub fn balanced_json(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let open = *bytes.first()?;
    let close = match open {
        b'{' => b'}',
        b'[' => b']',
        _ => return None,
    };

    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            _ if b == open => depth += 1,
            _ if b == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 取 `<script id="...">` 标签里的内容。
pub fn script_by_id<'a>(html: &'a str, id: &str) -> Option<&'a str> {
    // 属性顺序不固定, 先定位到 id 再往回找标签开头
    let needle = format!("id=\"{id}\"");
    let at = html
        .find(&needle)
        .or_else(|| html.find(&format!("id='{id}'")))?;
    let tag_end = html[at..].find('>')? + at + 1;
    let close = html[tag_end..].find("</script>")? + tag_end;
    Some(html[tag_end..close].trim())
}

/// 页面 `<title>`，给"平台返回了什么页面"这类报错用。
pub fn html_title(html: &str) -> String {
    let Some(start) = html.find("<title") else {
        return String::new();
    };
    let Some(open_end) = html[start..].find('>').map(|i| start + i + 1) else {
        return String::new();
    };
    let Some(close) = html[open_end..].find("</title>").map(|i| open_end + i) else {
        return String::new();
    };
    html[open_end..close].trim().chars().take(60).collect()
}

/// 去掉 HTML 标签，留纯文本（微博正文里带 `<a>` 之类）。
pub fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// 把 JS 的 `undefined` 换成 `null`，好让 JSON 解析器吃得下。
///
/// 只替换独立的词，避免把 `"undefinedFoo"` 这种也改掉。
pub fn undefined_to_null(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains("undefined") {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while let Some(found) = s[i..].find("undefined") {
        let at = i + found;
        let before_ok = at == 0 || !is_ident_byte(bytes[at - 1]);
        let after = at + "undefined".len();
        let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
        out.push_str(&s[i..at]);
        out.push_str(if before_ok && after_ok {
            "null"
        } else {
            "undefined"
        });
        i = after;
    }
    out.push_str(&s[i..]);
    std::borrow::Cow::Owned(out)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

// ------------------------------------------------------------------ JSON 取值

/// 按路径取值，中间任何一层缺失都返回 `None`。
///
/// 路径元素是字段名或数字下标：`get(v, &["data", "0", "url"])`。
pub fn get<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = value;
    for key in path {
        cur = match cur {
            Value::Object(map) => map.get(*key)?,
            Value::Array(arr) => arr.get(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// 取字符串；不是字符串或不存在都返回空串。
pub fn str_at(value: &Value, path: &[&str]) -> String {
    get(value, path)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// 取数字；缺失返回 0。数字被编码成字符串时也认。
pub fn num_at(value: &Value, path: &[&str]) -> f64 {
    match get(value, path) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// 取 ID。各家平台的 uid / postId 有的给字符串有的给数字，甚至同一个接口
/// 不同字段还不一样，用 [`str_at`] 取会静默得到空串。
pub fn id_at(value: &Value, path: &[&str]) -> String {
    match get(value, path) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 挨个试几个候选路径，返回第一个非空的 ID。
pub fn first_id(value: &Value, paths: &[&[&str]]) -> String {
    for p in paths {
        let s = id_at(value, p);
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

/// 取有符号整数。平台的错误码、状态码基本都是这个形状。
///
/// 不走 `num_at(..) as i64`：那条路先过一遍 f64，大整数会丢精度，NaN 会变成 0，
/// 而且散在十几个调用点上各写一遍 `as i64` 很容易漏掉某处的边界。
// 这个函数存在的意义就是把不可信的 JSON 数字安全地收进 i64：
// clamp 之后的 as 不会回绕，正是想要的饱和语义
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn i64_at(value: &Value, path: &[&str]) -> i64 {
    match get(value, path) {
        Some(Value::Number(n)) => n
            .as_i64()
            // 超出 i64 的浮点（平台偶尔给科学计数法）按饱和处理，不要回绕
            .or_else(|| {
                n.as_f64()
                    .map(|f| f.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
            })
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

// 同上：先判有限、再 min 到上界，剩下的 as 是安全的
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
/// 取无符号整数。负数、NaN 和缺失一律当 0。
pub fn u64_at(value: &Value, path: &[&str]) -> u64 {
    let n = num_at(value, path);
    if n.is_finite() && n > 0.0 {
        // clamp 而不是裸 as：体积字段偶尔是脏数据，回绕出一个巨大的值
        // 会让上层的配额判断失效
        n.min(u64::MAX as f64) as u64
    } else {
        0
    }
}

/// 取 `u32`，超出范围时夹到上界。
pub fn u32_at(value: &Value, path: &[&str]) -> u32 {
    u32::try_from(u64_at(value, path)).unwrap_or(u32::MAX)
}

/// 数组；不是数组就给个空切片。
pub fn arr_at<'a>(value: &'a Value, path: &[&str]) -> &'a [Value] {
    get(value, path)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

/// 第一个非空字符串。挨个试几个候选字段时用。
pub fn first_str(value: &Value, paths: &[&[&str]]) -> String {
    for p in paths {
        let s = str_at(value, p);
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

/// 字符串列表里挑第一个不是 `.webp` 的。
///
/// 抖音 / 小红书给的 `url_list` 第一条常常是 webp，很多下载器和相册不认。
/// 地址带签名参数，所以只能看 `?` 前面的路径。
pub fn prefer_non_webp(urls: &[Value]) -> String {
    let as_str = |v: &Value| v.as_str().unwrap_or("").to_owned();
    for v in urls {
        let u = as_str(v);
        // 大小写不敏感：CDN 偶尔回 `.WEBP`，按字面比会漏掉
        let path = u.split('?').next().unwrap_or("");
        if !u.is_empty() && !path.to_ascii_lowercase().ends_with(".webp") {
            return u;
        }
    }
    urls.first().map(as_str).unwrap_or_default()
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn balanced_json_handles_braces_in_strings() {
        let s = r#"{"a":"}{","b":[1,2]} trailing junk"#;
        assert_eq!(balanced_json(s).unwrap(), r#"{"a":"}{","b":[1,2]}"#);
    }

    #[test]
    fn balanced_json_handles_escapes() {
        let s = r#"{"a":"say \"hi\" }"} rest"#;
        assert_eq!(balanced_json(s).unwrap(), r#"{"a":"say \"hi\" }"}"#);
    }

    #[test]
    fn json_after_stops_at_json_end_not_script_end() {
        // 真实页面里 JSON 后面还跟着别的 JS; 非贪婪正则会把它们一起吞掉
        let html = r#"<script>window._ROUTER_DATA = {"a":1};var x=2;</script>"#;
        assert_eq!(
            json_after(html, "window._ROUTER_DATA").unwrap(),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn json_after_survives_escaped_script_tag() {
        let html = r#"<script>window.S = {"html":"<\/script>","ok":true}</script>"#;
        let got = json_after(html, "window.S").unwrap();
        let v: Value = serde_json::from_str(got).unwrap();
        assert_eq!(v["ok"], json!(true));
    }

    #[test]
    fn script_by_id_extracts_payload() {
        let html = r#"<script id="__NEXT_DATA__" type="application/json">{"k":1}</script>"#;
        assert_eq!(script_by_id(html, "__NEXT_DATA__").unwrap(), r#"{"k":1}"#);
    }

    #[test]
    fn undefined_becomes_null_but_not_inside_words() {
        assert_eq!(undefined_to_null(r#"{"a":undefined}"#), r#"{"a":null}"#);
        assert_eq!(
            undefined_to_null(r#"{"a":"undefinedX"}"#),
            r#"{"a":"undefinedX"}"#
        );
        // 没有 undefined 时不分配新字符串
        assert!(matches!(
            undefined_to_null("{}"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn extract_url_from_share_text() {
        let text = "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/iRNBho6u/ 很好看";
        assert_eq!(extract_url(text).unwrap(), "https://v.douyin.com/iRNBho6u/");
    }

    #[test]
    fn extract_url_trims_trailing_punctuation() {
        assert_eq!(
            extract_url("看这个 https://x.com/a/1.").unwrap(),
            "https://x.com/a/1"
        );
    }

    #[test]
    fn id_at_accepts_string_or_number() {
        // 同一个字段在不同平台 / 不同接口里两种类型都出现过
        let v = json!({"a": {"id": 123}, "b": {"id": "456"}, "c": {"id": null}});
        assert_eq!(id_at(&v, &["a", "id"]), "123");
        assert_eq!(id_at(&v, &["b", "id"]), "456");
        assert_eq!(id_at(&v, &["c", "id"]), "");
        assert_eq!(id_at(&v, &["nope"]), "");
    }

    #[test]
    fn first_id_falls_through() {
        let v = json!({"uid": null, "userId": 99});
        assert_eq!(first_id(&v, &[&["uid"], &["userId"]]), "99");
    }

    #[test]
    fn json_path_access() {
        let v = json!({"data": {"list": [{"url": "u1"}]}, "n": "42"});
        assert_eq!(str_at(&v, &["data", "list", "0", "url"]), "u1");
        assert_eq!(str_at(&v, &["data", "missing", "0"]), "");
        assert_eq!(num_at(&v, &["n"]), 42.0); // 字符串数字也认
        assert_eq!(arr_at(&v, &["data", "list"]).len(), 1);
    }

    #[test]
    fn non_webp_preferred_ignoring_query() {
        let urls = vec![
            json!("https://a/x.webp?sign=1"),
            json!("https://a/x.jpeg?sign=2"),
        ];
        assert!(prefer_non_webp(&urls).contains(".jpeg"));
    }

    #[test]
    fn strip_tags_keeps_text() {
        assert_eq!(strip_tags("<a href=\"x\">看看</a> 这个"), "看看 这个");
    }

    #[test]
    fn title_extraction() {
        assert_eq!(html_title("<html><title> 登录 </title>"), "登录");
    }
}
