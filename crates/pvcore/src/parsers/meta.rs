//! 从页面的 OpenGraph / Twitter Card 标签里兜底取媒体。
//!
//! 几个海外平台（Instagram、Threads、Facebook、Pinterest）的正式接口对机房 IP
//! 很不友好，但它们的分享卡片元数据一直是公开的——给爬虫看的那份。拿不到接口
//! 数据时，这里至少能给出视频地址、封面和标题，比直接报"解析失败"强得多。

use crate::model::VideoInfo;

/// 取一个 `<meta property="og:xxx" content="...">` 的值。
///
/// 手写扫描而不是丢给 HTML 解析器：这些页面动辄几百 KB，为了四五个 meta 标签
/// 建整棵 DOM 不划算，而 meta 标签的形状足够规整，扫一遍就够。
pub fn meta_content(html: &str, key: &str) -> String {
    for (attr, other) in [("property", "content"), ("name", "content")] {
        let needles = [format!("{attr}=\"{key}\""), format!("{attr}='{key}'")];
        for needle in &needles {
            let mut from = 0;
            while let Some(rel) = html[from..].find(needle.as_str()) {
                let at = from + rel;
                // 往回找到这个标签的起点，往前找到结尾
                let start = html[..at].rfind('<').unwrap_or(at);
                let end = html[at..].find('>').map(|i| at + i).unwrap_or(html.len());
                let tag = &html[start..end];
                if let Some(v) = attr_value(tag, other) {
                    if !v.is_empty() {
                        return decode_entities(&v);
                    }
                }
                from = end.max(at + needle.len());
            }
        }
    }
    String::new()
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let at = tag
        .find(&format!("{name}=\""))
        .map(|i| (i + name.len() + 2, '"'))
        .or_else(|| {
            tag.find(&format!("{name}='"))
                .map(|i| (i + name.len() + 2, '\''))
        })?;
    let (start, quote) = at;
    let end = tag[start..].find(quote)? + start;
    Some(tag[start..end].to_owned())
}

/// HTML 实体：meta 里的地址常常把 `&` 写成 `&amp;`，不还原直接请求会 404。
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

/// 用 OG 标签拼一份结果。拿不到任何媒体就返回 `None`。
pub fn from_og(html: &str) -> Option<VideoInfo> {
    let video = {
        let v = meta_content(html, "og:video:secure_url");
        if v.is_empty() {
            meta_content(html, "og:video")
        } else {
            v
        }
    };
    let image = meta_content(html, "og:image");
    if video.is_empty() && image.is_empty() {
        return None;
    }

    let title = {
        let t = meta_content(html, "og:title");
        if t.is_empty() {
            meta_content(html, "twitter:title")
        } else {
            t
        }
    };
    let description = meta_content(html, "og:description");

    let mut info = VideoInfo {
        video_url: video,
        cover_url: image.clone(),
        title: if title.is_empty() { description } else { title },
        width: meta_content(html, "og:video:width").parse().unwrap_or(0),
        height: meta_content(html, "og:video:height").parse().unwrap_or(0),
        ..Default::default()
    };

    // 纯图片贴：把 og:image 当成图集里唯一一张
    if info.video_url.is_empty() && !image.is_empty() {
        info.images.push(crate::model::Image::new(image));
    }
    Some(info)
}

#[cfg(test)]
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
}
