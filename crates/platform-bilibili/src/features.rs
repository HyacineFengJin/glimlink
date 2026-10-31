// SPDX-License-Identifier: MIT
//! Bounded social operations and collections. Fixed native endpoints only.
//! Field provenance: pskdje/bilibili-API-collect docs/{video,danmaku,dynamic,fav}.
use crate::{
    Client, DynamicItem, Error, Result, UploadedCommentImage, VideoId, api_data,
    comments::csrf_from_cookie,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeatureCommand {
    Favorite {
        video: String,
        folder_id: u64,
        add: bool,
    },
    Coin {
        video: String,
        count: u32,
    },
    Danmaku {
        video: String,
        page: u32,
        progress_ms: u64,
        text: String,
        #[serde(default = "white")]
        color: u32,
        #[serde(default = "scroll")]
        mode: u32,
    },
    DynamicCreate {
        text: String,
        #[serde(default)]
        pictures: Vec<UploadedCommentImage>,
    },
    DynamicLike {
        dynamic_id: String,
        like: bool,
    },
    DynamicReply {
        dynamic_id: String,
        message: String,
        #[serde(default)]
        root: u64,
        #[serde(default)]
        parent: u64,
    },
}
fn white() -> u32 {
    0xffffff
}
fn scroll() -> u32 {
    1
}
pub struct PreparedFeature {
    command: FeatureCommand,
    actor: u64,
    target: Value,
    before: Value,
    cookie: String,
    csrf: String,
}
/// Account-bound dynamic image preparation. Bytes and cookie remain private.
pub struct PreparedDynamicImage {
    actor: u64,
    cookie: String,
    csrf: String,
    body: Vec<u8>,
    content_type: String,
    width: u32,
    height: u32,
}
impl PreparedDynamicImage {
    pub fn actor_mid(&self) -> u64 {
        self.actor
    }
}
impl PreparedFeature {
    pub fn actor_mid(&self) -> u64 {
        self.actor
    }
    pub fn target(&self) -> &Value {
        &self.target
    }
    pub fn command(&self) -> &FeatureCommand {
        &self.command
    }
    pub fn state_before(&self) -> &Value {
        &self.before
    }
}
fn id(n: u64) -> Result<()> {
    if n == 0 || n > i64::MAX as u64 {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn dynamic_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 24 || !s.bytes().all(|c| c.is_ascii_digit()) {
        Err(Error::InvalidId)
    } else {
        Ok(())
    }
}
fn text(s: &str, limit: usize) -> Result<()> {
    if s.trim().is_empty() || s.len() > limit || s.contains('\0') {
        Err(Error::InvalidComment)
    } else {
        Ok(())
    }
}
impl FeatureCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Favorite { .. } => "favorite",
            Self::Coin { .. } => "coin",
            Self::Danmaku { .. } => "danmaku",
            Self::DynamicCreate { .. } => "dynamic_create",
            Self::DynamicLike { .. } => "dynamic_like",
            Self::DynamicReply { .. } => "dynamic_reply",
        }
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Favorite {
                video, folder_id, ..
            } => {
                video.parse::<VideoId>()?;
                id(*folder_id)?;
            }
            Self::Coin { video, count } => {
                video.parse::<VideoId>()?;
                if !(1..=2).contains(count) {
                    return Err(Error::InvalidComment);
                }
            }
            Self::Danmaku {
                video,
                page,
                progress_ms,
                text: t,
                color,
                mode,
            } => {
                video.parse::<VideoId>()?;
                text(t, 1000)?;
                if *page == 0
                    || *page > 2048
                    || *progress_ms > 86_400_000
                    || *color > 0xffffff
                    || !matches!(*mode, 1 | 4 | 5)
                {
                    return Err(Error::InvalidComment);
                }
            }
            Self::DynamicCreate { text: t, pictures } => {
                if t.len() > 16384
                    || t.contains('\0')
                    || (t.trim().is_empty() && pictures.is_empty())
                {
                    return Err(Error::InvalidComment);
                }
                if pictures.len() > 9 {
                    return Err(Error::InvalidComment);
                }
                for p in pictures {
                    p.picture().validate()?;
                }
            }
            Self::DynamicLike { dynamic_id: d, .. } => dynamic_id(d)?,
            Self::DynamicReply {
                dynamic_id: d,
                message,
                root,
                parent,
            } => {
                dynamic_id(d)?;
                text(message, 3000)?;
                if (*root == 0) != (*parent == 0) {
                    return Err(Error::InvalidComment);
                }
            }
        }
        Ok(())
    }
}
fn form(items: &[(&str, String)]) -> Vec<u8> {
    items
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn result(v: Value) -> Result<Value> {
    let code = v["code"]
        .as_i64()
        .ok_or(Error::WriteOutcomeUnknown("missing_result_code"))?;
    if code != 0 {
        return Err(match code {
            -101 => Error::LoginRequired,
            -352 | -412 | -401 | -509 | 12015 => Error::RiskControl(code),
            _ => Error::Platform(code),
        });
    }
    Ok(v.get("data").cloned().unwrap_or(json!({})))
}
fn request(p: &PreparedFeature) -> Result<(&'static str, Vec<u8>, bool)> {
    let csrf = p.csrf.clone();
    Ok(match &p.command {
        FeatureCommand::Favorite{folder_id,add,..}=>("/x/v3/fav/resource/deal",form(&[("rid",p.target["aid"].to_string()),("type","2".into()),("add_media_ids",if *add{folder_id.to_string()}else{String::new()}),("del_media_ids",if *add{String::new()}else{folder_id.to_string()}),("csrf",csrf)]),false),
        FeatureCommand::Coin{count,..}=>("/x/web-interface/coin/add",form(&[("aid",p.target["aid"].to_string()),("multiply",count.to_string()),("select_like","0".into()),("csrf",csrf)]),false),
        FeatureCommand::Danmaku{progress_ms,text,color,mode,..}=>("/x/v2/dm/post",form(&[("type","1".into()),("oid",p.target["cid"].to_string()),("aid",p.target["aid"].to_string()),("progress",progress_ms.to_string()),("msg",text.clone()),("color",color.to_string()),("fontsize","25".into()),("pool","0".into()),("mode",mode.to_string()),("rnd",std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|Error::InvalidConfig)?.as_micros().to_string()),("csrf",csrf)]),false),
        FeatureCommand::DynamicCreate{text,pictures}=>("/x/dynamic/feed/create/dyn",serde_json::to_vec(&json!({"dyn_req":{"scene":if pictures.is_empty(){1}else{2},"content":{"contents":[{"raw_text":text,"type":1,"biz_id":""}]},"pics":pictures.iter().map(|p|p.picture()).collect::<Vec<_>>(),"meta":{"app_meta":{"from":"create.dynamic.web","mobi_app":"web"}}}})).map_err(|_|Error::InvalidComment)?,true),
        FeatureCommand::DynamicLike{dynamic_id,like}=>("/x/dynamic/feed/dyn/thumb",serde_json::to_vec(&json!({"dyn_id_str":dynamic_id,"up":if *like{1}else{2}})).map_err(|_|Error::InvalidComment)?,true),
        FeatureCommand::DynamicReply{message,root,parent,..}=>("/x/v2/reply/add",form(&[("oid",p.target["comment_oid"].to_string()),("type",p.target["comment_type"].to_string()),("message",message.clone()),("root",root.to_string()),("parent",parent.to_string()),("plat","1".into()),("csrf",csrf)]),false),
    })
}
impl Client {
    pub fn prepare_dynamic_image(&self, bytes: &[u8]) -> Result<PreparedDynamicImage> {
        let (mime, width, height) = crate::comment_images::image_info(bytes)?;
        let cookie = self
            .transport
            .cookie_header()
            .ok_or(Error::LoginRequired)?
            .to_owned();
        let csrf = csrf_from_cookie(&cookie)?;
        let actor = self.current_user()?.mid;
        let (content_type, body) = crate::comment_images::multipart(bytes, mime, &csrf)?;
        Ok(PreparedDynamicImage {
            actor,
            cookie,
            csrf,
            body,
            content_type,
            width,
            height,
        })
    }
    pub fn submit_dynamic_image(
        &self,
        p: PreparedDynamicImage,
    ) -> Result<crate::ImageUploadReceipt> {
        if self.transport.cookie_header() != Some(p.cookie.as_str()) {
            return Err(Error::CommentPreparationChanged);
        }
        let data = self.transport.post_feature_payload(
            "/x/dynamic/feed/draw/upload_bfs",
            &p.csrf,
            &p.body,
            &p.content_type,
            false,
        )?;
        crate::comment_images::parse_upload_result(data, p.width, p.height, p.actor)
    }
    pub fn favorite_contents(&self, folder: u64, page: u32, limit: u32) -> Result<Value> {
        id(folder)?;
        if page == 0 || page > 100000 || !(1..=20).contains(&limit) {
            return Err(Error::InvalidPage);
        }
        let v=api_data(self.transport.json(&format!("https://api.bilibili.com/x/v3/fav/resource/list?media_id={folder}&pn={page}&ps={limit}&order=mtime&type=0&tid=0"))?,false)?;
        let info = v
            .get("info")
            .cloned()
            .ok_or(Error::Schema("favorite folder info"))?;
        if info["id"].as_u64() != Some(folder) {
            return Err(Error::Schema("favorite folder identity"));
        }
        let items = match v.get("medias") {
            Some(Value::Array(a)) if a.len() <= limit as usize => a.clone(),
            Some(Value::Null) => vec![],
            _ => return Err(Error::Schema("favorite items")),
        };
        Ok(
            json!({"folder":info,"items":items,"page":page,"has_more":v["has_more"].as_bool(),"next_page":if v["has_more"]==true{Some(page+1)}else{None}}),
        )
    }
    pub fn video_folders(&self, video: &VideoId, actor: u64) -> Result<Value> {
        id(actor)?;
        let video = self.video_info(video)?;
        api_data(self.transport.json(&format!("https://api.bilibili.com/x/v3/fav/folder/created/list-all?up_mid={actor}&type=2&rid={}",video.aid))?,false)
    }
    pub fn video_coins(&self, video: &VideoId) -> Result<u64> {
        let v = self.video_info(video)?;
        let data = api_data(
            self.transport.json(&format!(
                "https://api.bilibili.com/x/web-interface/archive/coins?aid={}",
                v.aid
            ))?,
            false,
        )?;
        data["multiply"].as_u64().ok_or(Error::Schema("coin count"))
    }
    pub fn dynamic_detail(&self, id: &str) -> Result<DynamicItem> {
        dynamic_id(id)?;
        let data = api_data(
            self.transport.json(&format!(
                "https://api.bilibili.com/x/polymer/web-dynamic/v1/detail?id={id}"
            ))?,
            false,
        )?;
        let item = crate::dynamics::parse_item(
            data.get("item").ok_or(Error::Schema("dynamic detail"))?,
            0,
        )?;
        if item.id != id {
            return Err(Error::Schema("dynamic identity"));
        }
        Ok(item)
    }
    pub fn dynamic_discussion(&self, id: &str, root: u64, page: u32, limit: u32) -> Result<Value> {
        if page == 0 || page > 100000 || !(1..=20).contains(&limit) {
            return Err(Error::InvalidPage);
        }
        let d = self.dynamic_detail(id)?;
        let oid = d
            .comment_oid
            .ok_or(Error::Schema("dynamic comment object"))?;
        let kind = d
            .comment_type
            .ok_or(Error::Schema("dynamic comment type"))?;
        let url = if root == 0 {
            format!(
                "https://api.bilibili.com/x/v2/reply/main?type={kind}&oid={oid}&mode=3&next={}&ps={limit}",
                page - 1
            )
        } else {
            format!(
                "https://api.bilibili.com/x/v2/reply/reply?type={kind}&oid={oid}&root={root}&pn={page}&ps={limit}"
            )
        };
        api_data(self.transport.json(&url)?, false)
    }
    /// Identity preflight for a publication whose cached images have not yet
    /// been uploaded. submit_feature revalidates the final command, so an empty
    /// text-only reservation cannot accidentally publish an empty dynamic.
    pub fn prepare_dynamic_publication(
        &self,
        text: &str,
        pending_images: usize,
    ) -> Result<PreparedFeature> {
        if !(1..=9).contains(&pending_images) || text.len() > 16384 || text.contains('\0') {
            return Err(Error::InvalidComment);
        }
        self.prepare_feature_inner(FeatureCommand::DynamicCreate {
            text: text.into(),
            pictures: vec![],
        })
    }
    pub fn prepare_feature(&self, command: FeatureCommand) -> Result<PreparedFeature> {
        command.validate()?;
        self.prepare_feature_inner(command)
    }
    fn prepare_feature_inner(&self, command: FeatureCommand) -> Result<PreparedFeature> {
        let cookie = self
            .transport
            .cookie_header()
            .ok_or(Error::LoginRequired)?
            .to_owned();
        let csrf = csrf_from_cookie(&cookie)?;
        let actor = self.current_user()?.mid;
        let (target, before) = match &command {
            FeatureCommand::Favorite {
                video, folder_id, ..
            } => {
                let video: VideoId = video.parse()?;
                let info = self.video_info(&video)?;
                let folders = self.video_folders(&video, actor)?;
                let f = folders["list"]
                    .as_array()
                    .and_then(|a| a.iter().find(|f| f["id"].as_u64() == Some(*folder_id)))
                    .ok_or(Error::Schema(
                        "favorite folder is not owned by current actor",
                    ))?;
                (
                    json!({"aid":info.aid,"bvid":info.bvid,"folder_id":folder_id}),
                    json!({"favorited":f["fav_state"].as_u64().map(|n|n!=0)}),
                )
            }
            FeatureCommand::Coin { video, .. } => {
                let id: VideoId = video.parse()?;
                let v = self.video_info(&id)?;
                (
                    json!({"aid":v.aid,"bvid":v.bvid}),
                    json!({"coins":self.video_coins(&id)?}),
                )
            }
            FeatureCommand::Danmaku {
                video,
                page,
                progress_ms,
                ..
            } => {
                let v = self.video_info(&video.parse()?)?;
                let part = v
                    .pages
                    .iter()
                    .find(|p| p.page == *page)
                    .ok_or(Error::InvalidPage)?;
                if *progress_ms > part.duration_seconds.saturating_mul(1000) {
                    return Err(Error::InvalidPage);
                }
                (
                    json!({"aid":v.aid,"bvid":v.bvid,"cid":part.cid,"page":page}),
                    json!({}),
                )
            }
            FeatureCommand::DynamicCreate { pictures, .. } => {
                if pictures.iter().any(|p| p.actor_mid() != actor) {
                    return Err(Error::CommentPreparationChanged);
                }
                (
                    json!({"actor_mid":actor,"kind":"dynamic_create"}),
                    json!({}),
                )
            }
            FeatureCommand::DynamicLike { dynamic_id, .. }
            | FeatureCommand::DynamicReply { dynamic_id, .. } => {
                let d = self.dynamic_detail(dynamic_id)?;
                if matches!(command, FeatureCommand::DynamicReply { .. })
                    && (d.comment_oid.is_none() || d.comment_type.is_none())
                {
                    return Err(Error::Schema("dynamic has no comment target"));
                }
                (
                    json!({"dynamic_id":d.id,"author_mid":d.author.mid,"comment_oid":d.comment_oid,"comment_type":d.comment_type}),
                    json!({"liked":d.stat.liked}),
                )
            }
        };
        Ok(PreparedFeature {
            command,
            actor,
            target,
            before,
            cookie,
            csrf,
        })
    }
    pub fn submit_feature(&self, p: PreparedFeature) -> Result<Value> {
        p.command.validate()?;
        if self.transport.cookie_header() != Some(p.cookie.as_str()) {
            return Err(Error::CommentPreparationChanged);
        }
        let (path, body, json_body) = request(&p)?;
        let mut data = result(
            self.transport
                .post_feature(path, &p.csrf, &body, json_body)?,
        )?;
        if matches!(p.command, FeatureCommand::DynamicCreate { .. }) {
            let id = data["dyn_id_str"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| data["dyn_id"].as_u64().map(|n| n.to_string()))
                .ok_or(Error::WriteOutcomeUnknown("dynamic_id"))?;
            dynamic_id(&id).map_err(|_| Error::WriteOutcomeUnknown("dynamic_id"))?;
            data["dynamic_id"] = json!(id);
        }
        Ok(data)
    }
    pub fn feature_readback(
        &self,
        command: &FeatureCommand,
        target: &Value,
        receipt: &Value,
        actor: u64,
        before: &Value,
    ) -> Result<Value> {
        let verified = match command {
            FeatureCommand::Favorite {
                video,
                folder_id,
                add,
            } => {
                let v = self.video_folders(&video.parse()?, actor)?;
                v["list"]
                    .as_array()
                    .and_then(|a| a.iter().find(|f| f["id"].as_u64() == Some(*folder_id)))
                    .and_then(|f| f["fav_state"].as_u64())
                    .is_some_and(|s| (s != 0) == *add)
            }
            FeatureCommand::Coin { video, count } => before["coins"].as_u64().is_some_and(|old| {
                self.video_coins(&video.parse::<VideoId>().unwrap())
                    .is_ok_and(|n| n == old + u64::from(*count))
            }),
            FeatureCommand::DynamicLike { dynamic_id, like } => {
                self.dynamic_detail(dynamic_id)?.stat.liked == Some(*like)
            }
            FeatureCommand::DynamicCreate { text, pictures } => {
                let d = self.dynamic_detail(
                    receipt["dynamic_id"]
                        .as_str()
                        .ok_or(Error::Schema("created dynamic id"))?,
                )?;
                d.author.mid == Some(actor)
                    && (d.text.as_deref() == Some(text)
                        || (text.trim().is_empty()
                            && d.text.as_deref().is_none_or(|t| t.trim().is_empty())))
                    && pictures
                        .iter()
                        .all(|p| d.media.iter().any(|m| m.url == p.picture().img_src))
            }
            FeatureCommand::DynamicReply {
                message,
                root,
                parent,
                ..
            } => {
                let rpid = receipt["rpid"]
                    .as_u64()
                    .or_else(|| receipt["rpid_str"].as_str()?.parse().ok())
                    .ok_or(Error::Schema("dynamic reply receipt"))?;
                let oid = target["comment_oid"].as_u64().ok_or(Error::InvalidId)?;
                let kind = target["comment_type"].as_u64().ok_or(Error::InvalidId)?;
                let data=api_data(self.transport.json(&format!("https://api.bilibili.com/x/v2/reply/reply?type={kind}&oid={oid}&root={}&pn=1&ps=20",if *root==0{rpid}else{*root}))?,false)?;
                data.get("root")
                    .into_iter()
                    .chain(data["replies"].as_array().into_iter().flatten())
                    .any(|r| {
                        r["rpid"].as_u64() == Some(rpid)
                            && r["oid"].as_u64() == Some(oid)
                            && r["root"].as_u64() == Some(*root)
                            && r["parent"].as_u64() == Some(*parent)
                            && r["member"]["mid"]
                                .as_str()
                                .and_then(|s| s.parse().ok())
                                .or_else(|| r["member"]["mid"].as_u64())
                                .or_else(|| r["mid"].as_u64())
                                == Some(actor)
                            && r["content"]["message"] == *message
                    })
            }
            FeatureCommand::Danmaku {
                video,
                page,
                progress_ms,
                text,
                color,
                mode,
            } => {
                let id = receipt["dmid_str"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| receipt["dmid"].as_u64().map(|n| n.to_string()));
                let segment =
                    u32::try_from(progress_ms / 360_000 + 1).map_err(|_| Error::InvalidPage)?;
                id.is_some_and(|id| {
                    self.danmaku_segment(&video.parse::<VideoId>().unwrap(), *page, segment)
                        .is_ok_and(|items| {
                            items.iter().any(|r| {
                                r.id == id
                                    && r.progress_ms == *progress_ms
                                    && r.content == *text
                                    && r.color == *color
                                    && r.mode == *mode
                            })
                        })
                })
            }
        };
        Ok(json!({"verified":verified,"state_or_exact_readback":true,"actor_mid":actor}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p(c: FeatureCommand) -> PreparedFeature {
        PreparedFeature {
            command: c,
            actor: 7,
            target: json!({"aid":42,"cid":99}),
            before: json!({}),
            cookie: "private".into(),
            csrf: "a".repeat(32),
        }
    }
    #[test]
    fn image_only_dynamic_is_valid_but_unfilled_reservation_cannot_submit() {
        let picture:UploadedCommentImage=serde_json::from_value(json!({"picture":{"img_src":"https://i0.hdslb.com/bfs/new_dyn/synthetic.png","img_width":160,"img_height":160,"img_size":1.0},"actor_mid":7})).unwrap();
        let image_only = FeatureCommand::DynamicCreate {
            text: String::new(),
            pictures: vec![picture],
        };
        assert!(image_only.validate().is_ok());
        let empty = p(FeatureCommand::DynamicCreate {
            text: String::new(),
            pictures: vec![],
        });
        let client = Client::new(crate::Config::default()).unwrap();
        assert!(matches!(
            client.submit_feature(empty),
            Err(Error::InvalidComment)
        ));
        assert!(matches!(
            client.prepare_dynamic_publication("", 0),
            Err(Error::InvalidComment)
        ));
    }
    #[test]
    fn exact_wire_commands_do_not_toggle_or_silently_like() {
        let a = p(FeatureCommand::Coin {
            video: "BV17x411w7KC".into(),
            count: 2,
        });
        let (path, b, j) = request(&a).unwrap();
        assert_eq!(path, "/x/web-interface/coin/add");
        assert!(!j);
        assert!(String::from_utf8(b).unwrap().contains("select_like=0"));
        let a = p(FeatureCommand::DynamicLike {
            dynamic_id: "123".into(),
            like: false,
        });
        let (_, b, j) = request(&a).unwrap();
        assert!(j);
        assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["up"], 2);
    }
    #[test]
    fn dynamic_payload_and_validation_preserve_text() {
        let a = p(FeatureCommand::DynamicCreate {
            text: "图文text".into(),
            pictures: vec![],
        });
        let (path, b, j) = request(&a).unwrap();
        assert_eq!(path, "/x/dynamic/feed/create/dyn");
        assert!(j);
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(
            v["dyn_req"]["content"]["contents"][0]["raw_text"],
            "图文text"
        );
        assert!(
            FeatureCommand::Coin {
                video: "BV17x411w7KC".into(),
                count: 3
            }
            .validate()
            .is_err()
        );
        assert!(
            FeatureCommand::DynamicLike {
                dynamic_id: "../".into(),
                like: true
            }
            .validate()
            .is_err()
        );
    }
}
