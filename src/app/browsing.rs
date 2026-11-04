// SPDX-License-Identifier: MIT
//! Read-only video discovery, discussion pages, subtitles and explicit media fetch.
use super::*;

fn page_number(params: &Value) -> Result<u32> {
    let page = u32::try_from(number(params, "page", 1)?).map_err(|_| "invalid page")?;
    if page == 0 || page > 100000 {
        return Err("page outside supported range".into());
    }
    Ok(page)
}
fn page_limit(params: &Value) -> Result<u32> {
    let limit = u32::try_from(number(params, "limit", 20)?).map_err(|_| "invalid limit")?;
    if !(1..=20).contains(&limit) {
        return Err("discussion limit must be 1..20".into());
    }
    Ok(limit)
}

pub(super) fn replace_media(
    node: &mut Value,
    assets: &mut crate::private_state::LocatorBatch<'_>,
    scope: &Scope,
    count: &mut usize,
    namespace: &str,
) -> Result<()> {
    match node {
        Value::Array(items) => {
            for item in items {
                replace_media(item, assets, scope, count, namespace)?;
            }
        }
        Value::Object(object) => {
            if let Some(media) = object.get_mut("media").and_then(Value::as_array_mut) {
                for item in media {
                    *count += 1;
                    if *count > 512 {
                        return Err(
                            "discussion media reference budget exceeded; request fewer comments"
                                .into(),
                        );
                    }
                    let url = item
                        .get("url")
                        .and_then(Value::as_str)
                        .ok_or("missing comment media URL")?
                        .to_owned();
                    platform_bilibili::validate_resource_url(&url).map_err(|e| e.to_string())?;
                    let resource = MediaResource {
                        url,
                        backup_urls: vec![],
                        expected_bytes: None,
                        mime_type: None,
                    };
                    let asset = assets.put(scope, namespace, &resource)?;
                    let item = item
                        .as_object_mut()
                        .ok_or("comment media object expected")?;
                    item.remove("url");
                    item.insert("asset".into(), json!(asset));
                    item.insert("fetched".into(), json!(false));
                    item.insert("model_observed".into(), json!(false));
                    item.insert("animation_verified".into(), json!(false));
                    item.remove("fetch_tool");
                    item.insert("media_ref".into(), json!(asset));
                    item.insert("read_tool".into(), json!("media_read"));
                }
            }
            for (key, value) in object {
                if key != "media" {
                    replace_media(value, assets, scope, count, namespace)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

impl Runtime {
    pub(super) fn call_bilibili_browse(&mut self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "bilibili.uploader_content" => self.bilibili_uploader_content(params),
            "bilibili.uploader_videos" => {
                let mid = number(params, "mid", 0)?;
                let data = self
                    .bili
                    .uploader_videos(mid, page_number(params)?, page_limit(params)?)
                    .map_err(|e| e.to_string())?;
                self.cache_bili_discovery(
                    "uploader_videos",
                    serde_json::to_value(data).map_err(|e| e.to_string())?,
                )
            }
            "bilibili.dynamics" | "bilibili.following" => {
                let mid = number(params, "mid", 0)?;
                let offset = match params.get("offset") {
                    None => None,
                    Some(v) => Some(v.as_str().ok_or("offset must be an opaque string")?),
                };
                let data = if method == "bilibili.following" {
                    self.bili.following_dynamics(offset, page_limit(params)?).map_err(|e| e.to_string())?
                } else {
                    serde_json::to_value(self.bili.uploader_dynamics(mid, offset, page_limit(params)?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
                };
                self.cache_bili_dynamic_page(data)
            }
            "bilibili.context" => {
                let id: VideoId = required_str(params, "video")?
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let info = self.bili.video_info(&id).map_err(|e| e.to_string())?;
                let tags = self.bili.video_tags(&id).map_err(|e| e.to_string())?;
                let uploader = self
                    .bili
                    .uploader_info(info.owner.id)
                    .map_err(|e| e.to_string())?;
                let scope = self.scope(Platform::Bilibili);
                let mut assets = self.locators.batch();
                let mut data =
                    json!({"info":info,"tags":tags,"uploader":uploader,"media_downloaded":false});
                for tag in data["tags"].as_array_mut().ok_or("tag list expected")? {
                    if !tag["cover"].is_null() {
                        let resource: MediaResource = serde_json::from_value(tag["cover"].take())
                            .map_err(|_| "invalid tag cover")?;
                        tag["cover"] = json!({"asset":assets.put(&scope,"bilibili-media",&resource)?,"fetched":false});
                    }
                }
                if !data["uploader"]["avatar"].is_null() {
                    let resource: MediaResource =
                        serde_json::from_value(data["uploader"]["avatar"].take())
                            .map_err(|_| "invalid uploader avatar")?;
                    data["uploader"]["avatar"] = json!({"asset":assets.put(&scope,"bilibili-media",&resource)?,"fetched":false});
                }
                let result = self.cache_account_page(Platform::Bilibili, "video_context", data)?;
                assets.commit();
                Ok(result)
            }
            "bilibili.search" => {
                let keyword = required_str(params, "keyword")?;
                let order = match params
                    .get("order")
                    .and_then(Value::as_str)
                    .unwrap_or("relevance")
                {
                    "relevance" => platform_bilibili::SearchOrder::Relevance,
                    "views" => platform_bilibili::SearchOrder::View,
                    "newest" => platform_bilibili::SearchOrder::Newest,
                    "comments" => platform_bilibili::SearchOrder::Comments,
                    _ => return Err("invalid search order".into()),
                };
                let duration = match params
                    .get("duration")
                    .and_then(Value::as_str)
                    .unwrap_or("all")
                {
                    "all" => platform_bilibili::SearchDuration::All,
                    "under_10m" => platform_bilibili::SearchDuration::UnderTenMinutes,
                    "10_30m" => platform_bilibili::SearchDuration::TenToThirtyMinutes,
                    "30_60m" => platform_bilibili::SearchDuration::ThirtyToSixtyMinutes,
                    "over_60m" => platform_bilibili::SearchDuration::OverSixtyMinutes,
                    _ => return Err("invalid search duration".into()),
                };
                let data = self
                    .bili
                    .search_videos(keyword, page_number(params)?, order, duration)
                    .map_err(|e| e.to_string())?;
                self.cache_bili_discovery(
                    "search",
                    serde_json::to_value(data).map_err(|e| e.to_string())?,
                )
            }
            "bilibili.popular" => {
                let data = self
                    .bili
                    .popular_videos(page_number(params)?, page_limit(params)?)
                    .map_err(|e| e.to_string())?;
                self.cache_bili_discovery(
                    "popular",
                    serde_json::to_value(data).map_err(|e| e.to_string())?,
                )
            }
            "bilibili.recommended" => {
                let data = self
                    .bili
                    .home_recommendations(page_number(params)?, page_limit(params)?)
                    .map_err(|e| e.to_string())?;
                self.cache_bili_discovery(
                    "recommended",
                    serde_json::to_value(data).map_err(|e| e.to_string())?,
                )
            }
            "bilibili.comments" | "bilibili.comment_replies" => {
                let id: VideoId = required_str(params, "video")?
                    .parse()
                    .map_err(|e: platform_bilibili::Error| e.to_string())?;
                let limit = page_limit(params)?;
                let data = if method == "bilibili.comments" {
                    let order = match params.get("order").and_then(Value::as_str).unwrap_or("hot") {
                        "hot" => platform_bilibili::CommentOrder::Hot,
                        "time" => platform_bilibili::CommentOrder::Time,
                        _ => return Err("comment order must be hot or time".into()),
                    };
                    let cursor = match params.get("cursor") {
                        None => None,
                        Some(v) => Some(
                            v.as_str()
                                .filter(|s| s.len() <= 8192)
                                .ok_or("cursor must be an opaque string within 8192 bytes")?,
                        ),
                    };
                    let info = self.bili.video_info(&id).map_err(|e| e.to_string())?;
                    serde_json::to_value(
                        self.bili
                            .comment_page(info.aid, order, cursor, limit)
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                } else {
                    let root = number(params, "root", 0)?;
                    if root == 0 || root > i64::MAX as u64 {
                        return Err("comment_replies requires a positive root ID".into());
                    }
                    let page = page_number(params)?;
                    let info = self.bili.video_info(&id).map_err(|e| e.to_string())?;
                    serde_json::to_value(
                        self.bili
                            .comment_replies(info.aid, root, page, limit)
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?
                };
                self.cache_bili_comments(
                    if method == "bilibili.comments" {
                        "comments"
                    } else {
                        "comment_replies"
                    },
                    data,
                )
            }
            "bilibili.comment_media" | "bilibili.dynamic_media" => {
                let asset = required_str(params, "asset")?;
                let scope = self.scope(Platform::Bilibili);
                let resource: MediaResource = self.locators.get(
                    &scope,
                    if method == "bilibili.dynamic_media" {
                        "bilibili-dynamic-media"
                    } else {
                        "bilibili-comment-media"
                    },
                    asset,
                )?;
                let work = WorkDir::new(&self.state)?;
                let path = work.path.join("comment-media");
                let max_bytes = number(params, "max_bytes", 8 * 1024 * 1024)?;
                if max_bytes == 0 || max_bytes > 16 * 1024 * 1024 {
                    return Err("comment media limit must be 1..16 MiB".into());
                }
                let receipt = self
                    .bili
                    .download_to_path(&resource, &path, max_bytes)
                    .map_err(|e| e.to_string())?;
                let mime = download_mime(&path, receipt.content_type.as_deref(), "image")?;
                if !matches!(
                    mime.as_str(),
                    "image/jpeg" | "image/png" | "image/gif" | "image/webp"
                ) {
                    return Err("downloaded comment media has no supported image signature".into());
                }
                let stored = self.import_output(&scope, "comment-media", &path, &mime)?;
                Ok(
                    json!({"asset":asset,"resource":stored,"fetched":true,"model_observed":false,
                    "animation_decoded":false,"note":"Image signature verified; use media.gif for GIF time samples or media.image for a static view."}),
                )
            }
            "bilibili.subtitles" => self.bilibili_subtitles(params),
            _ => Err("unknown Bilibili browsing tool".into()),
        }
    }

    fn bilibili_uploader_content(&mut self, params: &Value) -> Result<Value> {
        let (mid, video) = uploader_selector(params)?;
        let kind = match params.get("kind") {
            None => "all",
            Some(Value::String(kind)) if matches!(kind.as_str(), "all" | "videos" | "dynamics") => {
                kind.as_str()
            }
            _ => return Err("uploader content kind must be all, videos or dynamics".into()),
        };
        let limit = page_limit(params)?;
        let page =
            u32::try_from(number(params, "video_page", 1)?).map_err(|_| "invalid video page")?;
        if page == 0 || page > 100000 {
            return Err("invalid video page".into());
        }
        let offset = match params.get("dynamic_offset") {
            None => None,
            Some(v) => Some(v.as_str().ok_or("dynamic_offset must be opaque string")?),
        };
        let (mid, name, source) = if let Some(video) = video {
            let info = self.bili.video_info(&video).map_err(|e| e.to_string())?;
            (info.owner.id, Some(info.owner.name), Some(info.bvid))
        } else {
            (mid.ok_or("missing uploader selector")?, None, None)
        };
        let mut result = json!({"uploader_mid":mid,"uploader_name":name,"resolved_from_video":source,
            "scope":self.scope(Platform::Bilibili),"content_writes":false,"media_downloaded":false,"read_errors":{}});
        let mut successful = 0;
        if kind != "dynamics" {
            match self.call_bilibili_browse(
                "bilibili.uploader_videos",
                &json!({"mid":mid,"page":page,"limit":limit}),
            ) {
                Ok(data) => {
                    if result["uploader_name"].is_null() {
                        result["uploader_name"] = data["page"]["data"]["uploader_name"].clone();
                    }
                    result["videos"] = data;
                    successful += 1;
                }
                Err(error) => result["read_errors"]["videos"] = json!(error),
            }
        }
        if kind != "videos" {
            let mut request = json!({"mid":mid,"limit":limit});
            if let Some(offset) = offset {
                request["offset"] = json!(offset);
            }
            match self.call_bilibili_browse("bilibili.dynamics", &request) {
                Ok(data) => {
                    result["dynamics"] = data;
                    successful += 1;
                }
                Err(error) => result["read_errors"]["dynamics"] = json!(error),
            }
        }
        if successful == 0 {
            return Err(format!(
                "uploader content reads failed: {}",
                result["read_errors"]
            ));
        }
        result["status"] = json!(if result["read_errors"]
            .as_object()
            .is_some_and(|errors| errors.is_empty())
        {
            "complete"
        } else {
            "partial"
        });
        Ok(result)
    }

    fn cache_bili_discovery(&self, kind: &str, mut data: Value) -> Result<Value> {
        let scope = self.scope(Platform::Bilibili);
        let mut assets = self.locators.batch();
        for item in data["items"]
            .as_array_mut()
            .ok_or("discovery items expected")?
        {
            if !item["cover"].is_null() {
                let cover: MediaResource = serde_json::from_value(item["cover"].take())
                    .map_err(|_| "invalid discovery cover")?;
                let asset = assets.put(&scope, "bilibili-media", &cover)?;
                item["cover"] = json!({"asset":asset,"kind":"image","fetched":false});
            }
        }
        let result = self.cache_account_page(Platform::Bilibili, kind, data)?;
        assets.commit();
        Ok(result)
    }

    pub(super) fn cache_bili_comments(&self, kind: &str, mut data: Value) -> Result<Value> {
        let scope = self.scope(Platform::Bilibili);
        let mut assets = self.locators.batch();
        let mut count = 0;
        replace_media(
            &mut data,
            &mut assets,
            &scope,
            &mut count,
            "bilibili-comment-media",
        )?;
        let result = self.cache_account_page(Platform::Bilibili, kind, data)?;
        assets.commit();
        Ok(result)
    }

    pub(super) fn cache_bili_dynamic_page(&self, mut data: Value) -> Result<Value> {
        let scope = self.scope(Platform::Bilibili);
        if let Some(items) = data.get_mut("items").and_then(Value::as_array_mut) {
            for item in items {
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    let target = self.bili_dynamic_feature_ref(id)?;
                    item["target_ref"] = json!(target);
                    item["next_tools"] = json!([{"tool":"social_bilibili","action":"dynamic_open"},{"tool":"social_bilibili","action":"dynamic_discussion"},{"tool":"social_bilibili","action":"dynamic_reply"}]);
                }
            }
        }
        let mut assets = self.locators.batch();
        let mut count = 0;
        replace_media(
            &mut data,
            &mut assets,
            &scope,
            &mut count,
            "bilibili-dynamic-media",
        )?;
        let result = self.cache_account_page(Platform::Bilibili, "uploader_dynamics", data)?;
        assets.commit();
        Ok(result)
    }

    fn bilibili_subtitles(&mut self, params: &Value) -> Result<Value> {
        let video: VideoId = required_str(params, "video")?
            .parse()
            .map_err(|e: platform_bilibili::Error| e.to_string())?;
        let info = self.bili.video_info(&video).map_err(|e| e.to_string())?;
        let page = page_number(params)?;
        let part = info
            .pages
            .iter()
            .find(|p| p.page == page)
            .ok_or("video part absent")?;
        let index = self
            .bili
            .subtitles(&info, part.cid)
            .map_err(|e| e.to_string())?;
        let mut result = json!({"bvid":info.bvid,"cid":part.cid,"requires_login":index.requires_login,
            "tracks":index.tracks.iter().map(|t|json!({"id":t.id,"language":t.language,"label":t.label})).collect::<Vec<_>>(),"lines_fetched":false});
        if let Some(language) = params.get("language") {
            let language = language
                .as_str()
                .ok_or("language must be a track language or ID")?;
            let track = index
                .tracks
                .iter()
                .find(|t| t.language == language || t.id == language)
                .ok_or("requested subtitle track unavailable")?;
            let offset = usize::try_from(number(params, "offset", 0)?)
                .map_err(|_| "invalid subtitle offset")?;
            let limit = usize::try_from(number(params, "limit", 100)?)
                .map_err(|_| "invalid subtitle limit")?;
            if offset > 100000 || limit == 0 || limit > 200 {
                return Err("subtitle range limit".into());
            }
            let lines = self.bili.subtitle_lines(track).map_err(|e| e.to_string())?;
            let total = lines.len();
            let selected: Vec<_> = lines.into_iter().skip(offset).take(limit).collect();
            let end = offset + selected.len();
            result["selected_track"] = json!({"id":track.id,"language":track.language});
            result["lines"] = serde_json::to_value(selected).map_err(|e| e.to_string())?;
            result["lines_fetched"] = json!(true);
            result["total_lines"] = json!(total);
            result["next_offset"] = json!((end < total).then_some(end));
        }
        self.cache_account_page(Platform::Bilibili, "subtitles", result)
    }
}

fn uploader_selector(params: &Value) -> Result<(Option<u64>, Option<VideoId>)> {
    match (params.get("mid"), params.get("video")) {
        (Some(mid), None) => {
            let mid = mid
                .as_u64()
                .filter(|id| *id > 0 && *id <= i64::MAX as u64)
                .ok_or("mid must be a positive uploader ID")?;
            Ok((Some(mid), None))
        }
        (None, Some(video)) => {
            let video = video
                .as_str()
                .ok_or("video must be a reference string")?
                .parse()
                .map_err(|e: platform_bilibili::Error| e.to_string())?;
            Ok((None, Some(video)))
        }
        _ => Err("provide exactly one uploader selector: video or mid".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discussion_media_stays_lazy_and_account_scoped_across_restart() {
        let state = std::env::temp_dir().join(format!(
            "lazy-comments-{}-{}",
            std::process::id(),
            crate::outbox::now().unwrap()
        ));
        let runtime = Runtime::open(&state, "one", None).unwrap();
        let page=runtime.cache_bili_comments("comments",json!({"items":[{"rpid":3,"message":"a comment","media":[
            {"kind":"picture","url":"https://i0.hdslb.com/bfs/new_dyn/test.gif","format_hint":"gif","animated":null}],
            "replies":[{"rpid":4,"media":[{"kind":"emote","url":"https://i0.hdslb.com/bfs/emote/second.png"}]}]}]})).unwrap();
        let media = &page["page"]["data"]["items"][0]["media"][0];
        assert_eq!(media["fetched"], false);
        assert_eq!(media["animation_verified"], false);
        assert_eq!(media["media_ref"], media["asset"]);
        assert_eq!(media["read_tool"], "media_read");
        assert!(crate::tools::method(media["read_tool"].as_str().unwrap()).is_some());
        assert!(media.get("fetch_tool").is_none());
        assert!(!page.to_string().contains("hdslb.com"));
        let asset = media["asset"].as_str().unwrap().to_owned();
        assert_eq!(
            runtime
                .store
                .list(&runtime.scope(Platform::Bilibili), 0, 100)
                .unwrap()
                .len(),
            1
        );
        drop(runtime);
        let mut other = Runtime::open(&state, "two", None).unwrap();
        assert!(
            other
                .call("bilibili.comment_media", &json!({"asset":asset}))
                .is_err()
        );
        drop(other);
        let owner = Runtime::open(&state, "one", None).unwrap();
        let saved: MediaResource = owner
            .locators
            .get(
                &owner.scope(Platform::Bilibili),
                "bilibili-comment-media",
                &asset,
            )
            .unwrap();
        assert!(saved.url.ends_with("test.gif"));
        drop(owner);
        fs::remove_dir_all(state).unwrap();
    }
}
