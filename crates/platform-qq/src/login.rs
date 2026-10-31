// SPDX-License-Identifier: MIT
//! Cached registration and one explicit TempPwd refresh. No QR or retry loop.
use crate::{Client, Result, auth};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMode {
    CachedOnly,
    Auto,
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginSource {
    CachedSession,
    TempPwd,
    Qr,
}

// Challenges can include secret URLs/cookies. No automatic Debug/Serialize.
pub enum LoginOutcome {
    Online { source: LoginSource },
    VerificationRequired { challenge: auth::LoginChallenge },
    Rejected { failure: auth::LoginFailure },
    CredentialsUnavailable,
}

fn refreshable(error: &str) -> bool {
    error == "register rejected"
        || matches!(
            error,
            "QQ rejected session (-10001)"
                | "QQ rejected session (-10003)"
                | "QQ rejected session (-10008)"
        )
}

impl Client {
    /// The existing session file is replaced only after successful registration.
    /// A transport error never triggers a different login mode or QR fallback.
    pub fn authenticate(&mut self, mode: LoginMode) -> Result<LoginOutcome> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("QQ login generation exhausted")?;
        self.online = false;
        if mode != LoginMode::Refresh && self.session.has_tickets()? {
            match self.register() {
                Ok(()) => {
                    self.persist_session()?;
                    return Ok(LoginOutcome::Online {
                        source: LoginSource::CachedSession,
                    });
                }
                Err(error) if mode == LoginMode::Auto && refreshable(&error) => {}
                Err(error) => return Err(error),
            }
        }
        if mode == LoginMode::CachedOnly {
            return Ok(LoginOutcome::CredentialsUnavailable);
        }
        let temp = self.session.temp_pwd()?;
        if self.session.uin == 0 || temp.is_empty() {
            return Ok(LoginOutcome::CredentialsUnavailable);
        }
        let previous = self.session.clone();
        let outcome = self.refresh_temp_pwd(&temp);
        if !matches!(&outcome, Ok(LoginOutcome::Online { .. })) {
            // Keep credentials in memory and on disk. Retain allocated sequence
            // numbers so a later explicit request never reuses one accidentally.
            let sequence = self.session.sequence;
            self.session = previous;
            self.session.sequence = sequence;
            self.online = false;
        }
        outcome
    }
    fn refresh_temp_pwd(&mut self, temp: &[u8]) -> Result<LoginOutcome> {
        self.require_signer()?;
        let device = self.device().clone();
        let guid = device.guid_bytes()?;
        let previous_uid = self.session.uid.clone();
        self.session.clear_outer();
        let exchange = auth::KeyExchange::new()?;
        let request = exchange.request(self.session.uin, &guid)?;
        let reply = self.request(auth::KEY_EXCHANGE_COMMAND, &request)?;
        let exchange = exchange.parse_response(&reply)?;
        let identity = auth::LoginIdentity {
            uin: self.session.uin,
            guid: &guid,
            device_name: &device.device_name,
            kernel_version: &device.kernel_version,
            cookies: (!self.session.cookies.is_empty()).then_some(self.session.cookies.as_str()),
            app_profile: self.session.app_profile,
        };
        let request = auth::build_easy_login_request(&identity, temp, &exchange)?;
        let reply = self.request(auth::EASY_LOGIN_COMMAND, &request)?;
        match auth::parse_easy_login_response(&reply, &exchange)? {
            auth::LoginOutcome::Success(credentials) => {
                if !previous_uid.is_empty()
                    && credentials
                        .uid
                        .as_deref()
                        .is_some_and(|uid| uid != previous_uid)
                {
                    return Err("TempPwd login returned a different account UID".into());
                }
                self.session.set_credentials(
                    &credentials.tgt,
                    &credentials.d2,
                    &credentials.d2_key,
                    &credentials.temp_pwd,
                    credentials.uid.as_deref(),
                )?;
                self.register()?;
                self.persist_session()?;
                Ok(LoginOutcome::Online {
                    source: LoginSource::TempPwd,
                })
            }
            auth::LoginOutcome::VerificationRequired(challenge) => {
                Ok(LoginOutcome::VerificationRequired { challenge })
            }
            auth::LoginOutcome::Rejected(failure) => Ok(LoginOutcome::Rejected { failure }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_documented_session_rejections_allow_one_temp_pwd_attempt() {
        assert!(refreshable("register rejected"));
        assert!(refreshable("QQ rejected session (-10001)"));
        assert!(!refreshable("QQ closed the connection"));
        assert!(!refreshable("response timeout: status"));
        assert!(!refreshable("QQ rejected session (-1)"));
    }
    #[test]
    fn public_client_and_signer_are_send_without_manual_unsafe_impls() {
        fn is_send<T: Send>() {}
        is_send::<Client>();
        is_send::<crate::signer::Signer>();
    }
}
