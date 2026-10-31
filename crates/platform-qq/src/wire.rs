// SPDX-License-Identifier: MIT
// Adapted from the Agent Runtime QQ Rust experiment; protocol references in NOTICE.
use crate::Result;

pub const MAX_FRAME: usize = 4 * 1024 * 1024;

pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    pub fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).ok_or("length overflow")?;
        let value = self.data.get(self.at..end).ok_or("truncated packet")?;
        self.at = end;
        Ok(value)
    }
    pub fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn packet(&mut self) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        if !(4..=MAX_FRAME).contains(&length) {
            return Err("invalid packet field length".into());
        }
        self.take(length - 4)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.at..]
    }
}

pub fn packet(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 4);
    out.extend_from_slice(&((value.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(value);
    out
}

pub fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
pub fn number(field: u32, value: u64, out: &mut Vec<u8>) {
    varint((field as u64) << 3, out);
    varint(value, out);
}
pub fn bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
    varint(((field as u64) << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

#[derive(Clone, Copy)]
pub enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Fixed,
    Fixed32(u32),
}
pub struct Fields<'a>(pub Vec<(u32, Value<'a>)>);
impl<'a> Fields<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        fn get(r: &mut Reader<'_>) -> Result<u64> {
            let mut value = 0_u64;
            for shift in (0..70).step_by(7) {
                let b = r.byte()?;
                if shift == 63 && b > 1 {
                    return Err("protobuf varint overflow".into());
                }
                value |= ((b & 127) as u64) << shift;
                if b < 128 {
                    return Ok(value);
                }
            }
            Err("protobuf varint too long".into())
        }
        if data.len() > MAX_FRAME {
            return Err("protobuf exceeds limit".into());
        }
        let mut r = Reader::new(data);
        let mut values = Vec::new();
        while !r.rest().is_empty() {
            if values.len() >= 8192 {
                return Err("protobuf field limit".into());
            }
            let tag = get(&mut r)?;
            let field = tag >> 3;
            if field == 0 || field > 0x1fff_ffff {
                return Err("invalid protobuf field".into());
            }
            let value = match tag & 7 {
                0 => Value::Number(get(&mut r)?),
                1 => {
                    r.take(8)?;
                    Value::Fixed
                }
                2 => {
                    let length =
                        usize::try_from(get(&mut r)?).map_err(|_| "protobuf length overflow")?;
                    Value::Bytes(r.take(length)?)
                }
                5 => Value::Fixed32(u32::from_le_bytes(r.take(4)?.try_into().unwrap())),
                _ => return Err("unsupported protobuf wire type".into()),
            };
            values.push((field as u32, value));
        }
        Ok(Self(values))
    }
    pub fn data(&self, field: u32) -> Option<&'a [u8]> {
        self.0.iter().find_map(|(f, v)| match v {
            Value::Bytes(b) if *f == field => Some(*b),
            _ => None,
        })
    }
    pub fn num(&self, field: u32) -> Option<u64> {
        self.0.iter().find_map(|(f, v)| match v {
            Value::Number(n) if *f == field => Some(*n),
            _ => None,
        })
    }
}
