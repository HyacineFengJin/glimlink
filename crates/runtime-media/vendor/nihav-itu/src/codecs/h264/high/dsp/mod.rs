#[allow(unexpected_cfgs)]
mod mc;
pub use mc::{H264MC, McBlock};
//#[cfg(target_arch="x86_64")]
//use std::arch::asm;

pub const CHROMA_QUANTS: [u8; 52] = [
     0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11, 12, 13, 14, 15,
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 29, 30,
    31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38,
    39, 39, 39, 39
];

pub const CHROMA_DC_SCAN: [usize; 4] = [ 0, 1, 2, 3];
pub const ZIGZAG: [usize; 16] = [
    0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15
];
pub const ZIGZAG1: [usize; 15] = [
    0, 3, 7, 4, 1, 2, 5, 8, 11, 12, 9, 6, 10, 13, 14
];
/*pub const IL_SCAN: [usize; 16] = [
    0, 4, 1, 8, 12, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15
];*/
pub const ZIGZAG8X8: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63
];

const LEVEL_SCALE: [[i16; 6]; 3] = [
    [ 10, 11, 13, 14, 16, 18 ],
    [ 16, 18, 20, 23, 25, 29 ],
    [ 13, 14, 16, 18, 20, 23 ]
];

pub fn chroma_dc_transform(blk: &mut [i16; 4], qp: u8) {
    let t0 = blk[0] + blk[2];
    let t1 = blk[0] - blk[2];
    let t2 = blk[1] + blk[3];
    let t3 = blk[1] - blk[3];
    blk[0] = t0 + t2;
    blk[1] = t0 - t2;
    blk[2] = t1 + t3;
    blk[3] = t1 - t3;
    if qp < 6 {
        let mul = LEVEL_SCALE[0][qp as usize];
        for el in blk.iter_mut() {
            *el = el.wrapping_mul(mul) >> 1;
        }
    } else {
        let mul = LEVEL_SCALE[0][(qp % 6) as usize];
        let shift = qp / 6 - 1;
        for el in blk.iter_mut() {
            *el = el.wrapping_mul(mul) << shift;
        }
    }
}

macro_rules! transform {
    (luma_dc; $a: expr, $b: expr, $c: expr, $d: expr) => ({
        let t0 = $a.wrapping_add($c);
        let t1 = $a.wrapping_sub($c);
        let t2 = $b.wrapping_add($d);
        let t3 = $b.wrapping_sub($d);
        $a = t0.wrapping_add(t2);
        $b = t1.wrapping_add(t3);
        $c = t1.wrapping_sub(t3);
        $d = t0.wrapping_sub(t2);
    });
    ($a: expr, $b: expr, $c: expr, $d: expr, $shift: expr) => ({
        let t0 = $a.wrapping_add($c);
        let t1 = $a.wrapping_sub($c);
        let t2 = ($b >> 1).wrapping_sub($d);
        let t3 = $b.wrapping_add($d >> 1);
        let bias = 1 << $shift >> 1;
        $a = t0.wrapping_add(t3).wrapping_add(bias) >> $shift;
        $b = t1.wrapping_add(t2).wrapping_add(bias) >> $shift;
        $c = t1.wrapping_sub(t2).wrapping_add(bias) >> $shift;
        $d = t0.wrapping_sub(t3).wrapping_add(bias) >> $shift;
    });
    ($a: expr, $b: expr, $c: expr, $d: expr, $e: expr, $f: expr, $g: expr, $h: expr) => {
        let e0 = $a + $e;
        let e1 = -$d + $f - $h - ($h >> 1);
        let e2 = $a - $e;
        let e3 = $b + $h - $d - ($d >> 1);
        let e4 = ($c >> 1) - $g;
        let e5 = -$b + $h + $f + ($f >> 1);
        let e6 = $c + ($g >> 1);
        let e7 = $d + $f + $b + ($b >> 1);

        let f0 = e0 + e6;
        let f1 = e1 + (e7 >> 2);
        let f2 = e2 + e4;
        let f3 = e3 + (e5 >> 2);
        let f4 = e2 - e4;
        let f5 = (e3 >> 2) - e5;
        let f6 = e0 - e6;
        let f7 = e7 - (e1 >> 2);

        $a = f0 + f7;
        $b = f2 + f5;
        $c = f4 + f3;
        $d = f6 + f1;
        $e = f6 - f1;
        $f = f4 - f3;
        $g = f2 - f5;
        $h = f0 - f7;
    };
}

pub fn idct_luma_dc(blk: &mut [i16; 16], qp: u8) {
    if qp < 12 {
        let mul = LEVEL_SCALE[0][(qp % 6) as usize];
        let shift = 2 - qp / 6;
        let bias = 1 << shift >> 1;
        for el in blk.iter_mut() {
            *el = el.wrapping_mul(mul).wrapping_add(bias) >> shift;
        }
    } else {
        let mul = LEVEL_SCALE[0][(qp % 6) as usize];
        let shift = qp / 6 - 2;
        for el in blk.iter_mut() {
            *el = el.wrapping_mul(mul) << shift;
        }
    }
    for i in 0..4 {
        transform!(luma_dc; blk[i], blk[i + 4], blk[i + 8], blk[i + 12]);
    }
    for row in blk.chunks_exact_mut(4) {
        transform!(luma_dc; row[0], row[1], row[2], row[3]);
    }
}

pub fn idct_skip_dc(blk: &mut [i16; 16], qp: u8) {
    const BLK_INDEX: [usize; 16] = [
        0, 2, 0, 2,
        2, 1, 2, 1,
        0, 2, 0, 2,
        2, 1, 2, 1
    ];
    let qidx = (qp % 6) as usize;
    let shift = qp / 6;
    for (el, &idx) in blk.iter_mut().zip(BLK_INDEX.iter()).skip(1) {
        *el = (*el * LEVEL_SCALE[idx][qidx]) << shift;
    }
    for row in blk.chunks_exact_mut(4) {
        transform!(row[0], row[1], row[2], row[3], 0);
    }
    for i in 0..4 {
        transform!(blk[i], blk[i + 4], blk[i + 8], blk[i + 12], 6);
    }
}

pub fn idct(blk: &mut [i16; 16], qp: u8) {
    const BLK_INDEX: [usize; 16] = [
        0, 2, 0, 2,
        2, 1, 2, 1,
        0, 2, 0, 2,
        2, 1, 2, 1
    ];
    let qidx = (qp % 6) as usize;
    let shift = qp / 6;
    for (el, &idx) in blk.iter_mut().zip(BLK_INDEX.iter()) {
        *el = (*el * LEVEL_SCALE[idx][qidx]) << shift;
    }
    for row in blk.chunks_exact_mut(4) {
        transform!(row[0], row[1], row[2], row[3], 0);
    }
    for i in 0..4 {
        transform!(blk[i], blk[i + 4], blk[i + 8], blk[i + 12], 6);
    }
}

pub fn idct_dc(blk: &mut [i16; 16], qp: u8, quant_dc: bool) {
    let dc = if quant_dc {
            (blk[0] * LEVEL_SCALE[0][(qp % 6) as usize]) << (qp / 6)
        } else {
            blk[0]
        };
    *blk  = [(dc + 0x20) >> 6; 16];
}

const QMAT_8X8: [[u8; 16]; 6] = [
  [
    20, 19, 25, 19,
    19, 18, 24, 18,
    25, 24, 32, 24,
    19, 18, 24, 18
  ], [
    22, 21, 28, 21,
    21, 19, 26, 19,
    28, 26, 35, 26,
    21, 19, 26, 19
  ], [
    26, 24, 33, 24,
    24, 23, 31, 23,
    33, 31, 42, 31,
    24, 23, 31, 23
  ], [
    28, 26, 35, 26,
    26, 25, 33, 25,
    35, 33, 45, 33,
    26, 25, 33, 25
  ], [
    32, 30, 40, 30,
    30, 28, 38, 28,
    40, 38, 51, 38,
    30, 28, 38, 28
  ], [
    36, 34, 46, 34,
    34, 32, 43, 32,
    46, 43, 58, 43,
    34, 32, 43, 32
  ]
];

pub fn dequant8x8(blk: &mut [i16; 64], slist: &[u8; 64]) {
    for (el, &scan) in blk.iter_mut().zip(ZIGZAG8X8.iter()) {
        if *el != 0 {
            *el = el.wrapping_mul(i16::from(slist[scan]));
        }
    }
}

pub fn idct8x8(blk: &mut [i16; 64], qp: u8) {
    let mut tmp = [0i32; 64];
    let qmat = &QMAT_8X8[(qp % 6) as usize];
    if qp >= 36 {
        let shift = qp / 6 - 6;
        for (i, (dst, &src)) in tmp.iter_mut().zip(blk.iter()).enumerate() {
            let x = i & 7;
            let y = i >> 3;
            let idx = (x & 3) + (y & 3) * 4;
            *dst = i32::from(src).wrapping_mul(i32::from(qmat[idx])) << shift;
        }
    } else {
        let shift = 6 - qp / 6;
        let bias = (1 << shift) >> 1;
        for (i, (dst, &src)) in tmp.iter_mut().zip(blk.iter()).enumerate() {
            let x = i & 7;
            let y = i >> 3;
            let idx = (x & 3) + (y & 3) * 4;
            *dst = i32::from(src).wrapping_mul(i32::from(qmat[idx])).wrapping_add(bias) >> shift;
        }
    }
    for row in tmp.chunks_exact_mut(8) {
        transform!(row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]);
    }
    for col in 0..8 {
        transform!(tmp[col], tmp[col + 8], tmp[col + 8 * 2], tmp[col + 8 * 3],
                   tmp[col + 8 * 4], tmp[col + 8 * 5], tmp[col + 8 * 6], tmp[col + 8 * 7]);
    }
    for (dst, &src) in blk.iter_mut().zip(tmp.iter()) {
        *dst = ((src + 0x20) >> 6) as i16;
    }
}

macro_rules! depth_funcs {
    ($name_add:ident, $name_add8:ident, $clip_name:ident,
     $name_dc:ident, $dc4:ident, $dc8y:ident, $dc8c:ident, $dc16:ident,
     $plane8:ident, $plane16:ident, $val:expr) => {
        fn $name_add(dst: &mut [u16], offset: usize, stride: usize, coeffs: &[i16]) {
            let out = &mut dst[offset..][..stride * 3 + 4];
            for (line, src) in out.chunks_mut(stride).take(4).zip(coeffs.chunks_exact(4)) {
                for (dst, src) in line.iter_mut().take(4).zip(src.iter()) {
                    *dst = ((*dst as i16) + *src).clamp(0, $val * 2 - 1) as u16;
                }
            }
        }
        fn $name_add8(dst: &mut [u16], offset: usize, stride: usize, coeffs: &[i16; 64]) {
            let out = &mut dst[offset..];
            for (line, src) in out.chunks_mut(stride).take(8).zip(coeffs.chunks_exact(8)) {
                for (dst, src) in line.iter_mut().take(8).zip(src.iter()) {
                    *dst = ((*dst as i16) + *src).clamp(0, $val * 2 - 1) as u16;
                }
            }
        }
        fn $clip_name(val: i16) -> u16 { val.clamp(0, $val * 2 - 1) as u16 }
        fn $name_dc(buf: &mut [u16], stride: usize, bsize: usize) {
            for row in buf.chunks_mut(stride).take(bsize) {
                for el in row[..bsize].iter_mut() {
                    *el = $val;
                }
            }
        }
        fn $dc4(buf: &mut [u16], stride: usize, _top: &[u16], _left: &[u16], _tr: &[u16]) {
            $name_dc(buf, stride, 4);
        }
        fn $dc8y(buf: &mut [u16], stride: usize, _ctx: &IPred8Context) {
            $name_dc(buf, stride, 8);
        }
        fn $dc8c(buf: &mut [u16], stride: usize, _top: &[u16], _left: &[u16]) {
            $name_dc(buf, stride, 8);
        }
        fn $dc16(buf: &mut [u16], stride: usize, _top: &[u16], _left: &[u16]) {
            $name_dc(buf, stride, 16);
        }
        fn $plane8(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16]) {
            let mut h: i32 = 4 * (i32::from(top[7]) - i32::from(left[0]));
            let mut v: i32 = 4 * (i32::from(left[8]) - i32::from(left[0]));
            for i in 0..3 {
                let i1 = (i + 1) as i32;
                h += i1 * (i32::from(top[4 + i]) - i32::from(top[2 - i]));
                v += i1 * (i32::from(left[5 + i]) - i32::from(left[3 - i]));
            }
            let b = (17 * h + 16) >> 5;
            let c = (17 * v + 16) >> 5;
            let mut a = 16 * (i32::from(left[8]) + i32::from(top[7])) - 3 * (b + c) + 16;
            for line in buf.chunks_mut(stride).take(8) {
                let mut acc = a;
                for el in line.iter_mut().take(8) {
                    *el = $clip_name((acc >> 5) as i16);
                    acc += b;
                }
                a += c;
            }
        }
        fn $plane16(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16]) {
            let mut h = 8 * (i32::from(top[15]) - i32::from(left[0]));
            let mut v = 8 * (i32::from(left[16]) - i32::from(left[0]));
            for k in 0..7 {
                h += ((k as i32) + 1) * (i32::from(top[8 + k])  - i32::from(top[6 - k]));
                v += ((k as i32) + 1) * (i32::from(left[9 + k]) - i32::from(left[7 - k]));
            }

            h = (5 * h + 32) >> 6;
            v = (5 * v + 32) >> 6;

            let mut a = 16 * (i32::from(left[16]) + i32::from(top[15]) + 1) - 7 * (v + h);

            for row in buf.chunks_mut(stride).take(16) {
                let mut b = a;
                a += v;

                for dst in row.chunks_exact_mut(4).take(4) {
                    dst[0] = $clip_name(((b      ) >> 5) as i16);
                    dst[1] = $clip_name(((b +   h) >> 5) as i16);
                    dst[2] = $clip_name(((b + 2*h) >> 5) as i16);
                    dst[3] = $clip_name(((b + 3*h) >> 5) as i16);
                    b += h * 4;
                }
            }
        }
    }
}

depth_funcs!(add_coeffs_10, add_coeffs8_10, clip10,
    ipred_dc128_10, ipred_4x4_dc128_10, ipred_y_8x8_dc128_10, ipred_8x8_dc128_10, ipred_16x16_dc128_10,
    ipred_8x8_plane_10, ipred_16x16_plane_10, 0x200);

fn ipred_ver(buf: &mut [u16], stride: usize, top: &[u16], bsize: usize) {
    for row in buf.chunks_mut(stride).take(bsize) {
        row[..bsize].copy_from_slice(&top[..bsize]);
    }
}
fn ipred_hor(buf: &mut [u16], stride: usize, left: &[u16], bsize: usize) {
    for (row, &left) in buf.chunks_mut(stride).zip(left[1..].iter()).take(bsize) {
        for el in row[..bsize].iter_mut() {
            *el = left;
        }
    }
}
fn ipred_dc(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], bsize: usize, shift: u8) {
    let mut adc: u16 = 0;
    for i in 0..bsize { adc += top[i]; }
    for i in 0..bsize { adc += left[i + 1]; }
    let dc = (adc + (1 << (shift - 1))) >> shift;

    for row in buf.chunks_mut(stride).take(bsize) {
        for el in row[..bsize].iter_mut() {
            *el = dc;
        }
    }
}
fn ipred_left_dc(buf: &mut [u16], stride: usize, left: &[u16], bsize: usize, shift: u8) {
    let mut adc: u16 = 0;
    for i in 0..bsize { adc += left[i + 1]; }
    let dc = (adc + (1 << (shift - 1))) >> shift;

    for row in buf.chunks_mut(stride).take(bsize) {
        for el in row[..bsize].iter_mut() {
            *el = dc;
        }
    }
}
fn ipred_top_dc(buf: &mut [u16], stride: usize, top: &[u16], bsize: usize, shift: u8) {
    let mut adc: u16 = 0;
    for i in 0..bsize { adc += top[i]; }
    let dc = (adc + (1 << (shift - 1))) >> shift;

    for row in buf.chunks_mut(stride).take(bsize) {
        for el in row[..bsize].iter_mut() {
            *el = dc;
        }
    }
}

fn load(dst: &mut [u16], src: &[u16]) {
    for (dst, &src) in dst.iter_mut().zip(src.iter()) {
        *dst = src;
    }
}

fn ipred_4x4_ver(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16], _tr: &[u16]) {
    ipred_ver(buf, stride, top, 4);
}
fn ipred_4x4_hor(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16], _tr: &[u16]) {
    ipred_hor(buf, stride, left, 4);
}
fn ipred_4x4_diag_down_left(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16], tr: &[u16]) {
    let mut t: [u16; 9] = [0; 9];
    load(&mut t[..4], top);
    load(&mut t[4..8], tr);
    t[8] = t[7];

    for i in 0..4 {
        buf[i] = (t[i]     + 2 * t[i + 1] + t[i + 2] + 2) >> 2;
    }
    let dst = &mut buf[stride..];
    for i in 0..4 {
        dst[i] = (t[i + 1] + 2 * t[i + 2] + t[i + 3] + 2) >> 2;
    }
    let dst = &mut buf[stride * 2..];
    for i in 0..4 {
        dst[i] = (t[i + 2] + 2 * t[i + 3] + t[i + 4] + 2) >> 2;
    }
    let dst = &mut buf[stride * 3..];
    for i in 0..4 {
        dst[i] = (t[i + 3] + 2 * t[i + 4] + t[i + 5] + 2) >> 2;
    }
}
fn ipred_4x4_diag_down_right(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], _tr: &[u16]) {
    let mut t: [u16; 5] = [0; 5];
    t[0] = left[0];
    load(&mut t[1..], top);
    let mut l: [u16; 5] = [0; 5];
    load(&mut l, left);
    let dst = buf;

    for j in 0..4 {
        for i in 0..j {
            dst[i + j * stride] = (l[j - i - 1] + 2 * l[j - i] + l[j - i + 1] + 2) >> 2;
        }
        dst[j + j * stride] = (l[1] + 2 * l[0] + t[1] + 2) >> 2;
        for i in (j+1)..4 {
            dst[i + j * stride] = (t[i - j - 1] + 2 * t[i - j] + t[i - j + 1] + 2) >> 2;
        }
    }
}
fn ipred_4x4_ver_right(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], _tr: &[u16]) {
    let mut t: [u16; 5] = [0; 5];
    t[0] = left[0];
    load(&mut t[1..], top);
    let mut l: [u16; 5] = [0; 5];
    load(&mut l, left);
    let dst = buf;

    for j in 0..4 {
        for i in 0..4 {
            let zvr = ((2 * i) as i8) - (j as i8);
            let pix;
            if zvr >= 0 {
                if (zvr & 1) == 0 {
                    pix = (t[i - (j >> 1)] + t[i - (j >> 1) + 1] + 1) >> 1;
                } else {
                    pix = (t[i - (j >> 1) - 1] + 2 * t[i - (j >> 1)] + t[i - (j >> 1) + 1] + 2) >> 2;
                }
            } else {
                if zvr == -1 {
                    pix = (l[1] + 2 * l[0] + t[1] + 2) >> 2;
                } else {
                    pix = (l[j] + 2 * l[j - 1] + l[j - 2] + 2) >> 2;
                }
            }
            dst[i + j * stride] = pix;
        }
    }
}
fn ipred_4x4_ver_left(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16], tr: &[u16]) {
    let mut t: [u16; 8] = [0; 8];
    load(&mut t[..4], top);
    load(&mut t[4..], tr);
    let dst = buf;

    dst[0 + 0 * stride] = (t[0] + t[1] + 1) >> 1;
    let pix = (t[1] + t[2] + 1) >> 1;
    dst[1 + 0 * stride] = pix;
    dst[0 + 2 * stride] = pix;
    let pix = (t[2] + t[3] + 1) >> 1;
    dst[2 + 0 * stride] = pix;
    dst[1 + 2 * stride] = pix;
    let pix = (t[3] + t[4] + 1) >> 1;
    dst[3 + 0 * stride] = pix;
    dst[2 + 2 * stride] = pix;
    dst[3 + 2 * stride] = (t[4] + t[5] + 1) >> 1;
    dst[0 + 1 * stride] = (t[0] + 2*t[1] + t[2] + 2) >> 2;
    let pix = (t[1] + 2*t[2] + t[3] + 2) >> 2;
    dst[1 + 1 * stride] = pix;
    dst[0 + 3 * stride] = pix;
    let pix = (t[2] + 2*t[3] + t[4] + 2) >> 2;
    dst[2 + 1 * stride] = pix;
    dst[1 + 3 * stride] = pix;
    let pix = (t[3] + 2*t[4] + t[5] + 2) >> 2;
    dst[3 + 1 * stride] = pix;
    dst[2 + 3 * stride] = pix;
    dst[3 + 3 * stride] = (t[4] + 2*t[5] + t[6] + 2) >> 2;
}
fn ipred_4x4_hor_down(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], _tr: &[u16]) {
    let mut t: [u16; 5] = [0; 5];
    t[0] = left[0];
    load(&mut t[1..], top);
    let mut l: [u16; 5] = [0; 5];
    load(&mut l, left);
    let dst = buf;

    for j in 0..4 {
        for i in 0..4 {
            let zhd = ((2 * j) as i8) - (i as i8);
            let pix;
            if zhd >= 0 {
                if (zhd & 1) == 0 {
                    pix = (l[j - (i >> 1)] + l[j - (i >> 1) + 1] + 1) >> 1;
                } else {
                    pix = (l[j - (i >> 1) - 1] + 2 * l[j - (i >> 1)] + l[j - (i >> 1) + 1] + 2) >> 2;
                }
            } else {
                if zhd == -1 {
                    pix = (l[1] + 2 * l[0] + t[1] + 2) >> 2;
                } else {
                    pix = (t[i - 2] + 2 * t[i - 1] + t[i] + 2) >> 2;
                }
            }
            dst[i + j * stride] = pix;
        }
    }
}
fn ipred_4x4_hor_up(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16], _tr: &[u16]) {
    let mut l: [u16; 8] = [0; 8];
    load(&mut l, &left[1..]);
    let dst = buf;

    dst[0 + 0 * stride] = (l[0] + l[1] + 1) >> 1;
    dst[1 + 0 * stride] = (l[0] + 2*l[1] + l[2] + 2) >> 2;
    let pix = (l[1] + l[2] + 1) >> 1;
    dst[2 + 0 * stride] = pix;
    dst[0 + 1 * stride] = pix;
    let pix = (l[1] + 2*l[2] + l[3] + 2) >> 2;
    dst[3 + 0 * stride] = pix;
    dst[1 + 1 * stride] = pix;
    let pix = (l[2] + l[3] + 1) >> 1;
    dst[2 + 1 * stride] = pix;
    dst[0 + 2 * stride] = pix;
    let pix = (l[2] + 3*l[3] + 2) >> 2;
    dst[3 + 1 * stride] = pix;
    dst[1 + 2 * stride] = pix;
    dst[3 + 2 * stride] = l[3];
    dst[1 + 3 * stride] = l[3];
    dst[0 + 3 * stride] = l[3];
    dst[2 + 2 * stride] = l[3];
    dst[2 + 3 * stride] = l[3];
    dst[3 + 3 * stride] = l[3];
}
fn ipred_4x4_dc(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], _tr: &[u16]) {
    ipred_dc(buf, stride, top, left, 4, 3);
}
fn ipred_4x4_left_dc(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16], _tr: &[u16]) {
    ipred_left_dc(buf, stride, left, 4, 2);
}
fn ipred_4x4_top_dc(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16], _tr: &[u16]) {
    ipred_top_dc(buf, stride, top, 4, 2);
}

pub struct IPred8Context {
    pub t:      [u16; 16],
    pub l:      [u16; 8],
    pub tl:     u16,
    pub fill:   u16,
}

impl IPred8Context {
    pub fn new(fill: u16) -> Self {
        Self {
            t:      [fill; 16],
            l:      [fill; 8],
            tl:     fill,
            fill,
        }
    }
    pub fn fill(&mut self, top: &[u16], left: &[u16], has_t: bool, has_tr: bool, has_l: bool, has_tl: bool) {
        let mut t = [self.fill; 19];
        let mut l = [self.fill; 11];
        if has_t {
            t[1..8 + 1].copy_from_slice(&top[..8]);
        }
        if has_tr {
            t[8 + 1..16 + 1].copy_from_slice(&top[8..][..8]);
            t[16 + 1] = t[15 + 1];
            t[17 + 1] = t[15 + 1];
        } else {
            let (t0, t1) = t.split_at_mut(8 + 1);
            for el in t1.iter_mut() {
                *el = t0[7 + 1];
            }
        }
        if has_l {
            l[1..9].copy_from_slice(&left[1..9]);
            l[8 + 1] = l[7 + 1];
            l[9 + 1] = l[7 + 1];
        }
        if has_tl {
            t[0] = left[0];
            l[0] = left[0];
        } else {
            t[0] = t[1];
            l[0] = l[1];
        }

        for i in 0..16 {
            self.t[i] = (t[i] + 2 * t[i + 1] + t[i + 2] + 2) >> 2;
        }
        for i in 0..8 {
            self.l[i] = (l[i] + 2 * l[i + 1] + l[i + 2] + 2) >> 2;
        }
        self.tl = if has_t && has_l {
                (t[1] + 2 * t[0] + l[1] + 2) >> 2
            } else if has_t {
                (3 * t[0] + t[1] + 2) >> 2
            } else if has_l {
                (3 * l[0] + l[1] + 2) >> 2
            } else {
                t[0]
            };
    }
}

fn ipred_y_8x8_ver(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    for row in buf.chunks_mut(stride).take(8) {
        row[..8].copy_from_slice(&ctx.t[..8]);
    }
}
fn ipred_y_8x8_hor(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    for (row, &l) in buf.chunks_mut(stride).zip(ctx.l.iter()).take(8) {
        row[..8].copy_from_slice(&[l; 8]);
    }
}
fn ipred_y_8x8_diag_down_left(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut t = [0u16; 16];
    load(&mut t, &ctx.t);

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            *pix = (if (x != 7) || (y != 7) {
                    t[x + y] + 2 * t[x + y + 1] + t[x + y + 2]
                } else {
                    t[14] + 3 * t[15]
                } + 2) >> 2;
        }
    }
}
fn ipred_y_8x8_diag_down_right(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut t = [0u16; 9];
    t[0] = ctx.tl;
    load(&mut t[1..], &ctx.t);
    let mut l = [0u16; 9];
    l[0] = ctx.tl;
    load(&mut l[1..], &ctx.l);
    let diag = t[1] + 2 * t[0] + l[1];

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            *pix = (if x > y {
                    t[x - y - 1] + 2 * t[x - y] + t[x - y + 1]
                } else if x < y {
                    l[y - x - 1] + 2 * l[y - x] + l[y - x + 1]
                } else {
                    diag
                } + 2) >> 2;
        }
    }
}
fn ipred_y_8x8_ver_right(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut t = [0u16; 9];
    t[0] = ctx.tl;
    load(&mut t[1..], &ctx.t);
    let mut l = [0u16; 9];
    l[0] = ctx.tl;
    load(&mut l[1..], &ctx.l);

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            let zvr = 2 * (x as i8) - (y as i8);
            *pix = if zvr >= 0 {
                    let ix = x - (y >> 1);
                    if (zvr & 1) == 0 {
                        (t[ix] + t[ix + 1] + 1) >> 1
                    } else {
                        (t[ix - 1] + 2 * t[ix] + t[ix + 1] + 2) >> 2
                    }
                } else if zvr == -1 {
                    (l[1] + 2 * l[0] + t[1] + 2) >> 2
                } else {
                    let ix = y - 2 * x;
                    (l[ix] + 2 * l[ix - 1] + l[ix - 2] + 2) >> 2
                };
        }
    }
}
fn ipred_y_8x8_ver_left(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut t = [0u16; 16];
    load(&mut t, &ctx.t);

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            let ix = x + (y >> 1);
            *pix = if (y & 1) == 0 {
                    (t[ix] + t[ix + 1] + 1) >> 1
                } else {
                    (t[ix] + 2 * t[ix + 1] + t[ix + 2] + 2) >> 2
                };
        }
    }

}
fn ipred_y_8x8_hor_down(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut t = [0u16; 9];
    t[0] = ctx.tl;
    load(&mut t[1..], &ctx.t);
    let mut l = [0u16; 9];
    l[0] = ctx.tl;
    load(&mut l[1..], &ctx.l);

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            let zhd = 2 * (y as i8) - (x as i8);
            *pix = if zhd >= 0 {
                    let ix = y - (x >> 1);
                    if (zhd & 1) == 0 {
                        (l[ix] + l[ix + 1] + 1) >> 1
                    } else {
                        (l[ix - 1] + 2 * l[ix] + l[ix + 1] + 2) >> 2
                    }
                } else if zhd == -1 {
                    (l[1] + 2 * l[0] + t[1] + 2) >> 2
                } else {
                    let ix = x - 2 * y;
                    (t[ix] + 2 * t[ix - 1] + t[ix - 2] + 2) >> 2
                };
        }
    }
}
fn ipred_y_8x8_hor_up(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut l = [0u16; 8];
    load(&mut l, &ctx.l);

    for (y, row) in buf.chunks_mut(stride).take(8).enumerate() {
        for (x, pix) in row.iter_mut().take(8).enumerate() {
            let zhu = x + 2 * y;
            let ix = y + (x >> 1);
            *pix = if zhu > 13 {
                    l[7]
                } else if zhu == 13 {
                    (l[6] + 3 * l[7] + 2) >> 2
                } else if (zhu & 1) != 0 {
                    (l[ix] + 2 * l[ix + 1] + l[ix + 2] + 2) >> 2
                } else {
                    (l[ix] + l[ix + 1] + 1) >> 1
                };
        }
    }
}
fn ipred_y_8x8_dc(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut sum = 0u16;
    for &t in ctx.t[..8].iter() {
        sum += t;
    }
    for &l in ctx.l[..8].iter() {
        sum += l;
    }
    let dc = (sum + 8) >> 4;
    for row in buf.chunks_mut(stride).take(8) {
        for pix in row.iter_mut().take(8) {
            *pix = dc;
        }
    }
}
fn ipred_y_8x8_left_dc(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut sum = 0u16;
    for &l in ctx.l[..8].iter() {
        sum += l;
    }
    let dc = (sum + 4) >> 3;
    for row in buf.chunks_mut(stride).take(8) {
        for pix in row.iter_mut().take(8) {
            *pix = dc;
        }
    }
}
fn ipred_y_8x8_top_dc(buf: &mut [u16], stride: usize, ctx: &IPred8Context) {
    let mut sum = 0u16;
    for &t in ctx.t[..8].iter() {
        sum += t;
    }
    let dc = (sum + 4) >> 3;
    for row in buf.chunks_mut(stride).take(8) {
        for pix in row.iter_mut().take(8) {
            *pix = dc;
        }
    }
}

fn ipred_8x8_ver(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16]) {
    ipred_ver(buf, stride, top, 8);
}
fn ipred_8x8_hor(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16]) {
    ipred_hor(buf, stride, left, 8);
}
fn ipred_8x8_dc(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16]) {
    let mut l = [0; 8];
    load(&mut l, &left[1..]);
    let mut t = [0; 8];
    load(&mut t, top);

    let dc0 = (t[0] + t[1] + t[2] + t[3] + l[0] + l[1] + l[2] + l[3] + 4) >> 3;
    let sum1 = t[4] + t[5] + t[6] + t[7];
    let dc1 = (sum1 + 2) >> 2;
    let sum2 = l[4] + l[5] + l[6] + l[7];
    let dc2 = (sum2 + 2) >> 2;
    let dc3 = (sum1 + sum2 + 4) >> 3;

    for row in buf.chunks_mut(stride).take(4) {
        row[..4].copy_from_slice(&[dc0; 4]);
        row[4..8].copy_from_slice(&[dc1; 4]);
    }
    for row in buf.chunks_mut(stride).skip(4).take(4) {
        row[..4].copy_from_slice(&[dc2; 4]);
        row[4..8].copy_from_slice(&[dc3; 4]);
    }
}
fn ipred_8x8_left_dc(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16]) {
    let mut left_dc0 = 0;
    let mut left_dc1 = 0;
    for &el in left[1..].iter().take(4) {
        left_dc0 += el;
    }
    for &el in left[1..].iter().skip(4).take(4) {
        left_dc1 += el;
    }
    let dc0 = (left_dc0 + 2) >> 2;
    let dc2 = (left_dc1 + 2) >> 2;
    for row in buf.chunks_mut(stride).take(4) {
        row[..8].copy_from_slice(&[dc0; 8]);
    }
    for row in buf.chunks_mut(stride).skip(4).take(4) {
        row[..8].copy_from_slice(&[dc2; 8]);
    }
}
fn ipred_8x8_top_dc(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16]) {
    ipred_top_dc(buf,           stride, top,       4, 2);
    ipred_top_dc(&mut buf[4..], stride, &top[4..], 4, 2);
    let mut top = [0; 8];
    top.copy_from_slice(&buf[stride * 3..][..8]);
    ipred_top_dc(&mut buf[4 * stride..],     stride, &top,      4, 2);
    ipred_top_dc(&mut buf[4 + 4 * stride..], stride, &top[4..], 4, 2);
}

fn ipred_16x16_ver(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16]) {
    ipred_ver(buf, stride, top, 16);
}
fn ipred_16x16_hor(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16]) {
    ipred_hor(buf, stride, left, 16);
}
fn ipred_16x16_dc(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16]) {
    ipred_dc(buf, stride, top, left, 16, 5);
}
fn ipred_16x16_left_dc(buf: &mut [u16], stride: usize, _top: &[u16], left: &[u16]) {
    ipred_left_dc(buf, stride, left, 16, 4);
}
fn ipred_16x16_top_dc(buf: &mut [u16], stride: usize, top: &[u16], _left: &[u16]) {
    ipred_top_dc(buf, stride, top, 16, 4);
}

pub type AddCoeffsFunc = fn(dst: &mut [u16], offset: usize, stride: usize, coeffs: &[i16]);
pub type AddCoeffs8Func = fn(dst: &mut [u16], offset: usize, stride: usize, coeffs: &[i16; 64]);
pub type IPred4x4Func = fn(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16], tr: &[u16]);
pub type IPred8x8Func = fn(buf: &mut [u16], stride: usize, top: &[u16], left: &[u16]);
pub type IPred8x8LumaFunc = fn(buf: &mut [u16], stride: usize, ctx: &IPred8Context);

pub const IPRED4_DC128: usize = 11;
pub const IPRED4_DC_TOP: usize = 10;
pub const IPRED4_DC_LEFT: usize = 9;
pub const IPRED8_DC128: usize = 6;
pub const IPRED8_DC_TOP: usize = 5;
pub const IPRED8_DC_LEFT: usize = 4;

pub struct IPredFuncs {
    pub fill_val:           u16,
    pub add_coeffs:         AddCoeffsFunc,
    pub add_coeffs8:        AddCoeffs8Func,
    pub ipred4x4:           [IPred4x4Func; 12],
    pub ipred8x8_luma:      [IPred8x8LumaFunc; 12],
    pub ipred8x8_chroma:    [IPred8x8Func; 7],
    pub ipred16x16:         [IPred8x8Func; 7],
}

static IPRED_FUNCS: [IPredFuncs; 1] = [
    IPredFuncs {
        fill_val:       0x200,
        add_coeffs:     add_coeffs_10,
        add_coeffs8:    add_coeffs8_10,
        ipred4x4: [
            ipred_4x4_ver, ipred_4x4_hor, ipred_4x4_dc,
            ipred_4x4_diag_down_left, ipred_4x4_diag_down_right,
            ipred_4x4_ver_right, ipred_4x4_hor_down, ipred_4x4_ver_left, ipred_4x4_hor_up,
            ipred_4x4_left_dc, ipred_4x4_top_dc, ipred_4x4_dc128_10
        ],
        ipred8x8_luma: [
            ipred_y_8x8_ver, ipred_y_8x8_hor, ipred_y_8x8_dc,
            ipred_y_8x8_diag_down_left, ipred_y_8x8_diag_down_right,
            ipred_y_8x8_ver_right, ipred_y_8x8_hor_down,
            ipred_y_8x8_ver_left, ipred_y_8x8_hor_up,
            ipred_y_8x8_left_dc, ipred_y_8x8_top_dc, ipred_y_8x8_dc128_10
        ],
        ipred8x8_chroma: [
            ipred_8x8_dc, ipred_8x8_hor, ipred_8x8_ver, ipred_8x8_plane_10,
            ipred_8x8_left_dc, ipred_8x8_top_dc, ipred_8x8_dc128_10
        ],
        ipred16x16: [
            ipred_16x16_ver, ipred_16x16_hor, ipred_16x16_dc, ipred_16x16_plane_10,
            ipred_16x16_left_dc, ipred_16x16_top_dc, ipred_16x16_dc128_10
        ]
    }
];

pub fn find_ipred_funcs(fill_val: u16) -> &'static IPredFuncs {
    for ipf in IPRED_FUNCS.iter() {
        if ipf.fill_val == fill_val {
            return ipf;
        }
    }
    unreachable!()
}

macro_rules! loop_filter {
    (lumaedge; $buf: expr, $off: expr, $step: expr, $alpha: expr, $beta: expr, $clip: ident) => {
        let p2 = $buf[$off - $step * 3] as i16;
        let p1 = $buf[$off - $step * 2] as i16;
        let p0 = $buf[$off - $step] as i16;
        let q0 = $buf[$off] as i16;
        let q1 = $buf[$off + $step] as i16;
        let q2 = $buf[$off + $step * 2] as i16;
        let a_p = (p2 - p0).abs() < $beta;
        let a_q = (q2 - q0).abs() < $beta;
        if a_p && (p0 - q0).abs() < (($alpha >> 2) + 2) {
            let p3 = $buf[$off - $step * 4] as i16;
            $buf[$off - $step * 3] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u16;
            $buf[$off - $step * 2] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u16;
            $buf[$off - $step] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u16;
        } else {
            $buf[$off - $step] = ((2 * p1 + p0 + q1 + 2) >> 2) as u16;
        }
        if a_q && (p0 - q0).abs() < (($alpha >> 2) + 2) {
            let q3 = $buf[$off + $step * 3] as i16;
            $buf[$off]             = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u16;
            $buf[$off + $step]     = ((p0 + q0 + q1 + q2 + 2) >> 2) as u16;
            $buf[$off + $step * 2] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u16;
        } else {
            $buf[$off] = ((2 * q1 + q0 + p1 + 2) >> 2) as u16;
        }
    };
    (chromaedge; $buf: expr, $off: expr, $step: expr, $clip:ident) => {
        let p1 = $buf[$off - $step * 2] as i16;
        let p0 = $buf[$off - $step] as i16;
        let q0 = $buf[$off] as i16;
        let q1 = $buf[$off + $step] as i16;
        $buf[$off - $step] = ((2 * p1 + p0 + q1 + 2) >> 2) as u16;
        $buf[$off]         = ((2 * q1 + q0 + p1 + 2) >> 2) as u16;
    };
    (lumanormal; $buf: expr, $off: expr, $step: expr, $tc0: expr, $beta: expr, $clip: ident) => {
        let p2 = $buf[$off - $step * 3] as i16;
        let p1 = $buf[$off - $step * 2] as i16;
        let p0 = $buf[$off - $step] as i16;
        let q0 = $buf[$off] as i16;
        let q1 = $buf[$off + $step] as i16;
        let q2 = $buf[$off + $step * 2] as i16;
        let a_p = (p2 - p0).abs() < $beta;
        let a_q = (q2 - q0).abs() < $beta;
        let tc = $tc0 + (a_p as i16) + (a_q as i16);
        let delta = (((q0 - p0) * 4 + (p1 - q1) + 4) >> 3).max(-tc).min(tc);
        if a_p && ($tc0 > 0) {
            $buf[$off - $step * 2] = $clip(p1 + ((p2 + ((p0 + q0 + 1) >> 1) - p1 * 2) >> 1).max(-$tc0).min($tc0));
        }
        $buf[$off - $step] = $clip(p0 + delta);
        $buf[$off]         = $clip(q0 - delta);
        if a_q && ($tc0 > 0) {
            $buf[$off + $step] = $clip(q1 + ((q2 + ((p0 + q0 + 1) >> 1) - q1 * 2) >> 1).max(-$tc0).min($tc0));
        }
    };
    (chromanormal; $buf: expr, $off: expr, $step: expr, $tc0: expr, $clip: ident) => {
        let p1 = $buf[$off - $step * 2] as i16;
        let p0 = $buf[$off - $step] as i16;
        let q0 = $buf[$off] as i16;
        let q1 = $buf[$off + $step] as i16;
        let tc = $tc0 + 1;
        let delta = (((q0 - p0) * 4 + (p1 - q1) + 4) >> 3).max(-tc).min(tc);
        $buf[$off - $step] = $clip(p0 + delta);
        $buf[$off]         = $clip(q0 - delta);
    }
}

fn check_filter(buf: &[u16], off: usize, step: usize, alpha: i16, beta: i16) -> bool {
    let p1 = buf[off - step * 2] as i16;
    let p0 = buf[off - step] as i16;
    let q0 = buf[off] as i16;
    let q1 = buf[off + step] as i16;
    (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta
}

fn check_filter4(buf: &[u16], mut off: usize, step: usize, stride: usize, alpha: i16, beta: i16) -> [bool; 4] {
    let mut flags = [false; 4];
    for flag in flags.iter_mut() {
        let p1 = buf[off - step * 2] as i16;
        let p0 = buf[off - step] as i16;
        let q0 = buf[off] as i16;
        let q1 = buf[off + step] as i16;
        *flag = (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta;
        off += stride;
    }
    flags
}

fn loop_filter_lumaedge_v_10(dst: &mut [u16], mut off: usize, stride: usize, alpha: i16, beta: i16) {
    let flags = check_filter4(dst, off, 1, stride, alpha, beta);
    for &flag in flags.iter() {
        if flag {
            loop_filter!(lumaedge; dst, off, 1, alpha, beta, clip10);
        }
        off += stride;
    }
}
fn loop_filter_lumaedge_h_10(dst: &mut [u16], off: usize, stride: usize, alpha: i16, beta: i16) {
    let flags = check_filter4(dst, off, stride, 1, alpha, beta);
    for (x, &flag) in flags.iter().enumerate() {
        if flag {
            loop_filter!(lumaedge; dst, off + x, stride, alpha, beta, clip10);
        }
    }
}
fn loop_filter_lumanormal_v_10(dst: &mut [u16], mut off: usize, stride: usize, alpha: i16, beta: i16, tc0: i16) {
    let flags = check_filter4(dst, off, 1, stride, alpha, beta);
    for &flag in flags.iter() {
        if flag {
            loop_filter!(lumanormal; dst, off, 1, tc0, beta, clip10);
        }
        off += stride;
    }
}
fn loop_filter_lumanormal_h_10(dst: &mut [u16], off: usize, stride: usize, alpha: i16, beta: i16, tc0: i16) {
    let flags = check_filter4(dst, off, stride, 1, alpha, beta);
    for (x, &flag) in flags.iter().enumerate() {
        if flag {
            loop_filter!(lumanormal; dst, off + x, stride, tc0, beta, clip10);
        }
    }
}
fn loop_filter_chromaedge_v_10(dst: &mut [u16], mut off: usize, stride: usize, alpha: i16, beta: i16) {
    for _ in 0..2 {
        if check_filter(dst, off, 1, alpha, beta) {
            loop_filter!(chromaedge; dst, off, 1, clip10);
        }
        off += stride;
    }
}
fn loop_filter_chromaedge_h_10(dst: &mut [u16], off: usize, stride: usize, alpha: i16, beta: i16) {
    for x in 0..2 {
        if check_filter(dst, off + x, stride, alpha, beta) {
            loop_filter!(chromaedge; dst, off + x, stride, clip10);
        }
    }
}
fn loop_filter_chromanormal_v_10(dst: &mut [u16], mut off: usize, stride: usize, alpha: i16, beta: i16, tc0: i16) {
    for _ in 0..2 {
        if check_filter(dst, off, 1, alpha, beta) {
            loop_filter!(chromanormal; dst, off, 1, tc0, clip10);
        }
        off += stride;
    }
}
fn loop_filter_chromanormal_h_10(dst: &mut [u16], off: usize, stride: usize, alpha: i16, beta: i16, tc0: i16) {
    for x in 0..2 {
        if check_filter(dst, off + x, stride, alpha, beta) {
            loop_filter!(chromanormal; dst, off + x, stride, tc0, clip10);
        }
    }
}

const ALPHA: [i16; 52] = [
      0,   0,   0,   0,  0,  0,  0,  0,  0,  0,   0,   0,   0,   0,   0,   0,
      4,   4,   5,   6,  7,  8,  9, 10, 12, 13,  15,  17,  20,  22,  25,  28,
     32,  36,  40,  45, 50, 56, 63, 71, 80, 90, 100, 113, 127, 144, 162, 182,
    203, 226, 255, 255
];
const BETA: [i16; 52] = [
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     2,  2,  2,  3,  3,  3,  3,  4,  4,  4,  6,  6,  7,  7,  8,  8,
     9,  9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16,
    17, 17, 18, 18
];

const TC0: [[u8; 3]; 52] = [
    [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0],
    [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0],
    [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0],
    [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0], [ 0,  0,  0],
    [ 0,  0,  0], [ 0,  0,  1], [ 0,  0,  1], [ 0,  0,  1],
    [ 0,  0,  1], [ 0,  1,  1], [ 0,  1,  1], [ 1,  1,  1],
    [ 1,  1,  1], [ 1,  1,  1], [ 1,  1,  1], [ 1,  1,  2],
    [ 1,  1,  2], [ 1,  1,  2], [ 1,  1,  2], [ 1,  2,  3],
    [ 1,  2,  3], [ 2,  2,  3], [ 2,  2,  4], [ 2,  3,  4],
    [ 2,  3,  4], [ 3,  3,  5], [ 3,  4,  6], [ 3,  4,  6],
    [ 4,  5,  7], [ 4,  5,  8], [ 4,  6,  9], [ 5,  7, 10],
    [ 6,  8, 11], [ 6,  8, 13], [ 7, 10, 14], [ 8, 11, 16],
    [ 9, 12, 18], [10, 13, 20], [11, 15, 23], [13, 17, 25]
];

fn get_lf_idx(qp0: u8, qp1: u8, off: i8) -> usize {
    (i16::from((qp0 + qp1 + 1) >> 1) + i16::from(off)).clamp(0, 51) as usize
}

macro_rules! filter_edge_func {
    ($funcname: ident, $edgefilter: ident, $normfilter: ident, $shift: expr) => {
        fn $funcname(dst: &mut [u16], off: usize, stride: usize, dmode: u8, quants: [u8; 2], alpha_off: i8, beta_off: i8) {
            let q = quants[0];
            let qleft = quants[1];
            if dmode != 0 {
                let index_a = get_lf_idx(q, qleft, alpha_off);
                let alpha = ALPHA[index_a] << $shift;
                let beta = BETA[get_lf_idx(q, qleft, beta_off)] << $shift;
                if dmode == 4 {
                    $edgefilter(dst, off, stride, alpha, beta);
                } else {
                    let tc0 = i16::from(TC0[index_a][(dmode - 1) as usize]) << $shift;
                    $normfilter(dst, off, stride, alpha, beta, tc0);
                }
            }
        }
    }
}

filter_edge_func!(filter_edge_y_v_10, loop_filter_lumaedge_v_10, loop_filter_lumanormal_v_10, 10 - 8);
filter_edge_func!(filter_edge_y_h_10, loop_filter_lumaedge_h_10, loop_filter_lumanormal_h_10, 10 - 8);
filter_edge_func!(filter_edge_c_v_10, loop_filter_chromaedge_v_10, loop_filter_chromanormal_v_10, 10 - 8);
filter_edge_func!(filter_edge_c_h_10, loop_filter_chromaedge_h_10, loop_filter_chromanormal_h_10, 10 - 8);

pub type LoopFilterFn = fn (dst: &mut [u16], off: usize, stride: usize, dmode: u8, quants: [u8; 2], alpha_off: i8, beta_off: i8);

pub struct LoopFilterFuncs {
    pub fill_val:        u16,
    pub filter_edge_y_v: LoopFilterFn,
    pub filter_edge_y_h: LoopFilterFn,
    pub filter_edge_c_v: LoopFilterFn,
    pub filter_edge_c_h: LoopFilterFn,
}

static LOOP_FILTER_FUNCS: [LoopFilterFuncs; 1] = [
    LoopFilterFuncs {
        fill_val: 0x200,
        filter_edge_y_v: filter_edge_y_v_10,
        filter_edge_y_h: filter_edge_y_h_10,
        filter_edge_c_v: filter_edge_c_v_10,
        filter_edge_c_h: filter_edge_c_h_10,
    }
];

pub fn find_loop_filter_funcs(fill_val: u16) -> &'static LoopFilterFuncs {
    for lf in LOOP_FILTER_FUNCS.iter() {
        if lf.fill_val == fill_val {
            return lf;
        }
    }
    unreachable!()
}
