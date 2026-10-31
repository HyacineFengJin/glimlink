// SPDX-License-Identifier: MIT
//! Private human-operator control plane. This is deliberately separate from MCP.
//! Requests travel over a trusted local pipe or authenticated SSH stdio. There
//! is no network listener, generic method execution or remote file-path input.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    fmt,
    io::{BufRead, Write},
};

#[cfg(feature = "human-ui")]
pub mod human_ui;
pub mod login_window;

pub type Result<T> = std::result::Result<T, String>;
pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_SECRET: usize = 256 * 1024;
pub const MAX_QR: usize = 256 * 1024;

/// No Debug representation or error message contains the secret. Base64 is
/// framing, not encryption: only trusted pipes and authenticated SSH may carry it.
pub struct Secret(Vec<u8>);
impl Secret {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET {
            return Err("private input size rejected".into());
        }
        Ok(Self(bytes))
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(&self.0))
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_SECRET.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("private input too large"));
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| serde::de::Error::custom("invalid private input encoding"))?;
        Self::new(bytes).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Runtime,
    Bilibili,
    Tieba,
    Qq,
    Github,
    #[serde(alias = "twitter")]
    X,
    Codex,
    Antigravity,
    Weixin,
}
impl Platform {
    pub fn name(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Bilibili => "bilibili",
            Self::Tieba => "tieba",
            Self::Qq => "qq",
            Self::Github => "github",
            Self::X => "x",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Weixin => "weixin",
        }
    }
}
impl std::str::FromStr for Platform {
    type Err = String;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "runtime" => Ok(Self::Runtime),
            "bili" | "bilibili" => Ok(Self::Bilibili),
            "tieba" => Ok(Self::Tieba),
            "qq" => Ok(Self::Qq),
            "github" => Ok(Self::Github),
            "x" | "twitter" => Ok(Self::X),
            "codex" => Ok(Self::Codex),
            "antigravity" | "agy" => Ok(Self::Antigravity),
            "weixin" | "wechat" => Ok(Self::Weixin),
            _ => Err("unsupported operator platform".into()),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Capabilities,
    Login,
    LoginPoll,
    CancelLogin,
    Status,
    ImportLogin,
    PasswordLogin,
    PasswordResume,
    Challenge,
    Qr,
    QrPoll,
    ReuseLogin,
}
impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "capabilities" => Ok(Self::Capabilities),
            "login" => Ok(Self::Login),
            "login-poll" => Ok(Self::LoginPoll),
            "cancel-login" => Ok(Self::CancelLogin),
            "status" => Ok(Self::Status),
            "import-login" => Ok(Self::ImportLogin),
            "password-login" => Ok(Self::PasswordLogin),
            "password-resume" => Ok(Self::PasswordResume),
            "challenge" => Ok(Self::Challenge),
            "qr" => Ok(Self::Qr),
            "qr-poll" => Ok(Self::QrPoll),
            "reuse-login" => Ok(Self::ReuseLogin),
            _ => Err("unsupported operator action".into()),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub action: Action,
    pub platform: Platform,
    pub account: String,
    #[serde(default = "empty_object")]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<Secret>,
}
fn empty_object() -> Value {
    json!({})
}
impl Request {
    pub fn new(id: u64, platform: Platform, account: String, action: Action) -> Self {
        Self {
            version: VERSION,
            id,
            action,
            platform,
            account,
            params: empty_object(),
            secret: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != VERSION || self.id == 0 {
            return Err("unsupported operator version or request ID".into());
        }
        if self.account.is_empty()
            || self.account.len() > 128
            || self.account.chars().any(char::is_control)
        {
            return Err("invalid operator account alias".into());
        }
        let fields = self
            .params
            .as_object()
            .ok_or("operator params must be an object")?;
        let allowed: &[&str] = match self.action {
            Action::Login => &["method", "app_profile"],
            Action::LoginPoll => &["login_id", "challenge_id"],
            Action::CancelLogin => &["login_id"],
            Action::Qr => &["app_profile"],
            Action::PasswordResume => &["challenge_id"],
            _ => &[],
        };
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("unsupported operator parameter".into());
        }
        if fields.values().any(|v| {
            v.as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control))
        }) {
            return Err("invalid operator parameter value".into());
        }
        let takes_secret = matches!(
            self.action,
            Action::ImportLogin | Action::PasswordLogin | Action::PasswordResume
        );
        let optional_secret = matches!(self.action, Action::Login | Action::LoginPoll);
        if takes_secret && self.secret.is_none()
            || !takes_secret && !optional_secret && self.secret.is_some()
        {
            return Err("action private-input requirement not met".into());
        }
        if !matches!(self.action, Action::Status | Action::Capabilities) && self.account == "public"
        {
            return Err("authentication requires an explicit account alias".into());
        }
        let supported = match self.action {
            Action::Status | Action::Capabilities => true,
            Action::Login | Action::LoginPoll | Action::CancelLogin => {
                self.platform != Platform::Runtime
            }
            Action::ImportLogin => matches!(
                self.platform,
                Platform::Bilibili
                    | Platform::Tieba
                    | Platform::Qq
                    | Platform::Github
                    | Platform::X
            ),
            Action::PasswordLogin | Action::PasswordResume | Action::Challenge => {
                self.platform == Platform::Qq
            }
            Action::ReuseLogin => !matches!(self.platform, Platform::Runtime | Platform::Weixin),
            Action::Qr => matches!(
                self.platform,
                Platform::Bilibili | Platform::Tieba | Platform::Qq
            ),
            Action::QrPoll => matches!(self.platform, Platform::Bilibili | Platform::Tieba),
        };
        if !supported {
            return Err("action is unavailable for this platform".into());
        }
        if fields.contains_key("app_profile") && self.platform != Platform::Qq {
            return Err("app_profile is only supported for QQ".into());
        }
        if matches!(self.action, Action::LoginPoll | Action::CancelLogin)
            && !fields.contains_key("login_id")
        {
            return Err("login_id is required for polling or cancellation".into());
        }
        if let Some(method) = fields.get("method") {
            method
                .as_str()
                .ok_or("invalid login method")?
                .parse::<LoginMethod>()?;
        }
        Ok(())
    }
}

/// Methods describe actual backend flows. Auto delegates selection to the core;
/// it does not permit the client to retry arbitrary failures using another flow.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    Auto,
    Qr,
    Password,
    Device,
    Browser,
}
impl std::str::FromStr for LoginMethod {
    type Err = String;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "qr" => Ok(Self::Qr),
            "password" => Ok(Self::Password),
            "device" => Ok(Self::Device),
            "browser" => Ok(Self::Browser),
            _ => Err("unsupported login method".into()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginCapabilities {
    pub platform: Platform,
    pub methods: Vec<LoginMethod>,
    pub reuse: bool,
    #[serde(default)]
    pub import_login: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_method: Option<LoginMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Authenticated,
    Pending,
    VerificationRequired,
    Failed,
    Cancelled,
}
impl LoginStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Authenticated | Self::Failed | Self::Cancelled)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Authorization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretInputKind {
    Password,
    AuthorizationCode,
    Token,
    Session,
    /// A web session supplied privately by the operator, never a platform password.
    Cookies,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputRequired {
    pub kind: SecretInputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginError {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QrPresentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginResult {
    pub status: LoginStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<AccountIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<LoginMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<Authorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<Challenge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr: Option<QrPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_required: Option<InputRequired>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LoginError>,
}
impl LoginResult {
    pub fn new(status: LoginStatus) -> Self {
        Self {
            status,
            identity: None,
            login_id: None,
            method: None,
            reused_session: None,
            expires_at: None,
            poll_after_ms: None,
            authorization: None,
            challenge: None,
            qr: None,
            input_required: None,
            error: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Response {
        version: u32,
        id: u64,
        ok: bool,
        result: Value,
    },
    Event {
        version: u32,
        id: u64,
        event: String,
        data: Value,
    },
}
impl Frame {
    pub fn success(id: u64, result: Value) -> Self {
        Self::Response {
            version: VERSION,
            id,
            ok: true,
            result,
        }
    }
    pub fn failure(id: u64, code: &str, message: &str) -> Self {
        Self::Response {
            version: VERSION,
            id,
            ok: false,
            result: json!({"code":code,"message":message}),
        }
    }
    pub fn event(id: u64, event: &str, data: Value) -> Self {
        Self::Event {
            version: VERSION,
            id,
            event: event.into(),
            data,
        }
    }
    pub fn id(&self) -> u64 {
        match self {
            Self::Response { id, .. } | Self::Event { id, .. } => *id,
        }
    }
    pub fn version(&self) -> u32 {
        match self {
            Self::Response { version, .. } | Self::Event { version, .. } => *version,
        }
    }
}

/// Stops at MAX_FRAME without allocating an unbounded line, including for an
/// unterminated input stream. EOF is only accepted between complete frames.
pub fn read_json<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| "operator input read failed")?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err("operator frame missing newline".into())
            };
        }
        let end = available.iter().position(|&b| b == b'\n');
        let take = end.map_or(available.len(), |at| at + 1);
        if line.len() + take > MAX_FRAME {
            return Err("operator frame exceeds limit".into());
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() {
            break;
        }
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|_| "invalid operator JSON frame".into())
}
pub fn write_json(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| "cannot encode operator frame")?;
    if bytes.len() + 1 > MAX_FRAME {
        return Err("operator output frame exceeds limit".into());
    }
    writer
        .write_all(&bytes)
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|_| "operator output write failed".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub kind: String,
    pub challenge_id: String,
    pub expires_at: u32,
    pub verify_url: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptchaProof {
    pub ticket: String,
    pub randstr: String,
    pub widget_app_id: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn private_roundtrip_and_bounded_frames() {
        let mut r = Request::new(1, Platform::Bilibili, "main".into(), Action::ImportLogin);
        r.secret = Some(Secret::new(b"synthetic-cookie".to_vec()).unwrap());
        assert!(r.validate().is_ok());
        assert!(!format!("{r:?}").contains("synthetic-cookie"));
        let mut bytes = Vec::new();
        write_json(&mut bytes, &r).unwrap();
        let received: Request = read_json(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(received.secret.unwrap().bytes(), b"synthetic-cookie");
        assert!(read_json::<Request>(&mut Cursor::new(vec![b' '; MAX_FRAME + 1])).is_err());
        assert!(read_json::<Value>(&mut Cursor::new(b"{}".as_slice())).is_err());
        assert!(
            read_json::<Request>(&mut Cursor::new(Vec::new()))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn no_generic_method_remote_path_or_public_auth() {
        let mut r = Request::new(1, Platform::Qq, "main".into(), Action::Qr);
        r.params = json!({"path":"/secret"});
        assert!(r.validate().is_err());
        r.params = json!({});
        r.account = "public".into();
        assert!(r.validate().is_err());
        r.account = "main".into();
        r.action = Action::PasswordResume;
        assert!(r.validate().is_err());
        r.secret = Some(Secret::new(b"{}".to_vec()).unwrap());
        r.params = json!({"challenge_id":"synthetic"});
        assert!(r.validate().is_ok());
        assert!(serde_json::from_value::<Request>(json!({"version":1,"id":1,"action":"shell","platform":"qq","account":"main","params":{}})).is_err());
    }
    #[test]
    fn event_and_response_share_exact_request_id() {
        let frames = [
            Frame::event(
                7,
                "qr_ready",
                json!({"url":"https://example.invalid/synthetic"}),
            ),
            Frame::success(7, json!({"status":"online"})),
        ];
        let mut bytes = Vec::new();
        for frame in &frames {
            write_json(&mut bytes, frame).unwrap();
        }
        let mut reader = Cursor::new(bytes);
        for expected in frames {
            assert_eq!(read_json::<Frame>(&mut reader).unwrap(), Some(expected));
        }
    }
    #[test]
    fn common_login_validates_methods_ids_and_secret_only_proofs() {
        let mut request = Request::new(1, Platform::Codex, "synthetic".into(), Action::Login);
        request.params = json!({"method":"device"});
        assert!(request.validate().is_ok());
        request.params = json!({"method":"guess-a-new-provider"});
        assert!(request.validate().is_err());
        request.action = Action::LoginPoll;
        request.params = json!({});
        assert!(request.validate().is_err());
        request.params = json!({"login_id":"synthetic-flow","captcha":{"ticket":"synthetic"}});
        assert!(request.validate().is_err());
        request.params = json!({"login_id":"synthetic-flow"});
        request.secret =
            Some(Secret::new(br#"{"authorization_code":"synthetic"}"#.to_vec()).unwrap());
        assert!(request.validate().is_ok());
        request.action = Action::CancelLogin;
        assert!(request.validate().is_err());
        request.secret = None;
        assert!(request.validate().is_ok());
        request.action = Action::Capabilities;
        request.account = "public".into();
        request.platform = Platform::Weixin;
        request.params = json!({});
        assert!(request.validate().is_ok());
    }
    #[test]
    fn x_web_session_is_private_and_only_offers_supported_operator_actions() {
        assert_eq!("twitter".parse::<Platform>().unwrap(), Platform::X);
        assert_eq!(serde_json::to_value(Platform::X).unwrap(), "x");
        assert_eq!(
            serde_json::to_value(SecretInputKind::Cookies).unwrap(),
            "cookies"
        );
        for action in [
            Action::Status,
            Action::Capabilities,
            Action::ReuseLogin,
            Action::Login,
        ] {
            assert!(
                Request::new(1, Platform::X, "synthetic".into(), action)
                    .validate()
                    .is_ok()
            );
        }
        let mut request = Request::new(1, Platform::X, "synthetic".into(), Action::ImportLogin);
        assert!(request.validate().is_err());
        request.secret = Some(
            Secret::new(br#"{"auth_token":"synthetic-auth","ct0":"synthetic-csrf"}"#.to_vec())
                .unwrap(),
        );
        assert!(request.validate().is_ok());
        assert!(!format!("{request:?}").contains("synthetic-auth"));
        request.params = json!({"auth_token":"must-not-be-public"});
        assert!(request.validate().is_err());
        for action in [Action::Qr, Action::PasswordLogin, Action::Challenge] {
            let mut request = Request::new(1, Platform::X, "synthetic".into(), action);
            request.secret = Some(Secret::new(b"synthetic".to_vec()).unwrap());
            assert!(request.validate().is_err());
        }
    }
}
