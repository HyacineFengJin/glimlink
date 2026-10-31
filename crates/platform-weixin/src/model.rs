// SPDX-License-Identifier: MIT
use crate::{Error, Result};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub username: String,
    pub nickname: String,
    pub remark: String,
    pub group: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct ContactPage {
    pub contacts: Vec<Contact>,
    pub next_sequence: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub user: Contact,
    pub cursor_entries: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub selector: u32,
    pub has_updates: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Voice,
    Video,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub msg_type: u32,
    pub from: String,
    pub to: String,
    pub conversation: String,
    pub group_sender: Option<String>,
    pub content: String,
    pub created_at: u64,
    pub voice_length_ms: Option<u64>,
    pub video_length_seconds: Option<u64>,
    pub media: Option<MediaKind>,
    pub source: Value,
}
#[derive(Debug, Clone, Serialize)]
pub struct SyncBatch {
    pub messages: Vec<Message>,
    pub changed_contacts: Vec<Contact>,
    pub continue_sync: bool,
}
pub(crate) struct Ticket {
    pub skey: String,
    pub sid: String,
    pub uin: u64,
    pub pass_ticket: String,
}
pub(crate) type SyncKey = Vec<(u64, u64)>;

pub(crate) fn ticket_response(response: crate::http::TicketResponse) -> Result<Ticket> {
    let declared_gbk = response.content_type.as_deref().is_some_and(|v| {
        let value = v.to_ascii_lowercase();
        value.contains("gbk") || value.contains("gb2312")
    });
    let text = if declared_gbk {
        let (value, _, errors) = encoding_rs::GBK.decode(&response.body);
        if errors {
            return Err(Error::Protocol);
        }
        value
    } else {
        std::str::from_utf8(&response.body)
            .map(std::borrow::Cow::Borrowed)
            .map_err(|_| Error::Protocol)?
    };
    if text.to_ascii_lowercase().contains("<html") {
        if text.contains("暂不支持使用网页版微信")
            && (text.contains("账号安全") || text.contains("帐号安全"))
        {
            return Err(Error::WebLoginPageUnavailable);
        }
        return Err(Error::LoginHtmlResponse {
            status: response.status,
        });
    }
    if text.trim().is_empty()
        && response.location.is_some()
        && matches!(response.status, 301 | 302 | 303 | 307 | 308)
    {
        return Err(Error::UnexpectedLoginRedirect {
            status: response.status,
        });
    }
    parse_ticket(text.as_bytes())
}

pub(crate) fn parse_ticket(bytes: &[u8]) -> Result<Ticket> {
    if bytes.len() > 64 * 1024 {
        return Err(Error::Limit);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Protocol)?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::new();
    let mut fields = BTreeMap::<String, String>::new();
    loop {
        match reader.read_event().map_err(|_| Error::Protocol)? {
            Event::Start(e) => {
                if stack.len() >= 16 {
                    return Err(Error::Limit);
                }
                let name =
                    String::from_utf8(e.name().as_ref().to_vec()).map_err(|_| Error::Protocol)?;
                stack.push(name);
            }
            Event::End(_) => {
                stack.pop().ok_or(Error::Protocol)?;
            }
            Event::Text(e) => {
                if let Some(name) = stack.last() {
                    let text = e.decode().map_err(|_| Error::Protocol)?;
                    let value = quick_xml::escape::unescape(&text).map_err(|_| Error::Protocol)?;
                    fields.entry(name.clone()).or_default().push_str(&value);
                }
            }
            Event::CData(e) => {
                if let Some(name) = stack.last() {
                    fields
                        .entry(name.clone())
                        .or_default()
                        .push_str(&e.decode().map_err(|_| Error::Protocol)?);
                }
            }
            Event::GeneralRef(e) => {
                if let Some(name) = stack.last() {
                    let key = e.decode().map_err(|_| Error::Protocol)?;
                    let encoded = format!("&{key};");
                    let value =
                        quick_xml::escape::unescape(&encoded).map_err(|_| Error::Protocol)?;
                    fields.entry(name.clone()).or_default().push_str(&value);
                }
            }
            Event::DocType(_) => return Err(Error::Protocol),
            Event::Eof => break,
            _ => {}
        }
    }
    let code = fields
        .get("ret")
        .ok_or(Error::Protocol)?
        .trim()
        .parse::<i64>()
        .map_err(|_| Error::Protocol)?;
    if code != 0 {
        let message = fields.get("message").map(String::as_str).unwrap_or("");
        if message.contains("网页版")
            || message.contains("Web WeChat")
            || message.contains("web WeChat")
        {
            return Err(Error::WebLoginUnavailable { code });
        }
        return Err(Error::Platform { code });
    }
    let take = |key: &str| -> Result<String> {
        let value = fields.get(key).ok_or(Error::Protocol)?;
        if value.is_empty() || value.len() > 8192 {
            return Err(Error::Protocol);
        }
        Ok(value.clone())
    };
    let uin = take("wxuin")?.parse::<u64>().map_err(|_| Error::Protocol)?;
    if uin == 0 {
        return Err(Error::Protocol);
    }
    Ok(Ticket {
        skey: take("skey")?,
        sid: take("wxsid")?,
        uin,
        pass_ticket: take("pass_ticket")?,
    })
}
pub(crate) fn check_response(value: &Value) -> Result<()> {
    let code = value
        .pointer("/BaseResponse/Ret")
        .and_then(Value::as_i64)
        .ok_or(Error::Protocol)?;
    match code {
        0 => Ok(()),
        1100 | 1101 | 1102 => Err(Error::AuthExpired { code }),
        _ => Err(Error::Platform { code }),
    }
}
fn string(value: &Value, key: &str, max: usize) -> Result<String> {
    let value = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::Protocol)?;
    if value.len() > max {
        return Err(Error::Limit);
    }
    Ok(value.to_owned())
}
fn optional_string(value: &Value, key: &str, max: usize) -> Result<String> {
    if value.get(key).is_none() {
        Ok(String::new())
    } else {
        string(value, key, max)
    }
}
pub(crate) fn contact(value: &Value) -> Result<Contact> {
    let username = string(value, "UserName", 256)?;
    if username.is_empty() || username.chars().any(char::is_control) {
        return Err(Error::Protocol);
    }
    Ok(Contact {
        group: username.starts_with("@@"),
        username,
        nickname: optional_string(value, "NickName", 4096)?,
        remark: optional_string(value, "RemarkName", 4096)?,
    })
}
pub(crate) fn contacts(value: Value) -> Result<ContactPage> {
    check_response(&value)?;
    let list = value
        .get("MemberList")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    if list.len() > 10000 {
        return Err(Error::Limit);
    }
    if value
        .get("MemberCount")
        .and_then(Value::as_u64)
        .is_some_and(|n| n != list.len() as u64)
    {
        return Err(Error::Protocol);
    }
    Ok(ContactPage {
        contacts: list.iter().map(contact).collect::<Result<_>>()?,
        next_sequence: value.get("Seq").and_then(Value::as_u64).unwrap_or(0),
    })
}
pub(crate) fn parse_sync_key(value: &Value) -> Result<SyncKey> {
    let list = value
        .get("List")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    if list.is_empty()
        || list.len() > 1024
        || value.get("Count").and_then(Value::as_u64) != Some(list.len() as u64)
    {
        return Err(Error::Protocol);
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in list {
        let k = item
            .get("Key")
            .and_then(Value::as_u64)
            .ok_or(Error::Protocol)?;
        let v = item
            .get("Val")
            .and_then(Value::as_u64)
            .ok_or(Error::Protocol)?;
        if !seen.insert(k) {
            return Err(Error::Protocol);
        }
        out.push((k, v));
    }
    Ok(out)
}
pub(crate) fn sync_key_value(key: &SyncKey) -> Value {
    json!({"Count":key.len(),"List":key.iter().map(|&(k,v)|json!({"Key":k,"Val":v})).collect::<Vec<_>>()})
}
fn quoted_number(text: &str, key: &str) -> Result<i64> {
    let tail = text.split_once(key).ok_or(Error::Protocol)?.1.trim_start();
    let tail = tail.strip_prefix(':').ok_or(Error::Protocol)?.trim_start();
    let quote = tail.chars().next().ok_or(Error::Protocol)?;
    if !matches!(quote, '"' | '\'') {
        return Err(Error::Protocol);
    }
    let digits = tail[1..].split_once(quote).ok_or(Error::Protocol)?.0;
    if digits.len() > 12 {
        return Err(Error::Protocol);
    }
    digits.parse().map_err(|_| Error::Protocol)
}
pub(crate) fn parse_sync_check(bytes: &[u8]) -> Result<SyncStatus> {
    if bytes.len() > 4096 {
        return Err(Error::Limit);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Protocol)?;
    if !text.contains("window.synccheck=") {
        return Err(Error::Protocol);
    }
    let code = quoted_number(text, "retcode")?;
    if code != 0 {
        return Err(Error::AuthExpired { code });
    }
    let selector = u32::try_from(quoted_number(text, "selector")?).map_err(|_| Error::Protocol)?;
    Ok(SyncStatus {
        selector,
        has_updates: selector != 0,
    })
}
pub(crate) fn sync_batch(value: Value, self_username: &str) -> Result<(SyncBatch, SyncKey)> {
    check_response(&value)?;
    let next = parse_sync_key(value.get("SyncKey").ok_or(Error::Protocol)?)?;
    let mut messages = Vec::new();
    let list = value
        .get("AddMsgList")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol)?;
    if list.len() > 1000 {
        return Err(Error::Limit);
    }
    if value.get("AddMsgCount").and_then(Value::as_u64) != Some(list.len() as u64) {
        return Err(Error::Protocol);
    }
    for raw in list {
        let id = raw
            .get("MsgId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                raw.get("MsgId")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
            })
            .ok_or(Error::Protocol)?;
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Protocol);
        }
        let msg_type = u32::try_from(
            raw.get("MsgType")
                .and_then(Value::as_u64)
                .ok_or(Error::Protocol)?,
        )
        .map_err(|_| Error::Protocol)?;
        let from = string(raw, "FromUserName", 256)?;
        let to = string(raw, "ToUserName", 256)?;
        let content = string(raw, "Content", 128 * 1024)?;
        let conversation = if from == self_username {
            to.clone()
        } else {
            from.clone()
        };
        let group_sender = if from.starts_with("@@") {
            content
                .split_once(":<br/>")
                .map(|(prefix, _)| prefix)
                .filter(|v| {
                    v.starts_with('@') && v.len() <= 256 && !v.chars().any(char::is_whitespace)
                })
                .map(str::to_owned)
        } else {
            None
        };
        let media = match msg_type {
            3 => Some(MediaKind::Image),
            34 => Some(MediaKind::Voice),
            43 | 62 => Some(MediaKind::Video),
            _ => None,
        };
        let created_at = raw.get("CreateTime").and_then(Value::as_u64).unwrap_or(0);
        // Explicit whitelist: never expose top-level SyncKey/SKey or unknown auth fields.
        let source = json!({"MsgId":id,"MsgType":msg_type,"FromUserName":from,"ToUserName":to,"Content":content,"CreateTime":created_at,
            "VoiceLength":raw.get("VoiceLength").and_then(Value::as_u64),"PlayLength":raw.get("PlayLength").and_then(Value::as_u64)});
        messages.push(Message {
            id,
            msg_type,
            from,
            to,
            conversation,
            group_sender,
            content,
            created_at,
            voice_length_ms: raw.get("VoiceLength").and_then(Value::as_u64),
            video_length_seconds: raw.get("PlayLength").and_then(Value::as_u64),
            media,
            source,
        });
    }
    let changed = value.get("ModContactList").and_then(Value::as_array);
    if changed.is_some_and(|v| v.len() > 10000) {
        return Err(Error::Limit);
    }
    let changed_contacts = changed
        .into_iter()
        .flatten()
        .map(contact)
        .collect::<Result<Vec<_>>>()?;
    let batch = SyncBatch {
        messages,
        changed_contacts,
        continue_sync: value
            .get("ContinueFlag")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            != 0,
    };
    Ok((batch, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_html_is_classified_after_gbk_decode_without_fabricating_a_retcode() {
        let html = "<html><title>微信网页版</title><body>为了保障你的账号安全，暂不支持使用网页版微信。你可以前往微信官网下载客户端登录。</body></html>";
        let (bytes, _, errors) = encoding_rs::GBK.encode(html);
        assert!(!errors);
        let response = crate::http::TicketResponse {
            status: 200,
            content_type: Some("text/html; charset=gbk".into()),
            location: None,
            body: bytes.into_owned(),
        };
        assert!(matches!(
            ticket_response(response),
            Err(Error::WebLoginPageUnavailable)
        ));
        let response = crate::http::TicketResponse {
            status: 200,
            content_type: Some("text/html".into()),
            location: None,
            body: b"<html>unknown non-account-specific page</html>".to_vec(),
        };
        assert!(matches!(
            ticket_response(response),
            Err(Error::LoginHtmlResponse { status: 200 })
        ));
        let url=crate::http::ticket_url("https://wx2.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage?ticket=private%2Bencoded&lang=zh_CN&version=old").unwrap();
        assert_eq!(
            url,
            "https://wx2.qq.com/cgi-bin/mmwebwx-bin/webwxnewloginpage?ticket=private%2Bencoded&lang=zh_CN&version=v2"
        );
    }
    #[test]
    fn xml_ticket_and_rejection_are_bounded_and_redacted() {
        let ticket=parse_ticket(b"<error><ret>0</ret><skey>@a&amp;b</skey><wxsid>private-sid</wxsid><wxuin>123</wxuin><pass_ticket>private-ticket</pass_ticket></error>").unwrap();
        assert_eq!(ticket.skey, "@a&b");
        assert_eq!(ticket.uin, 123);
        let err=parse_ticket("<error><ret>1203</ret><message>此微信号不能登录网页版微信 private-token</message></error>".as_bytes()).err().unwrap();
        assert_eq!(err, Error::WebLoginUnavailable { code: 1203 });
        assert!(!format!("{err}").contains("private-token"));
        assert!(
            parse_ticket(b"<!DOCTYPE x [<!ENTITY a 'boom'>]><error><ret>0</ret></error>").is_err()
        );
    }
    #[test]
    fn sync_keeps_messages_and_excludes_session_material() {
        let value = json!({"BaseResponse":{"Ret":0},"SyncKey":{"Count":1,"List":[{"Key":1,"Val":20}]},"SKey":"secret-top","AddMsgCount":1,"AddMsgList":[{"MsgId":"1234567890123456789","MsgType":34,"FromUserName":"@@group","ToUserName":"@self","Content":"@sender:<br/>voice","VoiceLength":1200,"SKey":"secret-message"}],"ContinueFlag":0});
        let (batch, key) = sync_batch(value, "@self").unwrap();
        assert_eq!(key, vec![(1, 20)]);
        assert_eq!(batch.messages[0].id, "1234567890123456789");
        assert_eq!(batch.messages[0].group_sender.as_deref(), Some("@sender"));
        assert_eq!(batch.messages[0].media, Some(MediaKind::Voice));
        assert!(!serde_json::to_string(&batch).unwrap().contains("secret-"));
        assert_eq!(
            parse_sync_check(br#"window.synccheck={retcode:"1101",selector:"0"}"#).err(),
            Some(Error::AuthExpired { code: 1101 })
        );
        assert!(
            parse_sync_check(br#"window.synccheck={retcode:"0",selector:"2"}"#)
                .unwrap()
                .has_updates
        );
    }
}
