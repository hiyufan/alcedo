//! 皮皮虾。
//!
//! 作品本身给的视频常带水印；作者在评论区回复的那条往往是无水印版本，
//! 所以先取作品的地址兜底，再在评论里找作者自己发的那条覆盖掉。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Author, Image, VideoInfo};
use crate::util;

/// 解析一条皮皮虾分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    let location = http.resolve_redirect(url).await?;
    let id = location
        .split('?')
        .next()
        .and_then(|p| p.trim_end_matches('/').rsplit('/').next())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::deleted("短链跳转后没有作品 ID"))?
        .to_owned();
    parse_id(http, &id).await
}

/// 已知皮皮虾作品 ID 时直接解析。
pub async fn parse_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let api = format!(
        "https://api.pipix.com/bds/cell/cell_comment/?offset=0&cell_type=1&api_version=1\
         &cell_id={id}&ac=wifi&channel=huawei_1319_64&aid=1319&app_name=super"
    );
    let json = http.get_json(&api).await?;

    let status = util::i64_at(&json, &["status_code"]);
    if status != 0 {
        return Err(Error::restricted(format!(
            "获取作品信息失败: {}",
            util::str_at(&json, &["prompt"])
        )));
    }

    let item = util::get(
        &json,
        &["data", "cell_comments", "0", "comment_info", "item"],
    )
    .ok_or_else(|| Error::deleted("皮皮虾没有返回作品"))?;

    Ok(build(item))
}

fn build(item: &Value) -> VideoInfo {
    let author_id = util::i64_at(item, &["author", "id"]);

    // 图集
    let images: Vec<Image> = util::arr_at(item, &["note", "multi_image"])
        .iter()
        .filter_map(|img| {
            let u = util::str_at(img, &["url_list", "0", "url"]);
            (!u.is_empty()).then(|| Image::new(u))
        })
        .collect();

    // 视频：作品自带的是备用（可能有水印）
    let mut video_url = util::str_at(item, &["video", "video_high", "url_list", "0", "url"]);

    // 作者在评论里回复的那条通常无水印。comments 可能为空，所以上面先兜了底。
    for c in util::arr_at(item, &["comments"]) {
        if util::i64_at(c, &["item", "author", "id"]) != author_id {
            continue;
        }
        let u = util::str_at(c, &["item", "video", "video_high", "url_list", "0", "url"]);
        if !u.is_empty() {
            video_url = u;
            break;
        }
    }

    VideoInfo {
        video_url: if images.is_empty() {
            video_url
        } else {
            String::new()
        },
        cover_url: util::str_at(item, &["cover", "url_list", "0", "url"]),
        title: util::str_at(item, &["content"]),
        images,
        duration: util::num_at(item, &["video", "duration"]),
        author: Author::new(
            author_id.to_string(),
            util::str_at(item, &["author", "name"]),
            util::str_at(item, &["author", "avatar", "download_list", "0", "url"]),
        ),
        ..Default::default()
    }
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_author_comment_video() {
        let item = json!({
            "content": "标题",
            "author": {"id": 42, "name": "作者", "avatar": {"download_list": [{"url": "a"}]}},
            "cover": {"url_list": [{"url": "https://c/c.jpg"}]},
            "video": {"video_high": {"url_list": [{"url": "https://v/watermarked.mp4"}]}},
            "comments": [
                {"item": {"author": {"id": 7}, "video": {"video_high": {"url_list": [{"url": "https://v/other.mp4"}]}}}},
                {"item": {"author": {"id": 42}, "video": {"video_high": {"url_list": [{"url": "https://v/clean.mp4"}]}}}}
            ]
        });
        let info = build(&item);
        assert_eq!(info.video_url, "https://v/clean.mp4");
        assert_eq!(info.author.uid, "42");
    }

    #[test]
    fn falls_back_when_no_author_comment() {
        let item = json!({
            "author": {"id": 42},
            "video": {"video_high": {"url_list": [{"url": "https://v/watermarked.mp4"}]}},
            "comments": []
        });
        assert_eq!(build(&item).video_url, "https://v/watermarked.mp4");
    }

    #[test]
    fn gallery_drops_video_url() {
        let item = json!({
            "author": {"id": 1},
            "note": {"multi_image": [{"url_list": [{"url": "https://p/1.jpg"}]}]},
            "video": {"video_high": {"url_list": [{"url": "https://v/x.mp4"}]}}
        });
        let info = build(&item);
        assert!(info.is_gallery());
        assert_eq!(info.images.len(), 1);
    }
}
