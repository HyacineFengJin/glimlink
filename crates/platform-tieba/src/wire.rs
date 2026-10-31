// SPDX-License-Identifier: MIT
//! Narrow, bounded protobuf wire parser; unknown fields remain in the raw source.
use crate::{Error, Result};
#[derive(Clone, Copy)]
pub enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Fixed(&'a [u8]),
}
pub struct Fields<'a>(Vec<(u32, Value<'a>)>);
fn varint(data: &[u8], at: &mut usize) -> Result<u64> {
    let mut n = 0;
    for shift in (0..70).step_by(7) {
        let b = *data.get(*at).ok_or(Error::InvalidProto)?;
        *at += 1;
        if shift == 63 && b > 1 {
            return Err(Error::InvalidProto);
        }
        n |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err(Error::InvalidProto)
}
impl<'a> Fields<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() > 4 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        let mut fields = Vec::new();
        let mut at = 0;
        while at < data.len() {
            if fields.len() >= 65536 {
                return Err(Error::Limit);
            }
            let tag = varint(data, &mut at)?;
            let number = u32::try_from(tag >> 3).map_err(|_| Error::InvalidProto)?;
            if number == 0 || number > 0x1fff_ffff {
                return Err(Error::InvalidProto);
            }
            let value = match tag & 7 {
                0 => Value::Number(varint(data, &mut at)?),
                2 => {
                    let n =
                        usize::try_from(varint(data, &mut at)?).map_err(|_| Error::InvalidProto)?;
                    let end = at
                        .checked_add(n)
                        .filter(|e| *e <= data.len())
                        .ok_or(Error::InvalidProto)?;
                    let b = &data[at..end];
                    at = end;
                    Value::Bytes(b)
                }
                1 | 5 => {
                    let n = if tag & 7 == 1 { 8 } else { 4 };
                    let begin = at;
                    at = at
                        .checked_add(n)
                        .filter(|e| *e <= data.len())
                        .ok_or(Error::InvalidProto)?;
                    Value::Fixed(&data[begin..at])
                }
                _ => return Err(Error::InvalidProto),
            };
            fields.push((number, value));
        }
        Ok(Self(fields))
    }
    pub fn num(&self, number: u32) -> u64 {
        self.num_opt(number).unwrap_or(0)
    }
    pub fn num_opt(&self, number: u32) -> Option<u64> {
        self.0.iter().rev().find_map(|(f, v)| {
            if *f == number {
                if let Value::Number(v) = v {
                    Some(*v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub fn bytes(&self, number: u32) -> Option<&'a [u8]> {
        self.0.iter().rev().find_map(|(f, v)| {
            if *f == number {
                if let Value::Bytes(v) = v {
                    Some(*v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub fn text(&self, number: u32) -> Result<String> {
        self.bytes(number)
            .map(|v| {
                std::str::from_utf8(v)
                    .map(str::to_owned)
                    .map_err(|_| Error::InvalidProto)
            })
            .unwrap_or_else(|| Ok(String::new()))
    }
    pub fn all(&self, number: u32) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.0.iter().filter_map(move |(f, v)| {
            if *f == number {
                if let Value::Bytes(v) = v {
                    Some(*v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    pub fn fields(&self) -> impl Iterator<Item = (u32, Value<'a>)> + '_ {
        self.0.iter().copied()
    }
}
pub fn number(field: u32, value: u64, out: &mut Vec<u8>) {
    if value == 0 {
        return;
    }
    put((u64::from(field)) << 3, out);
    put(value, out)
}
pub fn bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
    if value.is_empty() {
        return;
    }
    put((u64::from(field) << 3) | 2, out);
    put(value.len() as u64, out);
    out.extend_from_slice(value)
}
pub fn value(field: u32, value: Value<'_>, out: &mut Vec<u8>) {
    match value {
        Value::Number(n) => number(field, n, out),
        Value::Bytes(b) => bytes(field, b, out),
        Value::Fixed(b) => {
            put(
                (u64::from(field) << 3) | if b.len() == 8 { 1 } else { 5 },
                out,
            );
            out.extend_from_slice(b);
        }
    }
}
fn put(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8) | 128);
        n >>= 7;
    }
    out.push(n as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_wire_and_lengths_are_bounded() {
        for data in [
            &[0_u8][..],
            &[0x0a, 255, 255, 255, 255, 127][..],
            &[8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2][..],
            &[11][..],
        ] {
            assert!(Fields::parse(data).is_err());
        }
        let mut b = Vec::new();
        number(4, 8537603600, &mut b);
        bytes(5, b"text", &mut b);
        let fields = Fields::parse(&b).unwrap();
        assert_eq!(fields.num(4), 8537603600);
        assert_eq!(fields.text(5).unwrap(), "text");
    }
}
