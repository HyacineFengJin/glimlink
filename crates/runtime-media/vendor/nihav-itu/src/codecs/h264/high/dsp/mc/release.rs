const TMP_BUF_STRIDE: usize = 32;

fn interp_block1(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, hor: bool, avg0: bool, clip: fn (i32) -> u16) {
    unsafe {
        let step = if hor { 1 } else { sstride };
        let avgidx = if avg0 { step * 2 } else { step * 3 };
        let mut src = src.as_ptr();
        let mut dst = dst.as_mut_ptr();
        for _ in 0..h {
            for _ in 0..w {
                let t = (clip)((        i32::from(*src)
                                 - 5  * i32::from(*src.add(step))
                                 + 20 * i32::from(*src.add(step * 2))
                                 + 20 * i32::from(*src.add(step * 3))
                                 - 5  * i32::from(*src.add(step * 4))
                                 +      i32::from(*src.add(step * 5))
                                 + 16) >> 5);
                *dst = (t + *src.add(avgidx) + 1) >> 1;
                src = src.add(1);
                dst = dst.add(1);
            }
            dst = dst.sub(w).add(dstride);
            src = src.sub(w).add(sstride);
        }
    }
}

fn interp_block2(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, hor: bool, clip: fn (i32) -> u16) {
    unsafe {
        let step = if hor { 1 } else { sstride };
        let mut pix = dst.as_mut_ptr();
        let mut src = src.as_ptr();
        for _ in 0..h {
            for x in 0..w {
                *pix.add(x) = (clip)((        i32::from(*src)
                                       - 5  * i32::from(*src.add(step))
                                       + 20 * i32::from(*src.add(step * 2))
                                       + 20 * i32::from(*src.add(step * 3))
                                       - 5  * i32::from(*src.add(step * 4))
                                       +      i32::from(*src.add(step * 5))
                                       + 16) >> 5);
                src = src.add(1);
            }
            pix = pix.add(dstride);
            src = src.sub(w);
            src = src.add(sstride);
        }
    }
}

fn mc_avg_tmp(dst: &mut [u16], dstride: usize, w: usize, h: usize, tmp: &[u16], tmp2: &[u16]) {
    unsafe {
        let mut src1 = tmp.as_ptr();
        let mut src2 = tmp2.as_ptr();
        let mut dst = dst.as_mut_ptr();
        for _ in 0..h {
            for x in 0..w {
                let a = *src1.add(x);
                let b = *src2.add(x);
                *dst.add(x) = (a + b + 1) >> 1;
            }
            dst = dst.add(dstride);
            src1 = src1.add(TMP_BUF_STRIDE);
            src2 = src2.add(TMP_BUF_STRIDE);
        }
    }
}

fn h264_mc00(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, _clip: fn(i32) -> u16) {
    unsafe {
        let mut src = src.as_ptr();
        let mut dst = dst.as_mut_ptr();
        for _ in 0..h {
            std::ptr::copy_nonoverlapping(src, dst, w);
            src = src.add(sstride);
            dst = dst.add(dstride);
        }
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
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc12(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc22(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc13(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc02(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc20(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block2(dst, dstride, &src[2..], sstride, w, h, false, clip);
}

fn h264_mc21(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, src, sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc22(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp: [i32; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    unsafe {
        let mut src = src.as_ptr();
        let mut dst = tmp.as_mut_ptr();
        for _ in 0..h {
            for _ in 0..w+5 {
                *dst =        i32::from(*src)
                       - 5  * i32::from(*src.add(sstride))
                       + 20 * i32::from(*src.add(sstride * 2))
                       + 20 * i32::from(*src.add(sstride * 3))
                       - 5  * i32::from(*src.add(sstride * 4))
                       +      i32::from(*src.add(sstride * 5));
                dst = dst.add(1);
                src = src.add(1);
            }
            src = src.sub(w+5).add(sstride);
            dst = dst.sub(w+5).add(TMP_BUF_STRIDE);
        }
    }
    unsafe {
        let mut dst = dst.as_mut_ptr();
        let mut src = tmp.as_ptr();
        for _ in 0..h {
            for _ in 0..w {
                *dst = (clip)((*src - 5 * *src.add(1) + 20 * *src.add(2) + 20 * *src.add(3) - 5 * *src.add(4) + *src.add(5) + 512) >> 10);
                dst = dst.add(1);
                src = src.add(1);
            }
            dst = dst.sub(w).add(dstride);
            src = src.sub(w).add(TMP_BUF_STRIDE);
        }
    }
}

fn h264_mc23(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc20(&mut tmp2, TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc30(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    interp_block1(dst, dstride, &src[2..], sstride, w, h, false, false, clip);
}

fn h264_mc31(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc20(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc32(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc22(&mut tmp,  TMP_BUF_STRIDE, src, sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}

fn h264_mc33(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, w: usize, h: usize, clip: fn (i32) -> u16) {
    let mut tmp : [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    let mut tmp2: [u16; TMP_BUF_STRIDE * 16] = unsafe { let arr = std::mem::MaybeUninit::uninit(); arr.assume_init() };
    h264_mc20(&mut tmp,  TMP_BUF_STRIDE, &src[1..], sstride, w, h, clip);
    h264_mc02(&mut tmp2, TMP_BUF_STRIDE, &src[sstride..], sstride, w, h, clip);
    mc_avg_tmp(dst, dstride, w, h, &tmp, &tmp2);
}


fn chroma_interp(dst: &mut [u16], dstride: usize, src: &[u16], sstride: usize, dx: u16, dy: u16, w: usize, h: usize) {
    let a0 = 8 - dx;
    let a1 = dx;
    let b0 = 8 - dy;
    let b1 = dy;

    if a0 == 8 && b0 == 8 {
        unsafe {
            let mut src = src.as_ptr();
            let mut dst = dst.as_mut_ptr();
            for _ in 0..h {
                std::ptr::copy_nonoverlapping(src, dst, w);
                src = src.add(sstride);
                dst = dst.add(dstride);
            }
        }
    } else if a0 == 8 {
        unsafe {
            let mut src0 = src.as_ptr();
            let mut src1 = src0.add(sstride);
            let mut dst = dst.as_mut_ptr();
            for _ in 0..h {
                for x in 0..w {
                    let a = *src0.add(x);
                    let b = *src1.add(x);
                    *dst.add(x) = (a * b0 + b * b1 + 4) >> 3;
                }
                src0 = src0.add(sstride);
                src1 = src1.add(sstride);
                dst = dst.add(dstride);
            }
        }
    } else if b0 == 8 {
        unsafe {
            let mut src = src.as_ptr();
            let mut dst = dst.as_mut_ptr();
            for _ in 0..h {
                let mut a = *src;
                for x in 0..w {
                    let b = *src.add(x + 1);
                    *dst.add(x) = (a * a0 + b * a1 + 4) >> 3;
                    a = b;
                }
                src = src.add(sstride);
                dst = dst.add(dstride);
            }
        }
    } else {
        unsafe {
            let mut src0 = src.as_ptr();
            let mut src1 = src0.add(sstride);
            let mut dst = dst.as_mut_ptr();
            for _ in 0..h {
                let mut a = *src0;
                let mut c = *src1;
                for x in 0..w {
                    let b = *src0.add(x + 1);
                    let d = *src1.add(x + 1);
                    *dst.add(x) = ((u32::from(a) * u32::from(a0 * b0) + u32::from(b) * u32::from(a1 * b0) + u32::from(c) * u32::from(a0 * b1) + u32::from(d) * u32::from(a1 * b1) + 0x20) >> 6) as u16;
                    a = b;
                    c = d;
                }
                src0 = src0.add(sstride);
                src1 = src1.add(sstride);
                dst = dst.add(dstride);
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
