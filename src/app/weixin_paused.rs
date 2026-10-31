// SPDX-License-Identifier: MIT
//! Default build: no WebWx client code or account-state access.
use super::*;

const PAUSED: &str =
    "Weixin testing is paused; login, polling and account requests are disabled in this build";

impl Runtime {
    pub fn weixin_login(&mut self) -> Result<Value> {
        Err(PAUSED.into())
    }

    pub fn weixin_login_profile(&mut self, _profile: &str) -> Result<Value> {
        Err(PAUSED.into())
    }

    pub fn weixin_poll_login(&mut self) -> Result<Value> {
        Err(PAUSED.into())
    }

    pub(super) fn call_weixin(&mut self, method: &str, _params: &Value) -> Result<Value> {
        if method == "weixin.status" {
            Ok(json!({
                "status":"testing_paused",
                "live_enabled":false,
                "account_state_read":false,
                "automatic_retry":false
            }))
        } else {
            Err(PAUSED.into())
        }
    }
}
