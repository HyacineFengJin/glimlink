// SPDX-License-Identifier: AGPL-3.0-only
//! Native server history. Read-only; never acknowledges, retries or marks read.
use crate::{
    Client, IncomingEvent, Result,
    directory::{num, oidb, oidb_body, text},
    rich::valid_uid,
    wire::{self, Fields, Value},
};
pub struct HistoryPage {
    pub events: Vec<IncomingEvent>,
    pub next_time: Option<u32>,
    pub next_sequence: Option<u32>,
}
impl Client {
    pub fn group_latest_sequence(&mut self, code: u32) -> Result<u32> {
        if !self.is_online() || code == 0 {
            return Err("QQ group sequence request invalid".into());
        }
        let mut selector = Vec::new();
        wire::number(22, 1, &mut selector);
        let mut config = Vec::new();
        wire::number(1, code.into(), &mut config);
        wire::bytes(2, &selector, &mut config);
        let mut body = Vec::new();
        wire::number(1, 537099973, &mut body);
        wire::bytes(2, &config, &mut body);
        let raw = self.request("OidbSvcTrpcTcp.0x88d_0", &oidb(0x88d, 0, &body, false))?;
        let f = Fields::parse(oidb_body(&raw, 0x88d, 0)?)?;
        let info = Fields::parse(f.data(1).ok_or("QQ group info missing")?)?;
        if num(&info, 1)? != code || num(&info, 2)? != 0 {
            return Err("QQ group info identity/result mismatch".into());
        }
        let data = Fields::parse(info.data(3).ok_or("QQ group info data missing")?)?;
        num(&data, 22)
    }
    pub fn private_history(&mut self, uid: &str, before: u32, count: u32) -> Result<HistoryPage> {
        if !self.is_online() || !valid_uid(uid) || !(1..=50).contains(&count) {
            return Err("invalid QQ private history request".into());
        }
        let mut b = Vec::new();
        wire::bytes(1, uid.as_bytes(), &mut b);
        wire::number(2, before.into(), &mut b);
        wire::number(4, count.into(), &mut b);
        wire::number(5, 1, &mut b);
        let raw = self.request("trpc.msg.register_proxy.RegisterProxy.SsoGetRoamMsg", &b)?;
        let f = Fields::parse(&raw)?;
        let returned = text(&f, 3, 128)?;
        if !returned.is_empty() && returned != uid {
            return Err("QQ history peer mismatch".into());
        }
        let next = num(&f, 5)?;
        let events = history_events(&f, 7, count as usize)?;
        Ok(HistoryPage {
            events,
            next_time: (next > 0 && next < before).then_some(next),
            next_sequence: None,
        })
    }
    pub fn group_history(&mut self, code: u32, start: u32, end: u32) -> Result<HistoryPage> {
        if !self.is_online() || code == 0 || start == 0 || end < start || end - start >= 50 {
            return Err("QQ group history requires an explicit sequence range of at most50".into());
        }
        let mut info = Vec::new();
        for (f, n) in [(1, code), (2, start), (3, end)] {
            wire::number(f, n.into(), &mut info)
        }
        let mut req = Vec::new();
        wire::bytes(1, &info, &mut req);
        wire::number(2, 1, &mut req);
        let raw = self.request("trpc.msg.register_proxy.RegisterProxy.SsoGetGroupMsg", &req)?;
        let f = Fields::parse(&raw)?;
        let body = Fields::parse(f.data(3).ok_or("QQ group history body missing")?)?;
        if num(&body, 3)? != code {
            return Err("QQ group history identity mismatch".into());
        }
        Ok(HistoryPage {
            events: history_events(&body, 6, 50)?,
            next_time: None,
            next_sequence: start.checked_sub(1).filter(|n| *n > 0),
        })
    }
}
fn history_events(f: &Fields<'_>, tag: u32, limit: usize) -> Result<Vec<IncomingEvent>> {
    let mut out = Vec::new();
    for (n, v) in &f.0 {
        if *n != tag {
            continue;
        }
        if out.len() >= limit {
            return Err("QQ history exceeds requested page bound".into());
        }
        let Value::Bytes(b) = v else {
            return Err("QQ history message malformed".into());
        };
        let mut push = Vec::new();
        wire::bytes(1, b, &mut push);
        out.push(IncomingEvent::from_push(0, push)?)
    }
    Ok(out)
}
