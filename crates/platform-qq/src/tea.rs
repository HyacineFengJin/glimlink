// SPDX-License-Identifier: AGPL-3.0-only
// Adapted from the Agent Runtime QQ Rust experiment; protocol references in NOTICE.
//! QQ's TEA framing, checked against the independent Go baseline.
//! Protocol reference: fumiama/gofastTEA v0.1.3 and LagrangeGo transport.
use crate::Result;
use std::io::Read;

pub fn random(bytes: &mut [u8]) -> Result<()> {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(bytes))
        .map_err(|e| e.to_string())
}
fn keys(key: &[u8]) -> Result<[u32; 4]> {
    if key.len() != 16 {
        return Err("TEA requires a 16-byte key".into());
    }
    Ok(std::array::from_fn(|i| {
        u32::from_be_bytes(key[i * 4..i * 4 + 4].try_into().unwrap())
    }))
}
fn mix(x: u32, sum: u32, a: u32, b: u32) -> u32 {
    x.wrapping_add(sum) ^ (x << 4).wrapping_add(a) ^ (x >> 5).wrapping_add(b)
}
fn block(value: u64, key: &[u32; 4], decrypt: bool) -> u64 {
    let (mut a, mut b) = ((value >> 32) as u32, value as u32);
    if decrypt {
        let mut sum = 0xe3779b90_u32;
        for _ in 0..16 {
            b = b.wrapping_sub(mix(a, sum, key[2], key[3]));
            a = a.wrapping_sub(mix(b, sum, key[0], key[1]));
            sum = sum.wrapping_sub(0x9e3779b9);
        }
    } else {
        let mut sum = 0_u32;
        for _ in 0..16 {
            sum = sum.wrapping_add(0x9e3779b9);
            a = a.wrapping_add(mix(b, sum, key[0], key[1]));
            b = b.wrapping_add(mix(a, sum, key[2], key[3]));
        }
    }
    ((a as u64) << 32) | b as u64
}
pub fn encrypt(input: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let key = keys(key)?;
    let fill = 10 - (input.len() + 1) % 8;
    let mut data = vec![0; fill + input.len() + 7];
    random(&mut data[..fill])?;
    data[0] = (fill - 3) as u8 | 0xf8;
    data[fill..fill + input.len()].copy_from_slice(input);
    let (mut iv1, mut iv2) = (0_u64, 0_u64);
    for chunk in data.chunks_exact_mut(8) {
        let plain = u64::from_be_bytes(chunk.try_into().unwrap()) ^ iv1;
        iv1 = block(plain, &key, false) ^ iv2;
        iv2 = plain;
        chunk.copy_from_slice(&iv1.to_be_bytes());
    }
    Ok(data)
}
pub fn decrypt(input: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let key = keys(key)?;
    if input.len() < 16 || input.len() % 8 != 0 {
        return Err("invalid TEA ciphertext length".into());
    }
    let mut data = Vec::with_capacity(input.len());
    let (mut previous_cipher, mut previous_plain) = (0_u64, 0_u64);
    for chunk in input.chunks_exact(8) {
        let cipher = u64::from_be_bytes(chunk.try_into().unwrap());
        previous_plain = block(previous_plain ^ cipher, &key, true);
        data.extend_from_slice(&(previous_plain ^ previous_cipher).to_be_bytes());
        previous_cipher = cipher;
    }
    let start = (data[0] & 7) as usize + 3;
    let end = data.len() - 7;
    if start > end {
        return Err("invalid TEA padding".into());
    }
    Ok(data[start..end].to_vec())
}
