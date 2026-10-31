// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded native friend/group/member reads; pinned LagrangeGo f7f8abfaa7f1.
use crate::{
    Client, Result,
    wire::{self, Fields, Value},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Friend {
    pub uin: u32,
    pub uid: String,
    pub nickname: String,
    pub remark: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub code: u32,
    pub name: String,
    pub member_count: u32,
    pub max_members: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub uin: u32,
    pub uid: String,
    pub nickname: String,
    pub card: String,
    pub title: String,
    pub permission: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

pub(crate) fn num(f: &Fields<'_>, tag: u32) -> Result<u32> {
    match f.0.iter().rev().find(|(k, _)| *k == tag) {
        None => Ok(0),
        Some((_, Value::Number(n))) => {
            u32::try_from(*n).map_err(|_| "QQ integer exceeds u32".into())
        }
        _ => Err("QQ integer has wrong wire type".into()),
    }
}
pub(crate) fn text(f: &Fields<'_>, tag: u32, max: usize) -> Result<String> {
    let Some(v) = f.0.iter().rev().find(|(k, _)| *k == tag) else {
        return Ok(String::new());
    };
    let Value::Bytes(b) = v.1 else {
        return Err("QQ text has wrong wire type".into());
    };
    if b.len() > max {
        return Err("QQ text exceeds bound".into());
    }
    String::from_utf8(b.to_vec()).map_err(|_| "QQ text is not UTF-8".into())
}
pub(crate) fn oidb(command: u32, sub: u32, body: &[u8], uid: bool) -> Vec<u8> {
    let mut out = Vec::new();
    wire::number(1, command.into(), &mut out);
    wire::number(2, sub.into(), &mut out);
    wire::bytes(4, body, &mut out);
    if uid {
        wire::number(12, 1, &mut out)
    }
    out
}
pub(crate) fn oidb_body<'a>(raw: &'a [u8], command: u32, sub: u32) -> Result<&'a [u8]> {
    let fields = Fields::parse(raw)?;
    if num(&fields, 1)? != command || num(&fields, 2)? != sub {
        return Err("QQ OIDB response command mismatch".into());
    }
    let error = num(&fields, 3)?;
    if error != 0 {
        return Err(format!("QQ OIDB rejected request ({error})"));
    }
    fields.data(4).ok_or("QQ OIDB response body missing".into())
}
pub(crate) fn friends_request(next: u32) -> Vec<u8> {
    let mut b = Vec::new();
    wire::number(2, 300, &mut b);
    if next != 0 {
        let mut n = Vec::new();
        wire::number(1, next.into(), &mut n);
        wire::bytes(5, &n, &mut b)
    }
    wire::number(6, 1, &mut b);
    wire::number(7, 2147483647, &mut b);
    for (typ, numbers) in [
        (1, &[103, 102, 20002, 27394][..]),
        (4, &[100, 101, 102][..]),
    ] {
        let mut n = Vec::new();
        for value in numbers {
            wire::number(1, *value, &mut n)
        }
        let mut item = Vec::new();
        wire::number(1, typ, &mut item);
        wire::bytes(2, &n, &mut item);
        wire::bytes(10001, &item, &mut b)
    }
    for n in [13578, 13579, 13573, 13572, 13568] {
        wire::number(10002, n, &mut b)
    }
    wire::number(10003, 4051, &mut b);
    oidb(0xfd4, 1, &b, false)
}
fn parse_friends(raw: &[u8]) -> Result<Page<Friend>> {
    let f = Fields::parse(oidb_body(raw, 0xfd4, 1)?)?;
    let mut items = Vec::new();
    for (tag, v) in &f.0 {
        if *tag != 101 {
            continue;
        }
        if items.len() >= 300 {
            return Err("QQ friends page exceeds 300".into());
        }
        let Value::Bytes(b) = v else {
            return Err("QQ friend malformed".into());
        };
        let friend = Fields::parse(b)?;
        let mut row = Friend {
            uin: num(&friend, 3)?,
            uid: text(&friend, 1, 128)?,
            nickname: String::new(),
            remark: String::new(),
            signature: String::new(),
        };
        if row.uin == 0 || !crate::rich::valid_uid(&row.uid) {
            return Err("QQ friend identity missing".into());
        }
        for (tag, extra) in friend
            .0
            .iter()
            .filter(|(t, _)| *t == 10001)
            .filter(|(_, v)| matches!(v, Value::Bytes(_)))
        {
            let _ = tag;
            let Value::Bytes(extra) = extra else { continue };
            let e = Fields::parse(extra)?;
            if num(&e, 1)? != 1 {
                continue;
            }
            let l = Fields::parse(e.data(2).unwrap_or_default())?;
            for (tag, v) in l.0 {
                if tag != 2 {
                    continue;
                }
                let Value::Bytes(b) = v else { continue };
                let property = Fields::parse(b)?;
                let value = text(&property, 2, 4096)?;
                match num(&property, 1)? {
                    103 => row.remark = value,
                    102 => row.signature = value,
                    20002 => row.nickname = value,
                    _ => {}
                }
            }
        }
        items.push(row);
    }
    let next = f
        .data(2)
        .map(Fields::parse)
        .transpose()?
        .map(|n| num(&n, 1))
        .transpose()?
        .filter(|n| *n != 0)
        .map(|n| n.to_string());
    Ok(Page { items, next })
}
pub(crate) fn groups_request() -> Vec<u8> {
    let mut a = Vec::new();
    for field in [
        1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 24, 25, 26, 27,
        28, 29, 30, 31, 32, 5001, 5002, 5003,
    ] {
        wire::number(field, 1, &mut a)
    }
    let mut b = Vec::new();
    for f in 1..=8 {
        wire::number(f, 1, &mut b)
    }
    let mut c = Vec::new();
    wire::number(5, 1, &mut c);
    wire::number(6, 1, &mut c);
    let mut config = Vec::new();
    wire::bytes(1, &a, &mut config);
    wire::bytes(2, &b, &mut config);
    wire::bytes(3, &c, &mut config);
    let mut body = Vec::new();
    wire::bytes(1, &config, &mut body);
    oidb(0xfe5, 2, &body, true)
}
fn parse_groups(raw: &[u8]) -> Result<Vec<Group>> {
    let f = Fields::parse(oidb_body(raw, 0xfe5, 2)?)?;
    let mut out = Vec::new();
    for (tag, v) in f.0 {
        if tag != 2 {
            continue;
        }
        if out.len() >= 2048 {
            return Err("QQ group list exceeds 2048".into());
        }
        let Value::Bytes(b) = v else {
            return Err("QQ group malformed".into());
        };
        let g = Fields::parse(b)?;
        let info = Fields::parse(g.data(4).unwrap_or_default())?;
        let code = num(&g, 3)?;
        if code == 0 {
            return Err("QQ group identity missing".into());
        }
        out.push(Group {
            code,
            name: text(&info, 5, 4096)?,
            member_count: num(&info, 4)?,
            max_members: num(&info, 3)?,
        })
    }
    Ok(out)
}
pub(crate) fn members_request(code: u32, token: &str) -> Result<Vec<u8>> {
    if code == 0 || token.len() > 4096 {
        return Err("invalid QQ member cursor".into());
    }
    let mut flags = Vec::new();
    for n in [10, 11, 12, 17, 100, 101, 102, 107] {
        wire::number(n, 1, &mut flags)
    }
    let mut b = Vec::new();
    wire::number(1, code.into(), &mut b);
    wire::number(2, 5, &mut b);
    wire::number(3, 2, &mut b);
    wire::bytes(4, &flags, &mut b);
    wire::bytes(15, token.as_bytes(), &mut b);
    Ok(oidb(0xfe7, 3, &b, false))
}
fn parse_members(raw: &[u8], expected: u32) -> Result<Page<Member>> {
    let f = Fields::parse(oidb_body(raw, 0xfe7, 3)?)?;
    if num(&f, 1)? != expected {
        return Err("QQ member group mismatch".into());
    }
    let mut items = Vec::new();
    for (tag, v) in &f.0 {
        if *tag != 2 {
            continue;
        }
        if items.len() >= 1000 {
            return Err("QQ member page exceeds1000".into());
        }
        let Value::Bytes(b) = v else {
            return Err("QQ member malformed".into());
        };
        let m = Fields::parse(b)?;
        let id = Fields::parse(m.data(1).unwrap_or_default())?;
        let card = Fields::parse(m.data(11).unwrap_or_default())?;
        let row = Member {
            uin: num(&id, 4)?,
            uid: text(&id, 2, 128)?,
            nickname: text(&m, 10, 4096)?,
            card: text(&card, 2, 4096)?,
            title: text(&m, 17, 4096)?,
            permission: num(&m, 107)?,
        };
        if row.uin == 0 || !crate::rich::valid_uid(&row.uid) {
            return Err("QQ member identity missing".into());
        }
        items.push(row)
    }
    let token = text(&f, 15, 4096)?;
    Ok(Page {
        items,
        next: (!token.is_empty()).then_some(token),
    })
}
impl Client {
    pub fn friends(&mut self, token: u32) -> Result<Page<Friend>> {
        if !self.is_online() {
            return Err("QQ session is not registered".into());
        }
        let raw = self.request("OidbSvcTrpcTcp.0xfd4_1", &friends_request(token))?;
        parse_friends(&raw)
    }
    pub fn groups(&mut self) -> Result<Vec<Group>> {
        if !self.is_online() {
            return Err("QQ session is not registered".into());
        }
        let raw = self.request("OidbSvcTrpcTcp.0xfe5_2", &groups_request())?;
        parse_groups(&raw)
    }
    pub fn members(&mut self, code: u32, token: &str) -> Result<Page<Member>> {
        if !self.is_online() {
            return Err("QQ session is not registered".into());
        }
        let raw = self.request("OidbSvcTrpcTcp.0xfe7_3", &members_request(code, token)?)?;
        parse_members(&raw, code)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn vector(name: &str) -> Vec<u8> {
        let set: serde_json::Value =
            serde_json::from_str(include_str!("../tests/reference.json")).unwrap();
        let s = set[name].as_str().unwrap();
        (0..s.len())
            .step_by(2)
            .map(|n| u8::from_str_radix(&s[n..n + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn directory_requests_match_independent_native_go() {
        assert_eq!(friends_request(12345), vector("friends_request"));
        assert_eq!(groups_request(), vector("groups_request"));
        assert_eq!(
            members_request(123456, "next_fixture").unwrap(),
            vector("members_request")
        );
    }
    #[test]
    fn missing_or_foreign_directory_identity_is_not_fabricated() {
        let mut friend = vec![];
        wire::number(3, 12345, &mut friend);
        let mut b = vec![];
        wire::bytes(101, &friend, &mut b);
        assert!(parse_friends(&oidb(0xfd4, 1, &b, false)).is_err());
        let mut b = vec![];
        wire::number(1, 123456, &mut b);
        assert!(parse_members(&oidb(0xfe7, 3, &b, false), 222).is_err());
        assert!(oidb_body(&oidb(0xfe5, 2, &[], true), 0xfd4, 1).is_err());
    }
}
