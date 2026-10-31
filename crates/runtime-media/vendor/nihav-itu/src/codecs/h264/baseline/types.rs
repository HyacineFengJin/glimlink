use nihav_core::frame::{NAVideoBuffer, NASimpleVideoFrame};
use nihav_codec_support::codecs::{MV, ZERO_MV};
use super::SimplifiedSliceRefs;
use super::pic_ref::FrameMBInfo;
use super::super::common_types::*;

#[derive(Clone,Copy)]
pub struct SimpleFrame<'a> {
    pub data:   &'a [u8],
    pub offset: [usize; 3],
    pub stride: [usize; 3],
}

impl<'a> SimpleFrame<'a> {
    pub fn new(buf: &'a NAVideoBuffer<u8>) -> Self {
        let mut offset = [0; 3];
        let mut stride = [0; 3];
        for (plane, (offs, strd)) in offset.iter_mut().zip(stride.iter_mut()).enumerate() {
            *offs = buf.get_offset(plane);
            *strd = buf.get_stride(plane);
        }
        Self {
            data:   buf.get_data(),
            offset, stride
        }
    }
}

pub type SliceState = SliceStateCommon<u8>;

impl SliceState {
    pub fn new() -> Self {
        let mut obj = SliceState::new_default();
        obj.def_fill = 0x80;
        obj
    }
    pub fn save_ipred_context(&mut self, frm: &NASimpleVideoFrame<u8>) {
        let dstoff = self.mb_x * 16;
        let srcoff = frm.offset[0] + self.mb_x * 16 + self.mb_y * 16 * frm.stride[0];
        self.left_y[0] = self.top_line_y[dstoff + 15];
        self.top_line_y[dstoff..][..16].copy_from_slice(&frm.data[srcoff + frm.stride[0] * 15..][..16]);
        for (dst, src) in self.left_y[1..].iter_mut().zip(frm.data[srcoff..].chunks(frm.stride[0])) {
            *dst = src[15];
        }
        for chroma in 0..2 {
            let cstride = frm.stride[chroma + 1];
            let dstoff = self.mb_x * 8;
            let srcoff = frm.offset[chroma + 1] + self.mb_x * 8 + self.mb_y * 8 * cstride;
            self.left_c[chroma][0] = self.top_line_c[chroma][dstoff + 7];
            self.top_line_c[chroma][dstoff..][..8].copy_from_slice(&frm.data[srcoff + cstride * 7..][..8]);
            for (dst, src) in self.left_c[chroma][1..].iter_mut().zip(frm.data[srcoff..].chunks(cstride)) {
                *dst = src[7];
            }
        }
    }
    pub fn fill_deblock(&mut self, frefs: &SimplifiedSliceRefs, deblock_mode: u8, is_s: bool) {
        if deblock_mode == 1 {
            return;
        }

        self.deblock = [0; 16];

        let tx8x8 = self.get_cur_mb().transform_8x8;

        let cur_intra     = self.get_cur_mb().mb_type.is_intra();
        let left_intra    = self.get_left_mb().mb_type.is_intra();
        let mut top_intra = self.get_top_mb().mb_type.is_intra();

        let mut coded_cache  = [false; 25];
        let mut mvc          = MVCache::default();
        let mv_cache         = &mut mvc.data;
        let mut ref_cache    = [[INVALID_REF; 2]; 25];

        if self.mb_y != 0 || self.has_top {
            for (x, (cc, mv)) in coded_cache[1..5].iter_mut().zip(mv_cache[1..5].iter_mut()).enumerate() {
                let blk4 = self.get_top_blk4(x);
                *cc = blk4.ncoded != 0;
                *mv = blk4.mv;
                if (x & 1) == 0 {
                    let blk8 = self.get_top_blk8(x / 2);
                    ref_cache[x + 1] = blk8.ref_idx;
                } else {
                    ref_cache[x + 1] = ref_cache[x];
                }
            }
        }
        for (y, (ccs, mvs)) in coded_cache[5..].chunks_exact_mut(5).zip(
                mv_cache[5..].chunks_exact_mut(5)).enumerate() {
            if self.has_left || self.mb_x != 0 {
                let blk4 = self.get_left_blk4(y * 4);
                ccs[0] = blk4.ncoded != 0;
                mvs[0] = blk4.mv;
                if (y & 1) == 0 {
                    let blk8 = self.get_left_blk8(y);
                    ref_cache[y * 5 + 5] = blk8.ref_idx;
                } else {
                    ref_cache[y * 5 + 5] = ref_cache[y * 5];
                }
            }
            for (x, (cc, mv)) in ccs[1..].iter_mut().zip(mvs[1..].iter_mut()).enumerate() {
                let blk4 = self.get_cur_blk4(x + y * 4);
                *cc = blk4.ncoded != 0;
                *mv = blk4.mv;
                ref_cache[x + 1 + (y + 1) * 5] = if ((x & 1) == 0) && ((y & 1) == 0) {
                        self.get_cur_blk8(x / 2 + y).ref_idx
                    } else {
                        ref_cache[(x & !1) + 1 + ((y & !1) + 1) * 5]
                    };
            }
        }

        for (y, (((top_ccs, cur_ccs), (top_mvs, cur_mvs)), (cur_refs, top_refs))) in
                coded_cache.chunks_exact(5).take(4).zip(coded_cache[5..].chunks_exact(5)).zip(
                    mv_cache.chunks_exact(5).zip(mv_cache[5..].chunks_exact(5))).zip(
                ref_cache[5..].chunks_exact(5).zip(ref_cache.chunks_exact(5))).enumerate() {
            let can_do_top = y != 0 || (self.mb_y != 0 && (self.has_top || deblock_mode != 2));
            if can_do_top && (!tx8x8 || (y & 1) == 0) {
                if is_s || cur_intra || top_intra {
                    let val = if y == 0 { 0x40 } else { 0x30 };
                    for el in self.deblock[y * 4..][..4].iter_mut() { *el |= val; }
                } else {
                    for (x, (((&cur_cc, &top_cc), (cur_mv, top_mv)), (&cur_ref, &top_ref))) in
                            cur_ccs[1..].iter().zip(top_ccs[1..].iter()).zip(
                                cur_mvs[1..].iter().zip(top_mvs[1..].iter())).zip(
                            cur_refs[1..].iter().zip(
                                top_refs[1..].iter())).take(4).enumerate() {
                        let mut mask = 0;
                        if cur_cc || top_cc {
                            mask = 0x20;
                        } else {
                            if mvdiff4(cur_mv, top_mv) || !frefs.cmp_refs(cur_ref, top_ref) {
                                mask = 0x10;
                            }
                        }
                        if mask != 0 {
                            self.deblock[y * 4 + x] = mask;
                        }
                    }
                }
            }
            let mut lleft_intra = left_intra;
            for (x, (((&cur_cc, &left_cc), (cur_mv, left_mv)), (&cur_ref, &left_ref))) in
                    cur_ccs[1..].iter().zip(cur_ccs.iter()).zip(
                        cur_mvs[1..].iter().zip(cur_mvs.iter())).zip(
                    cur_refs[1..].iter().zip(cur_refs.iter())).enumerate() {
                let skip_8 = tx8x8 && (x & 1) != 0;
                let can_do_left = x > 0 || self.has_left || (self.mb_x != 0 && deblock_mode != 2);
                if !can_do_left {
                    continue;
                }
                let mut mask = 0;
                if skip_8 {
                } else if is_s || cur_intra || lleft_intra {
                    mask = if x == 0 { 4 } else { 3 };
                } else if cur_cc || left_cc {
                    mask = 2;
                } else {
                    if mvdiff4(cur_mv, left_mv) || !frefs.cmp_refs(cur_ref, left_ref) {
                        mask = 1;
                    }
                }
                if mask != 0 {
                    self.deblock[y * 4 + x] |= mask;
                }
                lleft_intra = cur_intra;
            }
            top_intra = cur_intra;
        }
    }
    pub fn predict_direct_mb(&mut self, frame_refs: &SimplifiedSliceRefs, temporal_mv: bool, direct_8x8: bool, cur_id: u16) {
        let (col_mb, r1_poc, r1_long) = frame_refs.get_colocated_info(self.mb_x, self.mb_y);
        if direct_8x8 {
            for blk4 in 0..16 {
                let (mv0, ref0, mv1, ref1) = self.get_direct_mv(frame_refs, &col_mb, r1_poc, r1_long, temporal_mv, cur_id, BLK4_TO_D8[blk4]);
                self.get_cur_blk4(blk4).mv = [mv0, mv1];
                self.get_cur_blk8(blk4_to_blk8(blk4)).ref_idx = [ref0, ref1];
            }
        } else if col_mb.mb_type.is_16x16_ref() || !temporal_mv {
            let (mv0, ref0, mv1, ref1) = self.get_direct_mv(frame_refs, &col_mb, r1_poc, r1_long, temporal_mv, cur_id, 0);
            self.apply_to_blk4(|blk4| blk4.mv = [mv0, mv1]);
            self.apply_to_blk8(|blk8| blk8.ref_idx = [ref0, ref1]);
        } else {
            for blk4 in 0..16 {
                let (mv0, ref0, mv1, ref1) = self.get_direct_mv(frame_refs, &col_mb, r1_poc, r1_long, temporal_mv, cur_id, blk4);
                self.get_cur_blk4(blk4).mv = [mv0, mv1];
                self.get_cur_blk8(blk4_to_blk8(blk4)).ref_idx = [ref0, ref1];
            }
        }
    }
    pub fn predict_direct_sub(&mut self, frame_refs: &SimplifiedSliceRefs, temporal_mv: bool, direct8x8: bool, cur_id: u16, blk4: usize) {
        let src_blk = if !direct8x8 { blk4 } else { BLK4_TO_D8[blk4] };
        let (mbi, r1_poc, r1_long) = frame_refs.get_colocated_info(self.mb_x, self.mb_y);
        let (mv0, ref0, mv1, ref1) = self.get_direct_mv(frame_refs, &mbi, r1_poc, r1_long, temporal_mv, cur_id, src_blk);
        self.get_cur_blk4(blk4).mv = [mv0, mv1];
        self.get_cur_blk8(blk4_to_blk8(blk4)).ref_idx = [ref0, ref1];
    }
    #[allow(clippy::nonminimal_bool)]
    pub fn get_direct_mv(&self, frame_refs: &SimplifiedSliceRefs, mbi: &FrameMBInfo, r1_poc: u16, r1_long: bool, temporal_mv: bool, cur_id: u16, blk4: usize) -> (MV, PicRef, MV, PicRef) {
        let blk8 = blk4_to_blk8(blk4);
        let (col_mv, r0_poc, col_idx) = if mbi.ref_poc[blk8] == [MISSING_POC; 2] {
                (ZERO_MV, MISSING_POC, MISSING_REF)
            } else if mbi.ref_poc[blk8][0] != MISSING_POC {
                (mbi.mv[blk4][0], mbi.ref_poc[blk8][0], mbi.ref_idx[blk8][0])
            } else {
                (mbi.mv[blk4][1], mbi.ref_poc[blk8][1], mbi.ref_idx[blk8][1])
            };
        let (col_ref, r0_long) = frame_refs.map_ref0(r0_poc);
        if temporal_mv {
            let td = (i32::from(r1_poc) - i32::from(r0_poc)).clamp(-128, 127);
            if r0_long || td == 0 {
                (col_mv, col_ref, ZERO_MV, ZERO_REF)
            } else {
                let tx = (16384 + (td / 2).abs()) / td;
                let tb = (i32::from(cur_id) - i32::from(r0_poc)).clamp(-128, 127);
                let scale = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                let mv0 = MV {
                        x: ((i32::from(col_mv.x) * scale + 128) >> 8) as i16,
                        y: ((i32::from(col_mv.y) * scale + 128) >> 8) as i16,
                    };
                let mv1 = mv0 - col_mv;
                (mv0, col_ref, mv1, ZERO_REF)
            }
        } else {
            let blk4 = 0; // we generate the same MV prediction for the whole MB
            let blk8 = blk4_to_blk8(blk4);
            let midx = self.get_cur_blk4_idx(blk4);
            let ridx = self.get_cur_blk8_idx(blk8);
            let ridx_c = self.get_cur_blk8_idx(blk8) + 16 / 8 - self.blk8.stride;

            let mv_a = self.blk4.data[midx - 1].mv;
            let mv_b = self.blk4.data[midx - self.blk4.stride].mv;
            let mut mv_c = self.blk4.data[midx - self.blk4.stride + 16 / 4].mv;

            let ref_a = self.blk8.data[ridx - 1].ref_idx;
            let ref_b = self.blk8.data[ridx - self.blk8.stride].ref_idx;
            let mut ref_c = self.blk8.data[ridx_c].ref_idx;

            if ref_c == [MISSING_REF; 2] {
                mv_c = self.blk4.data[midx - self.blk4.stride - 1].mv;
                ref_c = self.blk8.data[ridx - self.blk8.stride - 1].ref_idx;
            }
            let mut refs = [INVALID_REF; 2];
            for cur_ref in [ref_a, ref_b, ref_c].iter() {
                refs[0] = refs[0].min_pos(cur_ref[0]);
                refs[1] = refs[1].min_pos(cur_ref[1]);
            }
            if refs == [INVALID_REF; 2] {
                return (ZERO_MV, ZERO_REF, ZERO_MV, ZERO_REF);
            }

            let mut col_zero = true;
            if r1_long || col_idx != ZERO_REF {
                col_zero = false;
            }
            if col_mv.x.abs() > 1 || col_mv.y.abs() > 1 {
                col_zero = false;
            }
            let mut mvs = [ZERO_MV; 2];
            for ref_l in 0..2 {
                if mbi.mb_type.is_intra() || (!refs[ref_l].not_avail() && !(refs[ref_l] == ZERO_REF && col_zero)) {
                    let ref_idx = refs[ref_l];
                    mvs[ref_l] = if ref_b[ref_l] == MISSING_REF && ref_c[ref_l] == MISSING_REF {
                            mv_a[ref_l]
                        } else {
                            let count = ((ref_a[ref_l] == ref_idx) as u8) + ((ref_b[ref_l] == ref_idx) as u8) + ((ref_c[ref_l] == ref_idx) as u8);
                            if count == 1 {
                                if ref_a[ref_l] == ref_idx {
                                    mv_a[ref_l]
                                } else if ref_b[ref_l] == ref_idx {
                                    mv_b[ref_l]
                                } else {
                                    mv_c[ref_l]
                                }
                            } else {
                                MV::pred(mv_a[ref_l], mv_b[ref_l], mv_c[ref_l])
                            }
                        };
                }
            }
            (mvs[0], refs[0], mvs[1], refs[1])
        }
    }
}
