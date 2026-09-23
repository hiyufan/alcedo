//! 一次请求就能拿到结果的平台。
//!
//! 这些平台的解析逻辑都是"拼个接口地址 → 取 JSON → 映射字段"，各写一个文件
//! 只会让人来回跳。放一起反而看得清楚：哪个平台用什么接口、错误码怎么判，
//! 一屏之内全在。

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{ua, Http, Req};
use crate::model::{Author, VideoInfo};
use crate::util;

/// 接口返回了业务错误码时统一报错。
fn check_code(json: &Value, code_path: &[&str], ok: i64, msg_paths: &[&[&str]]) -> Result<()> {
    let code = util::i64_at(json, code_path);
    if code == ok {
        return Ok(());
    }
    let msg = util::first_str(json, msg_paths);
    Err(Error::restricted(if msg.is_empty() {
        format!("接口返回 code={code}")
    } else {
        msg
    }))
}

// ---------------------------------------------------------------- 微视

pub async fn weishi(http: &Http, url: &str) -> Result<VideoInfo> {
    weishi_id(http, &util::query_param(url, "id")?).await
}

pub async fn weishi_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let api = format!("https://h5.weishi.qq.com/webapp/json/weishi/WSH5GetPlayPage?feedid={id}");
    let json = http.get_json(&api).await?;
    check_code(&json, &["ret"], 0, &[&["msg"]])?;

    let errmsg = util::str_at(&json, &["data", "errmsg"]);
    if !errmsg.is_empty() {
        return Err(Error::restricted(errmsg));
    }

    let feed = util::arr_at(&json, &["data", "feeds"])
        .first()
        .ok_or_else(|| Error::deleted("微视没有返回作品"))?;

    Ok(VideoInfo {
        video_url: util::str_at(feed, &["video_url"]),
        cover_url: util::str_at(feed, &["images", "0", "url"]),
        title: util::str_at(feed, &["feed_desc_withat"]),
        author: Author::new(
            util::id_at(feed, &["id"]),
            util::str_at(feed, &["poster", "nick"]),
            util::str_at(feed, &["poster", "avatar"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 最右

pub async fn zuiyou(http: &Http, url: &str) -> Result<VideoInfo> {
    zuiyou_id(http, &util::query_param(url, "pid")?).await
}

pub async fn zuiyou_id(http: &Http, id: &str) -> Result<VideoInfo> {
    // pid 必须是数字, 服务端按 int 解析
    let pid: u64 = id
        .parse()
        .map_err(|_| Error::unsupported(format!("最右的 pid 必须是数字: {id}")))?;

    let body = serde_json::json!({ "h_av": "5.2.13.011", "pid": pid });
    let resp = http
        .send(
            Req::post("https://share.xiaochuankeji.cn/planck/share/post/detail_h5")
                .json_body(&body),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let post =
        util::get(&json, &["data", "post"]).ok_or_else(|| Error::deleted("最右没有返回内容"))?;
    let key = util::str_at(post, &["imgs", "0", "id"]);
    let key = if key.is_empty() {
        util::i64_at(post, &["imgs", "0", "id"]).to_string()
    } else {
        key
    };

    Ok(VideoInfo {
        video_url: util::str_at(post, &["videos", &key, "url"]),
        title: util::str_at(post, &["content"]),
        author: Author::new(
            util::id_at(post, &["member", "id"]),
            util::str_at(post, &["member", "name"]),
            util::str_at(post, &["member", "avatar_urls", "origin", "urls", "0"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 度小视

pub async fn quanmin(http: &Http, url: &str) -> Result<VideoInfo> {
    quanmin_id(http, &util::query_param(url, "vid")?).await
}

pub async fn quanmin_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let api = format!(
        "https://quanmin.hao222.com/wise/growth/api/sv/immerse\
         ?source=share-h5&pd=qm_share_mvideo&_format=json&vid={id}"
    );
    let json = http.get_json(&api).await?;
    check_code(&json, &["errno"], 0, &[&["error"]])?;

    let data = util::get(&json, &["data"]).ok_or_else(|| Error::deleted("没有返回数据"))?;

    let status = util::str_at(data, &["meta", "statusText"]);
    if !status.is_empty() {
        return Err(Error::restricted(status));
    }

    let title = util::first_str(data, &[&["meta", "title"], &["shareInfo", "title"]]);
    // clarityUrl 是按清晰度升序排的，[1] 是平台自己给分享页选的那一档
    let clarity = util::arr_at(data, &["meta", "video_info", "clarityUrl"]);
    let video_url = clarity
        .get(1)
        .or_else(|| clarity.last())
        .map(|v| util::str_at(v, &["url"]))
        .unwrap_or_default();

    Ok(VideoInfo {
        video_url,
        cover_url: util::str_at(data, &["meta", "image"]),
        title,
        author: Author::new(
            util::id_at(data, &["author", "id"]),
            util::str_at(data, &["author", "name"]),
            util::str_at(data, &["author", "icon"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 皮皮搞笑

pub async fn pipigaoxiao(http: &Http, url: &str) -> Result<VideoInfo> {
    let parsed = url::Url::parse(url)?;
    let id = parsed
        .path()
        .trim_start_matches("/pp/post/")
        .trim_matches('/')
        .to_owned();
    if id.is_empty() {
        return Err(Error::unsupported("链接里没有作品 ID"));
    }
    pipigaoxiao_id(http, &id).await
}

pub async fn pipigaoxiao_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let pid: u64 = id
        .parse()
        .map_err(|_| Error::unsupported(format!("皮皮搞笑的 pid 必须是数字: {id}")))?;

    let api = "https://share.ippzone.com/ppapi/share/fetch_content";
    let resp = http
        .send(
            Req::post(api)
                .referer(api)
                .header("Content-Type", "text/plain;charset=UTF-8")
                .header("User-Agent", ua::pick(ua::Platform::Desktop))
                .body(format!(r#"{{"pid":{pid},"type":"post","mid":null}}"#)),
        )
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    if let Some(msg) = util::get(&json, &["msg"]).and_then(Value::as_str) {
        return Err(Error::restricted(msg.to_owned()));
    }

    let post = util::get(&json, &["data", "post"])
        .ok_or_else(|| Error::deleted("皮皮搞笑没有返回内容"))?;
    let img_id = util::i64_at(post, &["imgs", "0", "id"]);

    Ok(VideoInfo {
        video_url: util::str_at(post, &["videos", &img_id.to_string(), "url"]),
        cover_url: format!("https://file.ippzone.com/img/view/id/{img_id}"),
        title: util::str_at(post, &["content"]),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 逗拍

pub async fn doupai(http: &Http, url: &str) -> Result<VideoInfo> {
    doupai_id(http, &util::query_param(url, "id")?).await
}

pub async fn doupai_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let json = http
        .get_json(&format!("https://v2.doupai.cc/topic/{id}.json"))
        .await?;
    let data = util::get(&json, &["data"]).ok_or_else(|| Error::deleted("逗拍没有返回数据"))?;

    Ok(VideoInfo {
        video_url: util::str_at(data, &["videoUrl"]),
        cover_url: util::str_at(data, &["imageUrl"]),
        title: util::str_at(data, &["name"]),
        author: Author::new(
            util::id_at(data, &["userId", "id"]),
            util::str_at(data, &["userId", "name"]),
            util::str_at(data, &["userId", "avatar"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 全民K歌

pub async fn quanminkge(http: &Http, url: &str) -> Result<VideoInfo> {
    quanminkge_id(http, &util::query_param(url, "s")?).await
}

pub async fn quanminkge_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let page = format!("https://kg.qq.com/node/play?s={id}");
    let resp = http
        .send(Req::get(&page).header("User-Agent", ua::pick(ua::Platform::Desktop)))
        .await?;
    resp.error_for_status()?;
    let html = resp.text();

    let raw = util::json_after(&html, "window.__DATA__")
        .ok_or_else(|| Error::parse("页面里没有 window.__DATA__"))?;
    let json: Value = serde_json::from_str(util::undefined_to_null(raw).as_ref())?;

    let detail =
        util::get(&json, &["detail"]).ok_or_else(|| Error::deleted("__DATA__ 里没有 detail"))?;

    Ok(VideoInfo {
        video_url: util::str_at(detail, &["playurl_video"]),
        cover_url: util::str_at(detail, &["cover"]),
        title: util::str_at(detail, &["content"]),
        author: Author::new(
            util::id_at(detail, &["uid"]),
            util::str_at(detail, &["nick"]),
            util::str_at(detail, &["avatar"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 六间房

pub async fn sixroom(http: &Http, url: &str) -> Result<VideoInfo> {
    let id = if url.contains("watchMini.php") {
        util::query_param(url, "vid")?
    } else {
        util::last_path_segment(url).ok_or_else(|| Error::unsupported("链接里没有作品 ID"))?
    };
    sixroom_id(http, &id).await
}

pub async fn sixroom_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let api = format!(
        "https://v.6.cn/coop/mobile/index.php?padapi=minivideo-watchVideo.php\
         &av=3.0&encpass=&logiuid=&isnew=1&from=0&vid={id}"
    );
    let resp = http
        .send(Req::get(&api).referer(format!("https://m.6.cn/v/{id}")))
        .await?;
    resp.error_for_status()?;
    let json = resp.json()?;

    let content =
        util::get(&json, &["content"]).ok_or_else(|| Error::deleted("六间房没有返回内容"))?;

    Ok(VideoInfo {
        video_url: util::str_at(content, &["playurl"]),
        cover_url: util::str_at(content, &["picurl"]),
        title: util::str_at(content, &["title"]),
        author: Author::new(
            "",
            util::str_at(content, &["alias"]),
            util::str_at(content, &["picuser"]),
        ),
        ..Default::default()
    })
}

// ---------------------------------------------------------------- 好看视频

pub async fn haokan(http: &Http, url: &str) -> Result<VideoInfo> {
    haokan_id(http, &util::query_param(url, "vid")?).await
}

pub async fn haokan_id(http: &Http, id: &str) -> Result<VideoInfo> {
    let json = http
        .get_json(&format!("https://haokan.baidu.com/v?_format=json&vid={id}"))
        .await?;
    check_code(&json, &["errno"], 0, &[&["error"]])?;

    let meta = util::get(&json, &["data", "apiData", "curVideoMeta"])
        .ok_or_else(|| Error::deleted("好看视频没有返回作品"))?;

    Ok(VideoInfo {
        video_url: util::str_at(meta, &["playurl"]),
        cover_url: util::str_at(meta, &["poster"]),
        title: util::str_at(meta, &["title"]),
        duration: util::num_at(meta, &["duration"]),
        author: Author::new(
            util::id_at(meta, &["mth", "mthid"]),
            util::str_at(meta, &["mth", "author_name"]),
            util::str_at(meta, &["mth", "author_photo"]),
        ),
        ..Default::default()
    })
}

#[cfg(test)]
// 断言里比较确切的期望值是对的，浮点相等在这儿不是隐患
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn business_error_code_becomes_restricted() {
        let json = json!({"errno": 5, "error": "视频已下架"});
        let err = check_code(&json, &["errno"], 0, &[&["error"]]).unwrap_err();
        assert_eq!(err.reason, crate::Reason::Restricted);
        assert!(err.detail.contains("已下架"));
    }

    #[test]
    fn ok_code_passes() {
        assert!(check_code(&json!({"ret": 0}), &["ret"], 0, &[]).is_ok());
    }

    #[test]
    fn missing_message_falls_back_to_code() {
        let err = check_code(&json!({"ret": 7}), &["ret"], 0, &[&["msg"]]).unwrap_err();
        assert!(err.detail.contains("code=7"));
    }
}
