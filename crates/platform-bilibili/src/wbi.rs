//! WBI's published permutation and MD5 recipe. No browser script execution.
use crate::{Error, Result};
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;

pub struct WbiKey([u8; 32]);
impl WbiKey {
    pub fn new(image_key: &str, sub_key: &str) -> Result<Self> {
        if [image_key, sub_key]
            .iter()
            .any(|v| v.len() != 32 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::InvalidSignature);
        }
        // Only the first 32 entries are needed for the truncated mixing key.
        const ORDER: [usize; 32] = [
            46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42,
            19, 29, 28, 14, 39, 12, 38, 41, 13,
        ];
        let source = format!("{image_key}{sub_key}");
        Ok(Self(std::array::from_fn(|i| source.as_bytes()[ORDER[i]])))
    }
    pub(crate) fn from_nav(data: &Value) -> Result<Self> {
        let take_key = |name| -> Result<&str> {
            let value = data
                .get("wbi_img")
                .and_then(|v| v.get(name))
                .and_then(Value::as_str)
                .ok_or(Error::Schema("missing WBI keys"))?;
            crate::http::validate_resource_url(value)?;
            value
                .rsplit('/')
                .next()
                .and_then(|v| v.strip_suffix(".png"))
                .ok_or(Error::InvalidSignature)
        };
        Self::new(take_key("img_url")?, take_key("sub_url")?)
    }
    pub fn sign(&self, input: &[(&str, String)], timestamp: u64) -> Result<String> {
        if input.len() > 64 {
            return Err(Error::InvalidSignature);
        }
        let mut params = BTreeMap::new();
        for (key, value) in input {
            if key.is_empty()
                || key.len() > 128
                || value.len() > 8192
                || matches!(*key, "w_rid" | "wts")
                || params.contains_key(key)
            {
                return Err(Error::InvalidSignature);
            }
            params.insert(
                *key,
                value
                    .chars()
                    .filter(|c| !"!'()*".contains(*c))
                    .collect::<String>(),
            );
        }
        params.insert("wts", timestamp.to_string());
        let query = params
            .iter()
            .map(|(key, value)| format!("{}={}", percent(key), percent(value)))
            .collect::<Vec<_>>()
            .join("&");
        let mut hash = Md5::new();
        hash.update(query.as_bytes());
        hash.update(self.0);
        Ok(format!("{query}&w_rid={:x}", hash.finalize()))
    }
}
fn percent(input: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}
pub(crate) fn unix_seconds() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| Error::InvalidSignature)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> WbiKey {
        WbiKey::new(
            "7cd084941338484aae1ad9425b84077c",
            "4932caff0ff746eab6f01bf08b70ac45",
        )
        .unwrap()
    }
    #[test]
    fn published_wbi_vector_and_component_encoding() {
        let query = key()
            .sign(
                &[
                    ("foo", "114".into()),
                    ("bar", "514".into()),
                    ("zab", "1919810".into()),
                ],
                1702204169,
            )
            .unwrap();
        assert_eq!(
            query,
            "bar=514&foo=114&wts=1702204169&zab=1919810&w_rid=8f6f2b5b3d485fe1886cec6a0be8c5d4"
        );
        assert_eq!(percent("五一四 one"), "%E4%BA%94%E4%B8%80%E5%9B%9B%20one");
        assert!(
            key()
                .sign(&[("x", "!'()* a".into())], 1)
                .unwrap()
                .starts_with("wts=1&x=%20a&")
        );
        assert!(key().sign(&[("wts", "1".into())], 1).is_err());
        assert!(WbiKey::new("short", "short").is_err());
    }
}
