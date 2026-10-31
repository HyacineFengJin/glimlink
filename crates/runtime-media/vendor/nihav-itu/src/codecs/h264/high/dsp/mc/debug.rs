const TMP_BUF_STRIDE: usize = 32;

fn interp_block1(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, hor: bool, avg0: bool, clip: fn (i32) -> u16) {

    let step = if hor { 1 } else { sstride };
    let mut idx = 0;
    let avgidx = if avg0 { step * 2 } else { step * 3 };

    for dline in dst.chunks_mut(dstride).take(h) {
        for (x, pix) in dline.iter_mut().take(w).enumerate() {
            let t = (clip)((        i32::from(src[idx + x])
                             - 5  * i32::from(src[idx + x + step])
                             + 20 * i32::from(src[idx + x + step * 2])
                             + 20 * i32::from(src[idx + x + step * 3])
                             - 5  * i32::from(src[idx + x + step * 4])
                             +      i32::from(src[idx + x + step * 5])
                             + 16) >> 5);
            *pix = (t + src[idx + x + avgidx] + 1) >> 1;
        }
        idx += sstride;
    }
}

fn interp_block2(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, hor: bool, clip: fn (i32) -> u16) {
    let step = if hor { 1 } else { sstride };
    let mut idx = 0;
    for dline in dst.chunks_mut(dstride).take(h) {
        for (x, pix) in dline.iter_mut().take(w).enumerate() {
            *pix = (clip)((        i32::from(src[idx + x])
                            - 5  * i32::from(src[idx + x + step])
                            + 20 * i32::from(src[idx + x + step * 2])
                            + 20 * i32::from(src[idx + x + step * 3])
                            - 5  * i32::from(src[idx + x + step * 4])
                            +      i32::from(src[idx + x + step * 5])
                            + 16) >> 5);
        }
        idx += sstride;
    }
}

fn mc_avg_tmp(dst: &mut [u16], dstride: usize, w: usize, h: usize, tmp: &[u16], tmp2: &[u16]) {
    for (dline, (sline0, sline1)) in dst.chunks_mut(dstride).zip(tmp.chunks(TMP_BUF_STRIDE).zip(tmp2.chunks(TMP_BUF_STRIDE))).take(h) {
        for (pix, (&a, &b)) in dline.iter_mut().zip(sline0.iter().zip(sline1.iter())).take(w) {
            *pix = (a + b + 1) >> 1;
        }
    }
}

fn h264_mc00(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, _clip: fn (i32) -> u16) {
    for (dline, sline) in dst.chunks_mut(dstride).zip(src.chunks(sstride)).take(h) {
        dline[..w].copy_from_slice(&sline[..w]);
    }
}

fn h264_mc01(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block1(dst, dstride, &src[sstride * 2..], sstride, w, h, true, true, clip);
}

fn h264_mc02(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block2(dst, dstride, &src[sstride * 2..], sstride, w, h, true, clip);
}

fn h264_mc03(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block1(dst, dstride, &src[sstride * 2..], sstride, w, h, true, false, clip);
}

fn h264_mc10(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block1(dst, dstride, &src[2..], sstride, w, h, false, true, clip);
}

fn h264_mc11(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc12(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc22(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc13(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc20(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block2(dst, dstride, &src[2..], sstride, w, h, false, clip);
}

fn h264_mc21(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc22(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp = [0i32; TMP_BUF_STRIDE * 16];
    let mut idx = 0;
    for dline in tmp.chunks_mut(TMP_BUF_STRIDE).take(h) {
        for (x, pix) in dline.iter_mut().take(w + 5).enumerate() {
            *pix =        i32::from(src[idx + x])
                   - 5  * i32::from(src[idx + x + sstride])
                   + 20 * i32::from(src[idx + x + sstride * 2])
                   + 20 * i32::from(src[idx + x + sstride * 3])
                   - 5  * i32::from(src[idx + x + sstride * 4])
                   +      i32::from(src[idx + x + sstride * 5]);
        }
        idx += sstride;
    }
    for (dline, sline) in dst.chunks_mut(dstride).zip(tmp.chunks(TMP_BUF_STRIDE)).take(h) {
        for (x, pix) in dline.iter_mut().take(w).enumerate() {
            *pix = (clip)((sline[x] - 5 * sline[x + 1] + 20 * sline[x + 2] + 20 * sline[x + 3] - 5 * sline[x + 4] + sline[x + 5] + 512) >> 10);
        }
    }
}

fn h264_mc23(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc30(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block1(dst, dstride, &src[2..], sstride, w, h, false, false, clip);
}

fn h264_mc31(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc20(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc32(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc33(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp  = [0u16; TMP_BUF_STRIDE * 16];
    let mut tmp2 = [0u16; TMP_BUF_STRIDE * 16];
    h264_mc20(&mut tmp,  TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}


fn chroma_interp(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, dx: u16, dy: u16, w: usize, h: usize) {
    let a0 = 8 - dx;
    let a1 = dx;
    let b0 = 8 - dy;
    let b1 = dy;

    let src1 = &src[sstride..];
    if a0 == 8 && b0 == 8 {
        for (drow, line) in dst.chunks_mut(dstride).zip(src.chunks(sstride)).take(h) {
            drow[..w].copy_from_slice(&line[..w]);
        }
    } else if a0 == 8 {
        for (drow, (line0, line1)) in dst.chunks_mut(dstride).zip(src.chunks(sstride).zip(src1.chunks(sstride))).take(h) {
            for (pix, (&a, &b)) in drow.iter_mut().take(w).zip(line0.iter().zip(line1.iter())) {
                *pix = (a * b0 + b * b1 + 4) >> 3;
            }
        }
    } else if b0 == 8 {
        for (drow, line) in dst.chunks_mut(dstride).zip(src.chunks(sstride)).take(h) {
            let mut a = line[0];
            for (pix, &b) in drow.iter_mut().take(w).zip(line.iter().skip(1)) {
                *pix = (a * a0 + b * a1 + 4) >> 3;
                a = b;
            }
        }
    } else {
        for (drow, (line0, line1)) in dst.chunks_mut(dstride).zip(src.chunks(sstride).zip(src1.chunks(sstride))).take(h) {
            let mut a = line0[0];
            let mut c = line1[0];
            for (pix, (&b, &d)) in drow.iter_mut().take(w).zip(line0[1..].iter().zip(line1[1..].iter())) {
                *pix = ((u32::from(a) * u32::from(a0 * b0) + u32::from(b) * u32::from(a1 * b0) + u32::from(c) * u32::from(a0 * b1) + u32::from(d) * u32::from(a1 * b1) + 0x20) >> 6) as u16;
                a = b;
                c = d;
            }
        }
    }
}

pub fn chroma_interp_8(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, dx: u16, dy: u16, h: usize) {
    chroma_interp(dst, dstride, src, sstride, dx, dy, 8, h);
}

pub fn chroma_interp_4(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, dx: u16, dy: u16, h: usize) {
    chroma_interp(dst, dstride, src, sstride, dx, dy, 4, h);
}

pub fn chroma_interp_2(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, dx: u16, dy: u16, h: usize) {
    chroma_interp(dst, dstride, src, sstride, dx, dy, 2, h);
}

macro_rules! luma_mc {
    ($orig:ident, $func4:ident, $func8:ident, $func16:ident, $clip:expr) => {
        fn $func4(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, h: usize) {
            $orig(dst, dstride, src, sstride, 4, h, $clip);
        }
        fn $func8(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, h: usize) {
            $orig(dst, dstride, src, sstride, 8, h, $clip);
        }
        fn $func16(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, h: usize) {
            $orig(dst, dstride, src, sstride, 16, h, $clip);
        }
    }
}

luma_mc!(h264_mc00, h264_mc00_4_10, h264_mc00_8_10, h264_mc00_16_10, super::clip_10);
luma_mc!(h264_mc01, h264_mc01_4_10, h264_mc01_8_10, h264_mc01_16_10, super::clip_10);
luma_mc!(h264_mc02, h264_mc02_4_10, h264_mc02_8_10, h264_mc02_16_10, super::clip_10);
luma_mc!(h264_mc03, h264_mc03_4_10, h264_mc03_8_10, h264_mc03_16_10, super::clip_10);
luma_mc!(h264_mc10, h264_mc10_4_10, h264_mc10_8_10, h264_mc10_16_10, super::clip_10);
luma_mc!(h264_mc11, h264_mc11_4_10, h264_mc11_8_10, h264_mc11_16_10, super::clip_10);
luma_mc!(h264_mc12, h264_mc12_4_10, h264_mc12_8_10, h264_mc12_16_10, super::clip_10);
luma_mc!(h264_mc13, h264_mc13_4_10, h264_mc13_8_10, h264_mc13_16_10, super::clip_10);
luma_mc!(h264_mc20, h264_mc20_4_10, h264_mc20_8_10, h264_mc20_16_10, super::clip_10);
luma_mc!(h264_mc21, h264_mc21_4_10, h264_mc21_8_10, h264_mc21_16_10, super::clip_10);
luma_mc!(h264_mc22, h264_mc22_4_10, h264_mc22_8_10, h264_mc22_16_10, super::clip_10);
luma_mc!(h264_mc23, h264_mc23_4_10, h264_mc23_8_10, h264_mc23_16_10, super::clip_10);
luma_mc!(h264_mc30, h264_mc30_4_10, h264_mc30_8_10, h264_mc30_16_10, super::clip_10);
luma_mc!(h264_mc31, h264_mc31_4_10, h264_mc31_8_10, h264_mc31_16_10, super::clip_10);
luma_mc!(h264_mc32, h264_mc32_4_10, h264_mc32_8_10, h264_mc32_16_10, super::clip_10);
luma_mc!(h264_mc33, h264_mc33_4_10, h264_mc33_8_10, h264_mc33_16_10, super::clip_10);

pub const H264_LUMA_INTERP_10: [[super::MCFunc; 16]; 3] = [
  [
    h264_mc00_4_10, h264_mc01_4_10, h264_mc02_4_10, h264_mc03_4_10,
    h264_mc10_4_10, h264_mc11_4_10, h264_mc12_4_10, h264_mc13_4_10,
    h264_mc20_4_10, h264_mc21_4_10, h264_mc22_4_10, h264_mc23_4_10,
    h264_mc30_4_10, h264_mc31_4_10, h264_mc32_4_10, h264_mc33_4_10
  ], [
    h264_mc00_8_10, h264_mc01_8_10, h264_mc02_8_10, h264_mc03_8_10,
    h264_mc10_8_10, h264_mc11_8_10, h264_mc12_8_10, h264_mc13_8_10,
    h264_mc20_8_10, h264_mc21_8_10, h264_mc22_8_10, h264_mc23_8_10,
    h264_mc30_8_10, h264_mc31_8_10, h264_mc32_8_10, h264_mc33_8_10
  ], [
    h264_mc00_16_10, h264_mc01_16_10, h264_mc02_16_10, h264_mc03_16_10,
    h264_mc10_16_10, h264_mc11_16_10, h264_mc12_16_10, h264_mc13_16_10,
    h264_mc20_16_10, h264_mc21_16_10, h264_mc22_16_10, h264_mc23_16_10,
    h264_mc30_16_10, h264_mc31_16_10, h264_mc32_16_10, h264_mc33_16_10
  ]
];

impl super::RegisterSIMD for super::H264MC {
    fn register_simd(&mut self) {}
}
