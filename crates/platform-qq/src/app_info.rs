// SPDX-License-Identifier: MIT
//! Pinned LagrangeGo client/auth/app.go profiles. Selection is explicit; an
//! unknown/new profile is never guessed from cached credentials or server errors.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppProfile {
    #[default]
    Linux31939038,
    Linux32646494,
}
#[derive(Clone, Copy, Debug)]
pub struct AppInfo {
    pub profile: AppProfile,
    pub os: &'static str,
    pub vendor_os: &'static str,
    pub version: &'static str,
    pub app_id: u32,
    pub sub_app_id: u32,
    pub client_version: u32,
    pub package_name: &'static str,
    pub package_sign: &'static str,
    pub pt_version: &'static str,
    pub wt_login_sdk: &'static str,
    pub main_sigmap: u32,
}
pub const LINUX_319: AppInfo = AppInfo {
    profile: AppProfile::Linux31939038,
    os: "Linux",
    vendor_os: "linux",
    version: "3.2.19-39038",
    app_id: 1600001615,
    sub_app_id: 537313942,
    client_version: 39038,
    package_name: "com.tencent.qq",
    package_sign: "V1_LNX_NQ_3.2.19-39038_GW_B",
    pt_version: "2.0.0",
    wt_login_sdk: "nt.wtlogin.0.0.1",
    main_sigmap: 169742560,
};
pub const LINUX_326: AppInfo = AppInfo {
    profile: AppProfile::Linux32646494,
    version: "3.2.26-46494",
    sub_app_id: 537345891,
    client_version: 46494,
    package_sign: "V1_LNX_NQ_3.2.26-46494_GW_B",
    ..LINUX_319
};
impl AppProfile {
    pub fn is_legacy(&self) -> bool {
        *self == Self::Linux31939038
    }
    pub fn info(self) -> &'static AppInfo {
        match self {
            Self::Linux31939038 => &LINUX_319,
            Self::Linux32646494 => &LINUX_326,
        }
    }
}

impl std::str::FromStr for AppProfile {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "3.2.19-39038" | "linux31939038" => Ok(Self::Linux31939038),
            "3.2.26-46494" | "linux32646494" => Ok(Self::Linux32646494),
            _ => Err("unsupported QQ app profile".into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_selection_is_explicit_and_legacy_stays_default() {
        assert_eq!(AppProfile::default().info().version, "3.2.19-39038");
        assert_eq!(
            "3.2.26-46494"
                .parse::<AppProfile>()
                .unwrap()
                .info()
                .sub_app_id,
            537345891
        );
        assert!("latest".parse::<AppProfile>().is_err());
    }
}
