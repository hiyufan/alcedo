//! Twitch。
//!
//! 两种内容：
//! - **Clip**：GQL 直接给 mp4 直链，最省事
//! - **VOD**：要先用 GQL 换一张播放令牌，再拿它去 usher 取 m3u8
//!
//! 用的是 Twitch 网页版自己那个公开 client-id，不需要注册应用。

use serde_json::json;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, Format, VideoInfo};
use crate::util;

const GQL: &str = "https://gql.twitch.tv/gql";
/// 网页版 Twitch 内置的公开 client-id。
const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";

/// 解析一条Twitch分享链接。
pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo> {
    match target(url)? {
        Target::Clip(slug) => clip(http, &slug).await,
        Target::Vod(id) => vod(http, &id).await,
    }
}

#[derive(Debug)]
enum Target {
    Clip(String),
    Vod(String),
}

fn target(url: &str) -> Result<Target> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("");
    let segs: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    // clips.twitch.tv/<slug>
    if host.starts_with("clips.") {
        return segs
            .first()
            .map(|s| Target::Clip((*s).to_owned()))
            .ok_or_else(|| Error::unsupported("clips 链接里没有 slug"));
    }
    // twitch.tv/<channel>/clip/<slug>
    if let Some(i) = segs.iter().position(|s| *s == "clip") {
        return segs
            .get(i + 1)
            .map(|s| Target::Clip((*s).to_owned()))
            .ok_or_else(|| Error::unsupported("clip 链接里没有 slug"));
    }
    // twitch.tv/videos/<id>
    if let Some(i) = segs.iter().position(|s| *s == "videos" || *s == "video") {
        if let Some(id) = segs
            .get(i + 1)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Ok(Target::Vod((*id).to_owned()));
        }
    }
    Err(Error::unsupported(
        "只支持 Twitch 的 Clip 和录像（VOD）链接，直播间地址不行",
    ))
}

async fn gql(http: &Http, body: serde_json::Value) -> Result<serde_json::Value> {
    let resp = http
        .send(
            Req::post(GQL)
                .header("Client-ID", CLIENT_ID)
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .header("Accept", "application/json")
                .json_body(&body),
        )
        .await?;
    resp.error_for_status()?;
    resp.json()
}

async fn clip(http: &Http, slug: &str) -> Result<VideoInfo> {
    let query = format!(
        r#"{{ clip(slug: "{slug}") {{ title durationSeconds thumbnailURL
             broadcaster {{ id displayName profileImageURL(width: 150) }}
             videoQualities {{ quality frameRate sourceURL }} }} }}"#
    );
    let resp = gql(http, json!({ "query": query })).await?;

    let clip = util::get(&resp, &["data", "clip"])
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::deleted("这个 Clip 不存在或已被删除"))?;

    let mut qualities: Vec<&serde_json::Value> =
        util::arr_at(clip, &["videoQualities"]).iter().collect();
    qualities.sort_by_key(|q| {
        std::cmp::Reverse(util::str_at(q, &["quality"]).parse::<u32>().unwrap_or(0))
    });

    let best = qualities
        .first()
        .ok_or_else(|| Error::restricted("Twitch 没有给出这个 Clip 的下载地址"))?;

    Ok(VideoInfo {
        video_url: util::str_at(best, &["sourceURL"]),
        cover_url: util::str_at(clip, &["thumbnailURL"]),
        title: util::str_at(clip, &["title"]),
        duration: util::num_at(clip, &["durationSeconds"]),
        height: util::str_at(best, &["quality"]).parse().unwrap_or(0),
        formats: qualities
            .iter()
            .skip(1)
            .filter_map(|q| to_format(q))
            .collect(),
        author: Author::new(
            util::id_at(clip, &["broadcaster", "id"]),
            util::str_at(clip, &["broadcaster", "displayName"]),
            util::str_at(clip, &["broadcaster", "profileImageURL"]),
        ),
        ..Default::default()
    })
}

/// 一条 `videoQualities` 记录 → 一档清晰度。`quality` 是 `"1080"` 这样的字符串。
fn to_format(q: &serde_json::Value) -> Option<Format> {
    let url = util::str_at(q, &["sourceURL"]);
    if url.is_empty() {
        return None;
    }
    let height: u32 = util::str_at(q, &["quality"]).parse().unwrap_or(0);
    Some(Format::direct(
        Format::label_for("", height, ""),
        url,
        height,
    ))
}

async fn vod(http: &Http, id: &str) -> Result<VideoInfo> {
    // 元信息和播放令牌都只依赖已知的录像 id，互不依赖，一起发。
    // GQL 一个来回常要 200~500ms，串行等于每次解析白等一个 RTT。
    // 代价是录像不存在时会多发一次令牌请求——错误路径比成功路径少得多，划算。
    let meta_query = format!(
        r#"{{ video(id: "{id}") {{ title lengthSeconds previewThumbnailURL
             owner {{ id displayName profileImageURL(width: 150) }} }} }}"#
    );
    let token_query = format!(
        r#"{{ videoPlaybackAccessToken(id: "{id}", params: {{
             platform: "web", playerBackend: "mediaplayer", playerType: "site" }})
             {{ value signature }} }}"#
    );
    let (meta, token) = tokio::join!(
        gql(http, json!({ "query": meta_query })),
        gql(http, json!({ "query": token_query })),
    );

    // 两边都失败时报 meta 的错："录像不存在"比"拿不到令牌"更接近真实原因
    let meta_value = match meta {
        Ok(m) => m,
        Err(e) => return Err(e),
    };
    let video = util::get(&meta_value, &["data", "video"])
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::deleted("这个录像不存在、已过期或仅订阅者可见"))?;
    let token = token?;
    let value = util::str_at(&token, &["data", "videoPlaybackAccessToken", "value"]);
    let sig = util::str_at(&token, &["data", "videoPlaybackAccessToken", "signature"]);
    if value.is_empty() || sig.is_empty() {
        return Err(Error::restricted(
            "拿不到播放令牌，这个录像可能仅订阅者可见",
        ));
    }

    let encoded: String =
        percent_encoding::utf8_percent_encode(&value, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    let playlist = format!(
        "https://usher.ttvnw.net/vod/{id}.m3u8?allow_source=true&allow_audio_only=true\
         &player=twitchweb&token={encoded}&sig={sig}"
    );

    Ok(VideoInfo {
        video_url: playlist,
        cover_url: util::str_at(video, &["previewThumbnailURL"]),
        title: util::str_at(video, &["title"]),
        duration: util::num_at(video, &["lengthSeconds"]),
        author: Author::new(
            util::id_at(video, &["owner", "id"]),
            util::str_at(video, &["owner", "displayName"]),
            util::str_at(video, &["owner", "profileImageURL"]),
        ),
        ..Default::default()
    })
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn recognises_clip_and_vod_urls() {
        assert!(matches!(
            target("https://clips.twitch.tv/FunnySlugName").unwrap(),
            Target::Clip(s) if s == "FunnySlugName"
        ));
        assert!(matches!(
            target("https://www.twitch.tv/someone/clip/FunnySlugName?t=1").unwrap(),
            Target::Clip(s) if s == "FunnySlugName"
        ));
        assert!(matches!(
            target("https://www.twitch.tv/videos/1234567890").unwrap(),
            Target::Vod(s) if s == "1234567890"
        ));
    }

    #[test]
    fn live_channel_urls_are_rejected_with_a_clear_reason() {
        let err = target("https://www.twitch.tv/somestreamer").unwrap_err();
        assert_eq!(err.reason, crate::Reason::Unsupported);
        assert!(err.detail.contains("直播间"));
    }
}
