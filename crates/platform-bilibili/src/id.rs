// SPDX-License-Identifier: MIT
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum VideoId {
    Bv(String),
    Av(u64),
}
impl VideoId {
    pub(crate) fn query(&self) -> String {
        match self {
            Self::Bv(v) => format!("bvid={v}"),
            Self::Av(v) => format!("aid={v}"),
        }
    }
    /// Revalidate also covers callers constructing the public enum directly.
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::Bv(v)
                if v.len() == 12
                    && v.starts_with("BV1")
                    && v[3..]
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() && !b"0OIl".contains(&c)) =>
            {
                Ok(())
            }
            Self::Av(v) if *v > 0 => Ok(()),
            _ => Err(Error::InvalidId),
        }
    }
}
impl FromStr for VideoId {
    type Err = Error;
    fn from_str(input: &str) -> Result<Self> {
        if input.len() > 4096 || input.trim() != input {
            return Err(Error::InvalidId);
        }
        let value = if input.starts_with("https://") {
            let uri = crate::http::parse_url(input, crate::http::UrlRole::Video)?;
            let mut path = uri.path().split('/').filter(|p| !p.is_empty());
            if path.next() != Some("video") {
                return Err(Error::InvalidId);
            }
            let id = path.next().ok_or(Error::InvalidId)?;
            if path.next().is_some() {
                return Err(Error::InvalidId);
            }
            id.to_owned()
        } else {
            input.to_owned()
        };
        let id = if let Some(number) = value
            .strip_prefix("av")
            .or_else(|| value.strip_prefix("AV"))
        {
            if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::InvalidId);
            }
            Self::Av(number.parse().map_err(|_| Error::InvalidId)?)
        } else {
            Self::Bv(value.to_owned())
        };
        id.validate()?;
        Ok(id)
    }
}
impl fmt::Display for VideoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bv(v) => f.write_str(v),
            Self::Av(v) => write!(f, "av{v}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_are_strict_and_do_not_turn_input_into_arbitrary_urls() {
        assert_eq!(
            "BV17x411w7KC".parse::<VideoId>().unwrap().to_string(),
            "BV17x411w7KC"
        );
        assert_eq!(
            "https://www.bilibili.com/video/av170001/?p=2"
                .parse::<VideoId>()
                .unwrap(),
            VideoId::Av(170001)
        );
        for input in [
            "av0",
            "av-1",
            "av1&evil=x",
            "BV17x411w7KC?x=y",
            "http://www.bilibili.com/video/av1",
            "https://localhost/video/av1",
            "https://www.bilibili.com.evil.invalid/video/av1",
            "https://www.bilibili.com/video/av1/more",
            " av1",
        ] {
            assert!(input.parse::<VideoId>().is_err(), "{input}");
        }
    }
}
