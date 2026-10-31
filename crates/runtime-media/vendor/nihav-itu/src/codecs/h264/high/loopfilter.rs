use nihav_core::frame::NASimpleVideoFrame;
use super::types::SliceState;
use super::dsp::*;

pub fn loop_filter_mb(frm: &mut NASimpleVideoFrame<u16>, sstate: &SliceState, alpha_off: i8, beta_off: i8) {
    let lf = find_loop_filter_funcs(sstate.def_fill);

    let yoff = frm.offset[0] + sstate.mb_x * 16 + sstate.mb_y * 16 * frm.stride[0];
    let uoff = frm.offset[1] + sstate.mb_x *  8 + sstate.mb_y *  8 * frm.stride[1];
    let voff = frm.offset[2] + sstate.mb_x *  8 + sstate.mb_y *  8 * frm.stride[2];
    let mb_idx = sstate.mb.xpos + sstate.mb_x;

    let lqy = sstate.mb.data[mb_idx - 1].qp_y;
    let lqu = sstate.mb.data[mb_idx - 1].qp_u;
    let lqv = sstate.mb.data[mb_idx - 1].qp_v;
    let qy = sstate.mb.data[mb_idx].qp_y;
    let qu = sstate.mb.data[mb_idx].qp_u;
    let qv = sstate.mb.data[mb_idx].qp_v;

    for (y, dmodes) in sstate.deblock.chunks(4).enumerate() {
        (lf.filter_edge_y_v)(frm.data, yoff + y * 4 * frm.stride[0], frm.stride[0], dmodes[0] & 0xF, [qy, lqy], alpha_off, beta_off);
        for x in 1..4 {
            (lf.filter_edge_y_v)(frm.data, yoff + x * 4 + y * 4 * frm.stride[0], frm.stride[0], dmodes[x] & 0xF, [qy, qy], alpha_off, beta_off);
        }
        (lf.filter_edge_c_v)(frm.data, uoff + y * 2 * frm.stride[1], frm.stride[1], dmodes[0] & 0xF, [qu, lqu], alpha_off, beta_off);
        (lf.filter_edge_c_v)(frm.data, uoff + y * 2 * frm.stride[1] + 4, frm.stride[1], dmodes[2] & 0xF, [qu, qu], alpha_off, beta_off);
        (lf.filter_edge_c_v)(frm.data, voff + y * 2 * frm.stride[2], frm.stride[2], dmodes[0] & 0xF, [qv, lqv], alpha_off, beta_off);
        (lf.filter_edge_c_v)(frm.data, voff + y * 2 * frm.stride[2] + 4, frm.stride[2], dmodes[2] & 0xF, [qv, qv], alpha_off, beta_off);
    }

    let tqy = sstate.mb.data[mb_idx - sstate.mb.stride].qp_y;
    let tqu = sstate.mb.data[mb_idx - sstate.mb.stride].qp_u;
    let tqv = sstate.mb.data[mb_idx - sstate.mb.stride].qp_v;

    let dmodes = &sstate.deblock;
    for x in 0..4 {
        (lf.filter_edge_y_h)(frm.data, yoff + x * 4, frm.stride[0], dmodes[x] >> 4, [qy, tqy], alpha_off, beta_off);
    }
    for x in 0..4 {
        (lf.filter_edge_c_h)(frm.data, uoff + x * 2, frm.stride[1], dmodes[x] >> 4, [qu, tqu], alpha_off, beta_off);
        (lf.filter_edge_c_h)(frm.data, voff + x * 2, frm.stride[2], dmodes[x] >> 4, [qv, tqv], alpha_off, beta_off);
    }

    for (y, dmodes) in sstate.deblock.chunks(4).enumerate().skip(1) {
        for x in 0..4 {
            (lf.filter_edge_y_h)(frm.data, yoff + x * 4 + y * 4 * frm.stride[0], frm.stride[0], dmodes[x] >> 4, [qy, qy], alpha_off, beta_off);
        }
    }

    let dmodes = &sstate.deblock[4 * 2..];
    for x in 0..4 {
        (lf.filter_edge_c_h)(frm.data, uoff + x * 2 + frm.stride[1] * 4, frm.stride[1], dmodes[x] >> 4, [qu, qu], alpha_off, beta_off);
        (lf.filter_edge_c_h)(frm.data, voff + x * 2 + frm.stride[2] * 4, frm.stride[2], dmodes[x] >> 4, [qv, qv], alpha_off, beta_off);
    }
}
