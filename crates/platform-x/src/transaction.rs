// SPDX-License-Identifier: MIT
// Rust adaptation of XClientTransaction (c) 2025 Sarabjit Dhiman and Twikit
// (c) 2024 d60. Exact source revisions and separate licenses are in NOTICE.
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct Transaction {
    key: Vec<u8>,
    animation: String,
}
pub(crate) fn regex(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|_| Error::ProtocolChanged)
}
pub(crate) fn attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!(
        r#"(?:\s|<){}\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
        regex::escape(name)
    );
    let captures = Regex::new(&pattern).ok()?.captures(tag)?;
    Some(
        captures
            .get(1)
            .or_else(|| captures.get(2))?
            .as_str()
            .replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#39;", "'"),
    )
}
impl Transaction {
    pub(crate) fn from_pages(html: &str, script: &str) -> Result<Self> {
        let mut key_text = None;
        for tag in regex(r"(?is)<meta\b[^>]{1,4096}>")?.find_iter(html) {
            if attr(tag.as_str(), "name").as_deref() == Some("twitter-site-verification") {
                key_text = attr(tag.as_str(), "content");
                break;
            }
        }
        let key = STANDARD
            .decode(key_text.ok_or(Error::ProtocolChanged)?)
            .map_err(|_| Error::ProtocolChanged)?;
        if !(16..=128).contains(&key.len()) {
            return Err(Error::ProtocolChanged);
        }
        let indices: Vec<usize> = regex(r"\(\w\[(\d{1,3})\],\s*16\)")?
            .captures_iter(script)
            .filter_map(|c| c[1].parse().ok())
            .collect();
        if !(2..=9).contains(&indices.len()) || indices.iter().any(|i| *i >= key.len()) {
            return Err(Error::ProtocolChanged);
        }
        let frame_id = format!("loading-x-anim-{}", key[5] % 4);
        let re = regex(r"(?is)<svg\b[^>]*>.*?</svg>")?;
        let frame = re
            .find_iter(html)
            .find(|m| attr(m.as_str(), "id").as_deref() == Some(frame_id.as_str()))
            .ok_or(Error::ProtocolChanged)?;
        let paths: Vec<_> = regex(r"(?is)<path\b[^>]*>")?
            .find_iter(frame.as_str())
            .filter_map(|m| attr(m.as_str(), "d"))
            .collect();
        let path = paths.get(1).ok_or(Error::ProtocolChanged)?;
        let content = path.get(9..).ok_or(Error::ProtocolChanged)?;
        let row_text = content
            .split('C')
            .nth(key[indices[0]] as usize % 16)
            .ok_or(Error::ProtocolChanged)?;
        let values: Vec<f64> = regex(r"\d+")?
            .find_iter(row_text)
            .filter_map(|m| m.as_str().parse().ok())
            .collect();
        if values.len() != 11 || values.iter().any(|v| *v > 255.0) {
            return Err(Error::ProtocolChanged);
        }
        let ticks = indices[1..]
            .iter()
            .try_fold(1u64, |a, i| a.checked_mul((key[*i] % 16) as u64))
            .ok_or(Error::ProtocolChanged)?;
        let target = ((ticks as f64 / 10.0 + 0.5).floor() * 10.0) / 4096.0;
        Ok(Self {
            key,
            animation: animate(&values, target)?,
        })
    }
    pub(crate) fn generate(&self, method: &str, path: &str) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::ProtocolChanged)?
            .as_secs();
        let now: u32 = now
            .checked_sub(1_682_924_400)
            .ok_or(Error::ProtocolChanged)?
            .try_into()
            .map_err(|_| Error::ProtocolChanged)?;
        let mut random = [0u8];
        getrandom::getrandom(&mut random).map_err(|_| Error::Network)?;
        Ok(self.generate_fixed(method, path, now, random[0]))
    }
    fn generate_fixed(&self, method: &str, path: &str, now: u32, random: u8) -> String {
        let digest = Sha256::digest(format!(
            "{method}!{path}!{now}obfiowerehiring{}",
            self.animation
        ));
        let mut data = Vec::with_capacity(self.key.len() + 22);
        data.push(random);
        for b in self
            .key
            .iter()
            .copied()
            .chain(now.to_le_bytes())
            .chain(digest[..16].iter().copied())
            .chain([3])
        {
            data.push(b ^ random);
        }
        STANDARD.encode(data).trim_end_matches('=').to_owned()
    }
}
fn solve(value: f64, min: f64, max: f64, integer: bool) -> f64 {
    let value = value * (max - min) / 255.0 + min;
    if integer {
        value.floor()
    } else {
        (value * 100.0).round_ties_even() / 100.0
    }
}
fn calc(a: f64, b: f64, m: f64) -> f64 {
    3.0 * a * (1.0 - m) * (1.0 - m) * m + 3.0 * b * (1.0 - m) * m * m + m * m * m
}
fn curve(c: [f64; 4], time: f64) -> f64 {
    if time <= 0.0 {
        let gradient = if c[0] > 0.0 {
            c[1] / c[0]
        } else if c[1] == 0.0 && c[2] > 0.0 {
            c[3] / c[2]
        } else {
            0.0
        };
        return gradient * time;
    }
    if time >= 1.0 {
        let gradient = if c[2] < 1.0 {
            (c[3] - 1.0) / (c[2] - 1.0)
        } else if c[2] == 1.0 && c[0] < 1.0 {
            (c[1] - 1.0) / (c[0] - 1.0)
        } else {
            0.0
        };
        return 1.0 + gradient * (time - 1.0);
    }
    let (mut start, mut end, mut mid) = (0.0, 1.0, 0.0);
    for _ in 0..80 {
        mid = (start + end) / 2.0;
        let x = calc(c[0], c[2], mid);
        if (time - x).abs() < 0.00001 {
            break;
        }
        if x < time {
            start = mid;
        } else {
            end = mid;
        }
    }
    calc(c[1], c[3], mid)
}
fn hex_float(value: f64) -> String {
    let integer = value as u64;
    let mut out = if integer == 0 {
        String::new()
    } else {
        format!("{integer:X}")
    };
    let mut fraction = value - integer as f64;
    if fraction != 0.0 {
        out.push('.');
        for _ in 0..64 {
            if fraction == 0.0 {
                break;
            }
            fraction *= 16.0;
            let digit = fraction as u8;
            fraction -= digit as f64;
            out.push(
                char::from_digit(digit as u32, 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}
fn animate(frames: &[f64], time: f64) -> Result<String> {
    if frames.len() != 11 || !time.is_finite() {
        return Err(Error::ProtocolChanged);
    }
    let curves = std::array::from_fn(|i| {
        solve(
            frames[i + 7],
            if i % 2 == 1 { -1.0 } else { 0.0 },
            1.0,
            false,
        )
    });
    let value = curve(curves, time);
    let mut out = String::new();
    for i in 0..3 {
        let c = (frames[i] * (1.0 - value) + frames[i + 3] * value)
            .clamp(0.0, 255.0)
            .round_ties_even() as u64;
        out.push_str(&format!("{c:x}"));
    }
    let angle = (solve(frames[6], 60.0, 360.0, true) * value).to_radians();
    for x in [angle.cos(), -angle.sin(), angle.sin(), angle.cos()] {
        let rounded = ((x * 100.0).round_ties_even() / 100.0).abs();
        let h = hex_float(rounded);
        if h.starts_with('.') {
            out.push('0');
            out.push_str(&h.to_ascii_lowercase());
        } else if h.is_empty() {
            out.push('0');
        } else {
            out.push_str(&h);
        }
    }
    out.push_str("00");
    Ok(out.replace(['.', '-'], ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_python_reference_animation_and_digest() {
        let a = animate(
            &[50., 200., 237., 161., 221., 130., 68., 47., 29., 101., 209.],
            0.25,
        )
        .unwrap();
        assert_eq!(
            a,
            "27c6f70f851eb851eb8503ae147ae147ae203ae147ae147ae20f851eb851eb8500"
        );
        let t = Transaction {
            key: (0..32).collect(),
            animation: a,
        };
        assert_eq!(
            t.generate_fixed("GET", "/i/api/graphql/test/UserTweets", 123456789, 42),
            "KiorKCkuLywtIiMgISYnJCU6Ozg5Pj88PTIzMDE2NzQ1P+dxLSg6oZFxU1oxIzHhr9Lx7Ygp"
        );
    }
    #[test]
    fn requires_indices_and_verification_no_default_signature() {
        assert!(matches!(
            Transaction::from_pages("<html></html>", "(a[1],16)"),
            Err(Error::ProtocolChanged)
        ));
    }
    #[test]
    fn bounded_svg_and_indices() {
        let key = STANDARD.encode([16u8; 32]);
        let mut html = format!("<meta name='twitter-site-verification' content='{key}'>");
        for i in 0..4 {
            html.push_str(&format!("<svg id='loading-x-anim-{i}'><g><path d='unused'/><path d='M 10,30 {}'/></g></svg>",(0..16).map(|_|"C 50,200 237,161 221,130 h 68 s 47,29 101,209 ").collect::<String>()));
        }
        let t = Transaction::from_pages(&html, "(a[12],16)(a[15],16)(a[7],16)(a[11],16)").unwrap();
        assert!(!t.generate_fixed("GET", "/x", 1, 0).is_empty());
        assert!(Transaction::from_pages(&html, "(a[999],16)(a[15],16)").is_err());
    }
}
