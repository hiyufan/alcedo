//! 从页面的 OpenGraph / Twitter Card 标签里兜底取媒体。
//!
//! 几个海外平台（Instagram、Threads、Facebook、Pinterest）的正式接口对机房 IP
//! 很不友好，但它们的分享卡片元数据一直是公开的——给爬虫看的那份。拿不到接口
//! 数据时，这里至少能给出视频地址、封面和标题，比直接报"解析失败"强得多。
//!
//! 实现上整页只扫一遍：[`collect_meta`] 把所有 `<meta>` 标签的
//! `(property|name, content)` 收进一张小表，之后每个键都是查表。早期版本是
//! "每个键从头 find 四遍"（两种属性 × 两种引号），`from_og` 要 8 个键，最坏
//! 三十多遍几百 KB 页面的全文扫描，兜底路径上白付几十毫秒。

use crate::model::VideoInfo;

/// 取一个 `<meta property/name="key" content="...">` 的值。
///
/// 手写扫描而不是丢给 HTML 解析器：这些页面动辄几百 KB，为了四五个 meta 标签
/// 建整棵 DOM 不划算，而 meta 标签的形状足够规整。
pub fn meta_content(html: &str, key: &str) -> String {
    collect_meta(html)
        .iter()
        .find(|(k, _)| *k == key)
        .map_or_else(String::new, |(_, v)| decode_entities(v))
}

/// 一遍扫出页面里所有 `<meta>` 标签的 `(property|name, content)`。
///
/// 键和值都借自 `html`；没有 content、content 为空的不收。同名取先出现的。
fn collect_meta(html: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(rel) = rest.find("<meta") {
        let at = rel + "<meta".len();
        // 属性文本到 `>` 为止；找不到（残缺页面）就吞到结尾
        let end = rest[at..].find('>').map_or(rest.len(), |i| at + i + 1);
        let mut key = None;
        let mut content = None;
        for (name, value) in attrs(&rest[at..end]) {
            match name {
                "property" | "name" => {
                    key.get_or_insert(value);
                }
                "content" => {
                    content.get_or_insert(value);
                }
                _ => {}
            }
        }
        if let (Some(k), Some(v)) = (key, content) {
            if !v.is_empty() {
                out.push((k, v));
            }
        }
        rest = &rest[end..];
    }
    out
}

/// 收集标签体里的属性对。零分配，名字和值都借自标签文本。
///
/// 属性名做完整匹配，所以 `data-content="..."` 不会被当成 `content`。带引号
/// 和不带引号的值都认；`>` 出现在带引号的值里会把标签提前截断——meta 标签
/// 不会这么写，和旧实现的边界一致。
fn attrs(tag: &str) -> Vec<(&str, &str)> {
    fn is_space(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\n' | b'\r')
    }

    let mut out = Vec::new();
    let b = tag.as_bytes();
    let mut i = 0;
    while i < tag.len() {
        if is_space(b[i]) || b[i] == b'/' {
            i += 1;
            continue;
        }
        if b[i] == b'>' {
            // 标签结束（切片里带着收尾的 '>'）
            break;
        }
        // 属性名：到 '='、空白或 '>' 为止
        let name_start = i;
        while i < tag.len() && !is_space(b[i]) && !matches!(b[i], b'=' | b'>') {
            i += 1;
        }
        let name = &tag[name_start..i];
        while i < tag.len() && is_space(b[i]) {
            i += 1;
        }
        if i >= tag.len() || b[i] != b'=' {
            // 无值属性（`disabled` 这类）：名字读完了继续找下一个
            continue;
        }
        i += 1;
        while i < tag.len() && is_space(b[i]) {
            i += 1;
        }
        if i < tag.len() && (b[i] == b'"' || b[i] == b'\'') {
            let quote = b[i];
            i += 1;
            let v_start = i;
            while i < tag.len() && b[i] != quote {
                i += 1;
            }
            out.push((name, &tag[v_start..i]));
            if i < tag.len() {
                i += 1; // 吃掉收尾引号
            }
        } else {
            // 不带引号的值：到空白或 '>' 为止
            let v_start = i;
            while i < tag.len() && !is_space(b[i]) && b[i] != b'>' {
                i += 1;
            }
            if i > v_start {
                out.push((name, &tag[v_start..i]));
            }
        }
    }
    out
}

/// HTML 实体：meta 里的地址常常把 `&` 写成 `&amp;`，不还原直接请求会 404。
///
/// 单遍扫描。实体都是 ASCII，按字节配前缀就行；`&amp;` 必须排在第一个，
/// 这样 `&amp;lt;` 还原成 `&lt;` 而不是 `<`，不做二次解码。
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    const ENTITIES: [(&str, &str); 7] = [
        ("&amp;", "&"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&nbsp;", " "),
    ];

    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match ENTITIES.iter().find(|(from, _)| tail.starts_with(from)) {
            Some((from, to)) => {
                out.push_str(to);
                rest = &tail[from.len()..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 用 OG 标签拼一份结果。拿不到任何媒体就返回 `None`。
pub fn from_og(html: &str) -> Option<VideoInfo> {
    let meta = collect_meta(html);
    let pick = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| meta.iter().find(|(mk, _)| *mk == *k))
            .map_or_else(String::new, |(_, v)| decode_entities(v))
    };

    let video = pick(&["og:video:secure_url", "og:video"]);
    let image = pick(&["og:image"]);
    if video.is_empty() && image.is_empty() {
        return None;
    }

    let title = pick(&["og:title", "twitter:title"]);
    let description = pick(&["og:description"]);

    let mut info = VideoInfo {
        video_url: video,
        cover_url: image.clone(),
        title: if title.is_empty() { description } else { title },
        width: pick(&["og:video:width"]).parse().unwrap_or(0),
        height: pick(&["og:video:height"]).parse().unwrap_or(0),
        ..Default::default()
    };

    // 纯图片贴：把 og:image 当成图集里唯一一张
    if info.video_url.is_empty() && !image.is_empty() {
        info.images.push(crate::model::Image::new(image));
    }
    Some(info)
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
    <html><head>
      <meta property="og:title" content="标题 &amp; 副标题">
      <meta property="og:image" content="https://cdn/img.jpg?a=1&amp;b=2">
      <meta property="og:video" content="https://cdn/v.mp4">
      <meta property="og:video:width" content="1080">
      <meta property="og:video:height" content="1920">
      <meta name="twitter:title" content="不该用这个">
    </head></html>"#;

    #[test]
    fn reads_og_tags() {
        assert_eq!(meta_content(PAGE, "og:title"), "标题 & 副标题");
        assert_eq!(meta_content(PAGE, "og:video"), "https://cdn/v.mp4");
        assert_eq!(meta_content(PAGE, "og:missing"), "");
    }

    #[test]
    fn entities_are_decoded_in_urls() {
        // 不还原 &amp; 的话这个地址请求回来是 404
        assert_eq!(
            meta_content(PAGE, "og:image"),
            "https://cdn/img.jpg?a=1&b=2"
        );
    }

    #[test]
    fn builds_video_info() {
        let info = from_og(PAGE).unwrap();
        assert_eq!(info.video_url, "https://cdn/v.mp4");
        assert_eq!(info.width, 1080);
        assert_eq!(info.height, 1920);
        assert_eq!(info.title, "标题 & 副标题");
        assert!(info.images.is_empty(), "有视频时不该塞图集");
    }

    #[test]
    fn image_only_page_becomes_gallery() {
        let html = r#"<meta property="og:image" content="https://cdn/p.jpg">"#;
        let info = from_og(html).unwrap();
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 1);
    }

    #[test]
    fn page_without_media_yields_none() {
        assert!(from_og("<html><title>x</title></html>").is_none());
    }

    #[test]
    fn single_quoted_attributes_work() {
        let html = "<meta property='og:video' content='https://cdn/v.mp4'>";
        assert_eq!(meta_content(html, "og:video"), "https://cdn/v.mp4");
    }

    #[test]
    fn name_attribute_is_also_accepted() {
        let html = r#"<meta name="og:video" content="https://cdn/n.mp4">"#;
        assert_eq!(meta_content(html, "og:video"), "https://cdn/n.mp4");
    }

    #[test]
    fn first_tag_with_content_wins() {
        // 同名标签出现两次，取先出现且 content 非空的那个
        let html = r#"<meta property="og:video" content="">
                      <meta property="og:video" content="https://cdn/2.mp4">"#;
        assert_eq!(meta_content(html, "og:video"), "https://cdn/2.mp4");
    }

    #[test]
    fn prefixed_attribute_names_are_not_confused() {
        // 旧实现按子串找 `content="`，会把 data-content 当成 content
        let html = r#"<meta property="og:video" data-content="假的" content="https://cdn/v.mp4">"#;
        assert_eq!(meta_content(html, "og:video"), "https://cdn/v.mp4");
    }

    #[test]
    fn secure_url_beats_plain_video() {
        let html = r#"<meta property="og:video" content="https://cdn/http.mp4">
                      <meta property="og:video:secure_url" content="https://cdn/https.mp4">"#;
        let info = from_og(html).unwrap();
        assert_eq!(info.video_url, "https://cdn/https.mp4");
    }

    #[test]
    fn entities_decode_in_one_pass_without_double_decoding() {
        // &amp; 必须先于其它实体：&amp;lt; 是字面 "&lt;"，不能再解一层
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_entities("a&amp;b&#39;c&nbsp;d"), "a&b'c d");
        assert_eq!(
            decode_entities("没有实体 & 也不是实体"),
            "没有实体 & 也不是实体"
        );
    }
}
