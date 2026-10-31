use nihav_codec_support::codecs::{MV, ZERO_MV};
use nihav_codec_support::data::GenericCache;

#[repr(u8)]
#[derive(Clone,Copy,Debug,PartialEq)]
pub enum BMode {
    L0,
    L1,
    Bi,
}

#[derive(Clone,Copy,Debug,PartialEq,Default)]
pub enum MBType {
    #[default]
    Intra4x4,
    Intra8x8,
    Intra16x16(u8, u8, u8),
    PCM,

    P16x16,
    P16x8,
    P8x16,
    P8x8,
    P8x8Ref0,
    PSkip,

    Direct,
    B16x16(BMode),
    B16x8(BMode, BMode),
    B8x16(BMode, BMode),
    B8x8,
    BSkip,
}

impl MBType {
    pub fn is_intra(self) -> bool {
        matches!(self, MBType::Intra4x4 | MBType::Intra8x8 | MBType::Intra16x16(_, _, _) | MBType::PCM)
    }
    pub fn is_intra16x16(self) -> bool {
        matches!(self, MBType::Intra16x16(_, _, _))
    }
    pub fn is_skip(self) -> bool {
        matches!(self, MBType::PSkip | MBType::BSkip)
    }
    pub fn is_4x4(self) -> bool { self.num_parts() == 4 }
    pub fn is_l0(self, part: usize) -> bool {
        match self {
            MBType::B16x16(mode) => mode == BMode::L0,
            MBType::B16x8(mode0, mode1) | MBType::B8x16(mode0, mode1) => {
                if part == 0 {
                    mode0 == BMode::L0
                } else {
                    mode1 == BMode::L0
                }
            },
            MBType::Direct | MBType::BSkip => false,
            _ => true,
        }
    }
    pub fn is_l1(self, part: usize) -> bool {
        match self {
            MBType::B16x16(mode) => mode == BMode::L1,
            MBType::B16x8(mode0, mode1) | MBType::B8x16(mode0, mode1) => {
                if part == 0 {
                    mode0 == BMode::L1
                } else {
                    mode1 == BMode::L1
                }
            },
            _ => false,
        }
    }
    pub fn num_parts(self) -> usize {
        match self {
            MBType::Intra4x4 | MBType::Intra8x8 | MBType::Intra16x16(_, _, _) | MBType::PCM |
            MBType::PSkip |
            MBType::Direct | MBType::BSkip
                => 1,
            MBType::P16x16 |
            MBType::B16x16(_)
                => 1,
            MBType::P16x8 | MBType::P8x16 |
            MBType::B16x8(_, _) | MBType::B8x16(_, _)
                => 2,
            _ => 4,
        }
    }
    pub fn size(self) -> (usize, usize) {
        match self {
            MBType::Intra4x4 |
            MBType::Intra8x8 |
            MBType::Intra16x16(_, _, _) |
            MBType::PCM |
            MBType::P16x16 |
            MBType::PSkip |
            MBType::Direct |
            MBType::B16x16(_) |
            MBType::BSkip
                => (16, 16),
            MBType::P16x8 | MBType::B16x8(_, _) => (16, 8),
            MBType::P8x16 | MBType::B8x16(_, _) => (8, 16),
            _ => (8, 8),
        }
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Default)]
pub enum SubMBType {
    P8x8,
    P8x4,
    P4x8,
    P4x4,
    #[default]
    Direct8x8,
    B8x8(BMode),
    B8x4(BMode),
    B4x8(BMode),
    B4x4(BMode),
}

impl SubMBType {
    pub fn num_parts(self) -> usize {
        match self {
            SubMBType::P8x8 | SubMBType::Direct8x8 | SubMBType::B8x8(_) => 1,
            SubMBType::P4x4 | SubMBType::B4x4(_) => 4,
            _ => 2,
        }
    }
    pub fn size(self) -> (usize, usize) {
        match self {
            SubMBType::P8x8 | SubMBType::Direct8x8 | SubMBType::B8x8(_) => (8, 8),
            SubMBType::P8x4 | SubMBType::B8x4(_) => (8, 4),
            SubMBType::P4x8 | SubMBType::B4x8(_) => (4, 8),
            SubMBType::P4x4 | SubMBType::B4x4(_) => (4, 4),
        }
    }
    pub fn is_l0(self) -> bool {
        match self {
            SubMBType::B8x8(mode) | SubMBType::B8x4(mode) |
            SubMBType::B4x8(mode) | SubMBType::B4x4(mode) => {
                mode == BMode::L0
            },
            _ => true,
        }
    }
    pub fn is_l1(self) -> bool {
        match self {
            SubMBType::B8x8(mode) | SubMBType::B8x4(mode) |
            SubMBType::B4x8(mode) | SubMBType::B4x4(mode) => {
                mode == BMode::L1
            },
            _ => false,
        }
    }
}

#[repr(u8)]
#[derive(Clone,Copy,Debug,PartialEq,Default)]
pub enum CompactMBType {
    Intra4x4,
    Intra8x8,
    Intra16x16,
    PCM,

    P16x16,
    P16x8,
    P8x16,
    P8x8,
    P8x8Ref0,
    PSkip,

    Direct,
    B16x16,
    B16x8,
    B8x16,
    B8x8,
    BSkip,

    #[default]
    None,
}

impl CompactMBType {
    pub fn is_intra(self) -> bool {
        matches!(self, CompactMBType::Intra4x4 | CompactMBType::Intra8x8 | CompactMBType::Intra16x16)
    }
    pub fn is_intra16orpcm(self) -> bool {
        matches!(self, CompactMBType::Intra16x16 | CompactMBType::PCM)
    }
    pub fn is_skip(self) -> bool {
        matches!(self, CompactMBType::PSkip | CompactMBType::BSkip)
    }
    pub fn is_direct(self) -> bool {
        matches!(self, CompactMBType::BSkip | CompactMBType::Direct | CompactMBType::None)
    }
    pub fn is_inter(self) -> bool {
        !self.is_intra() && !self.is_skip() && self != CompactMBType::PCM
    }
    pub fn is_16x16_ref(self) -> bool {
        matches!(self,
            CompactMBType::Intra4x4 |
            CompactMBType::Intra8x8 |
            CompactMBType::Intra16x16 |
            CompactMBType::PCM |
            CompactMBType::P16x16 |
            CompactMBType::B16x16)
    }
}

impl From<MBType> for CompactMBType {
    fn from(mbtype: MBType) -> Self {
        match mbtype {
            MBType::Intra4x4 => CompactMBType::Intra4x4,
            MBType::Intra8x8 => CompactMBType::Intra8x8,
            MBType::Intra16x16(_, _, _) => CompactMBType::Intra16x16,
            MBType::PCM => CompactMBType::PCM,
            MBType::P16x16 => CompactMBType::P16x16,
            MBType::P16x8 => CompactMBType::P16x8,
            MBType::P8x16 => CompactMBType::P8x16,
            MBType::P8x8 => CompactMBType::P8x8,
            MBType::P8x8Ref0 => CompactMBType::P8x8Ref0,
            MBType::PSkip => CompactMBType::PSkip,
            MBType::Direct => CompactMBType::Direct,
            MBType::B16x16(_) => CompactMBType::B16x16,
            MBType::B16x8(_, _) => CompactMBType::B16x8,
            MBType::B8x16(_, _) => CompactMBType::B8x16,
            MBType::B8x8 => CompactMBType::B8x8,
            MBType::BSkip => CompactMBType::BSkip,
        }
    }
}

#[repr(u8)]
#[derive(Clone,Copy,Debug,PartialEq,Default)]
pub enum IntraPredMode {
    Vertical,
    Horizontal,
    DC,
    DiagDownLeft,
    DiagDownRight,
    VerRight,
    HorDown,
    VerLeft,
    HorUp,
    #[default]
    None,
}

impl IntraPredMode {
    pub fn is_none(self) -> bool { self == IntraPredMode::None }
    pub fn into_pred_idx(self) -> i8 {
        if !self.is_none() {
            self as u8 as i8
        } else {
            -1
        }
    }
}

impl From<u8> for IntraPredMode {
    fn from(val: u8) -> Self {
        match val {
            0 => IntraPredMode::Vertical,
            1 => IntraPredMode::Horizontal,
            2 => IntraPredMode::DC,
            3 => IntraPredMode::DiagDownLeft,
            4 => IntraPredMode::DiagDownRight,
            5 => IntraPredMode::VerRight,
            6 => IntraPredMode::HorDown,
            7 => IntraPredMode::VerLeft,
            8 => IntraPredMode::HorUp,
            _ => IntraPredMode::None,
        }
    }
}

impl From<IntraPredMode> for u8 {
    fn from(val: IntraPredMode) -> Self {
        match val {
            IntraPredMode::Vertical      => 0,
            IntraPredMode::Horizontal    => 1,
            IntraPredMode::DC            => 2,
            IntraPredMode::DiagDownLeft  => 3,
            IntraPredMode::DiagDownRight => 4,
            IntraPredMode::VerRight      => 5,
            IntraPredMode::HorDown       => 6,
            IntraPredMode::VerLeft       => 7,
            IntraPredMode::HorUp         => 8,
            _ => 9,
        }
    }
}

pub const MISSING_POC: u16 = 0xFFFF;

#[derive(Clone,Copy,Debug)]
pub struct PicRef {
    ref_idx: u8
}

pub const MISSING_REF: PicRef = PicRef { ref_idx: 0xFF };
pub const INVALID_REF: PicRef = PicRef { ref_idx: 0xFE };
pub const ZERO_REF: PicRef = PicRef { ref_idx: 0 };
const DIRECT_FLAG: u8 = 0x40;

impl PicRef {
    pub fn new(ref_idx: u8) -> Self {
        Self { ref_idx }
    }
    pub fn not_avail(self) -> bool {
        self == MISSING_REF || self == INVALID_REF
    }
    pub fn index(self) -> usize { (self.ref_idx & !DIRECT_FLAG) as usize }
    pub fn is_direct(self) -> bool { (self.ref_idx & DIRECT_FLAG) != 0 }
    pub fn set_direct(&mut self) { self.ref_idx |= DIRECT_FLAG; }
    pub fn min_pos(self, other: Self) -> Self {
        match (self.not_avail(), other.not_avail()) {
            (true,  true)   => self,
            (false, true)   => self,
            (true,  false)  => other,
            (false, false)  => PicRef::new((self.ref_idx & !DIRECT_FLAG).min(other.ref_idx & !DIRECT_FLAG)),
        }
    }
}

impl Default for PicRef {
    fn default() -> Self { MISSING_REF }
}

impl PartialEq for PicRef {
    fn eq(&self, other: &Self) -> bool {
        (self.ref_idx | DIRECT_FLAG) == (other.ref_idx | DIRECT_FLAG)
    }
}

impl std::fmt::Display for PicRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == MISSING_REF {
            write!(f, "-1")
        } else if *self == INVALID_REF {
            write!(f, "-2")
        } else {
            write!(f, "{}", self.ref_idx & !DIRECT_FLAG)
        }
    }
}

#[derive(Clone,Copy,Default)]
pub struct MBData {
    pub mb_type:        CompactMBType,
    pub cbp:            u8,
    pub coded_flags:    u32,
    pub cmode:          u8,
    pub qp_y:           u8,
    pub qp_u:           u8,
    pub qp_v:           u8,
    pub transform_8x8:  bool,
}

pub fn blk4_to_blk8(blk4: usize) -> usize {
    /*const MAP: [usize; 16] = [ 0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3 ];
    MAP[blk4 & 0xF]*/
    ((blk4 & 2) >> 1) | ((blk4 & 8) >> 2)
}

#[derive(Clone,Copy)]
pub struct Blk8Data {
    pub ref_idx:    [PicRef; 2],
    pub ncoded_c:   [u8; 2],
}

impl Default for Blk8Data {
    fn default() -> Self {
        Self {
            ref_idx:        [MISSING_REF; 2],
            ncoded_c:       [0; 2],
        }
    }
}

#[derive(Clone,Copy,Default)]
pub struct Blk4Data {
    pub ncoded:     u8,
    pub ipred:      IntraPredMode,
    pub mv:         [MV; 2],
    pub mvd:        [MV; 2],
}

#[repr(align(8))]
#[derive(Default)]
pub struct MVCache {
    pub data:   [[MV; 2]; 25]
}

pub struct SliceStateCommon<T> {
    pub mb_x:           usize,
    pub mb_y:           usize,
    pub mb_w:           usize,
    pub mb_h:           usize,
    pub mb_start:       usize,

    pub mb:             GenericCache<MBData>,
    pub blk8:           GenericCache<Blk8Data>,
    pub blk4:           GenericCache<Blk4Data>,

    pub deblock:        [u8; 16],

    pub has_top:        bool,
    pub has_left:       bool,

    pub def_fill:       T,
    pub top_line_y:     Vec<T>,
    pub left_y:         [T; 17], // first element is top-left
    pub top_line_c:     [Vec<T>; 2],
    pub left_c:         [[T; 9]; 2],
}

pub const BLK4_TO_D8: [usize; 16] = [ 0, 0, 3, 3, 0, 0, 3, 3, 12, 12, 15, 15, 12, 12, 15, 15 ];

impl<T: Clone+Copy+Default> SliceStateCommon<T> {
    pub fn new_default() -> Self {
        Self {
            mb_x:       0,
            mb_y:       0,
            mb_w:       0,
            mb_h:       0,
            mb_start:   0,
            mb:         GenericCache::new(0, 0, MBData::default()),
            blk8:       GenericCache::new(0, 0, Blk8Data::default()),
            blk4:       GenericCache::new(0, 0, Blk4Data::default()),

            deblock:    [0; 16],

            has_top:    false,
            has_left:   false,

            def_fill:   T::default(),
            top_line_y: Vec::new(),
            left_y:     [T::default(); 17],
            top_line_c: [Vec::new(), Vec::new()],
            left_c:     [[T::default(); 9]; 2],
        }
    }
    pub fn reset(&mut self, mb_w: usize, mb_h: usize, mb_pos: usize) {
        self.mb_w     = mb_w;
        self.mb_h     = mb_h;
        self.mb_start = mb_pos;
        if mb_w > 0 {
            self.mb_x = mb_pos % mb_w;
            self.mb_y = mb_pos / mb_w;
        } else {
            self.mb_x = 0;
            self.mb_y = 0;
        }
        self.mb    = GenericCache::new(1, mb_w     + 2, MBData::default());
        self.blk8  = GenericCache::new(2, mb_w * 2 + 2, Blk8Data::default());
        self.blk4  = GenericCache::new(4, mb_w * 4 + 2, Blk4Data::default());

        self.has_top  = false;
        self.has_left = false;

        self.top_line_y.resize(mb_w * 16 + 1, self.def_fill);
        self.top_line_c[0].resize(mb_w *  8 + 1, self.def_fill);
        self.top_line_c[1].resize(mb_w *  8 + 1, self.def_fill);
        self.left_y = [self.def_fill; 17];
        self.left_c = [[self.def_fill; 9]; 2];
    }
    pub fn next_mb(&mut self) {
        self.mb_x += 1;
        self.has_left = true;
        if self.mb_x == self.mb_w {
            self.mb_x = 0;
            self.mb_y += 1;
            self.mb.update_row();
            self.blk8.update_row();
            self.blk4.update_row();

            self.has_left = false;
        }
        self.has_top = self.mb_x + self.mb_y * self.mb_w >= self.mb_start + self.mb_w;
    }
    pub fn get_cur_mb_idx(&self) -> usize { self.mb.xpos + self.mb_x }
    pub fn get_cur_blk8_idx(&self, blk_no: usize) -> usize {
        self.blk8.xpos + self.mb_x * 2 + (blk_no & 1) + (blk_no >> 1) * self.blk8.stride
    }
    pub fn get_cur_blk4_idx(&self, blk_no: usize) -> usize {
        self.blk4.xpos + self.mb_x * 4 + (blk_no & 3) + (blk_no >> 2) * self.blk4.stride
    }
    pub fn get_cur_mb(&mut self) -> &mut MBData {
        let idx = self.get_cur_mb_idx();
        &mut self.mb.data[idx]
    }
    pub fn get_left_mb(&self) -> &MBData {
        &self.mb.data[self.get_cur_mb_idx() - 1]
    }
    pub fn get_top_mb(&self) -> &MBData {
        &self.mb.data[self.get_cur_mb_idx() - self.mb.stride]
    }
    pub fn get_cur_blk8(&mut self, blk_no: usize) -> &mut Blk8Data {
        let idx = self.get_cur_blk8_idx(blk_no);
        &mut self.blk8.data[idx]
    }
    pub fn get_left_blk8(&self, blk_no: usize) -> &Blk8Data {
        &self.blk8.data[self.get_cur_blk8_idx(blk_no) - 1]
    }
    pub fn get_top_blk8(&self, blk_no: usize) -> &Blk8Data {
        &self.blk8.data[self.get_cur_blk8_idx(blk_no) - self.blk8.stride]
    }
    pub fn get_cur_blk4(&mut self, blk_no: usize) -> &mut Blk4Data {
        let idx = self.get_cur_blk4_idx(blk_no);
        &mut self.blk4.data[idx]
    }
    pub fn get_left_blk4(&self, blk_no: usize) -> &Blk4Data {
        &self.blk4.data[self.get_cur_blk4_idx(blk_no) - 1]
    }
    pub fn get_top_blk4(&self, blk_no: usize) -> &Blk4Data {
        &self.blk4.data[self.get_cur_blk4_idx(blk_no) - self.blk4.stride]
    }

    pub fn apply_to_blk8<F: (Fn(&mut Blk8Data))>(&mut self, f: F) {
        let start = self.get_cur_blk8_idx(0);
        for row in self.blk8.data[start..].chunks_mut(self.blk8.stride).take(2) {
            for el in row[..2].iter_mut() {
                f(el);
            }
        }
    }
    pub fn apply_to_blk4<F: (Fn(&mut Blk4Data))>(&mut self, f: F) {
        let start = self.get_cur_blk4_idx(0);
        for row in self.blk4.data[start..].chunks_mut(self.blk4.stride).take(4) {
            for el in row[..4].iter_mut() {
                f(el);
            }
        }
    }

    pub fn fill_ipred(&mut self, imode: IntraPredMode) {
        self.apply_to_blk4(|blk| blk.ipred = imode);
    }
    pub fn fill_ncoded(&mut self, nc: u8) {
        self.apply_to_blk4(|blk| blk.ncoded = nc);
        self.apply_to_blk8(|blk| blk.ncoded_c = [nc; 2]);
    }
    pub fn reset_mb_mv(&mut self) {
        self.apply_to_blk8(|blk| blk.ref_idx = [INVALID_REF; 2]);
    }

    pub fn get_mv_ctx(&self, xoff: usize, yoff: usize, ref_l: usize) -> (usize, usize) {
        let blk_no = xoff / 4 + yoff;
        let mv_a = self.get_left_blk4(blk_no).mvd[ref_l];
        let mv_b = self.get_top_blk4(blk_no).mvd[ref_l];
        let mv = mv_a + mv_b;
        let ctx0 = if mv.x < 3 { 0 } else if mv.x <= 32 { 1 } else { 2 };
        let ctx1 = if mv.y < 3 { 0 } else if mv.y <= 32 { 1 } else { 2 };
        (ctx0, ctx1)
    }
    pub fn get_mv_ref_ctx(&self, xoff: usize, yoff: usize, ref_l: usize) -> usize {
        let blk_no = xoff / 8 + (yoff / 8) * 2;
        let mut ctx = 0;
        let left_ref = self.get_left_blk8(blk_no).ref_idx[ref_l];
        let top_ref = self.get_top_blk8(blk_no).ref_idx[ref_l];
        if !left_ref.not_avail() && !left_ref.is_direct() && left_ref.index() > 0 {
            ctx += 1;
        }
        if !top_ref.not_avail() && !top_ref.is_direct() && top_ref.index() > 0 {
            ctx += 2;
        }
        ctx
    }
    #[allow(clippy::if_same_then_else)]
    pub fn predict(&mut self, xpos: usize, ypos: usize, bw: usize, bh: usize, ref_l: usize, diff_mv: MV, ref_idx: PicRef) {
        let midx = self.get_cur_blk4_idx(0) + xpos / 4 + ypos / 4 * self.blk4.stride;
        let ridx = self.get_cur_blk8_idx(0) + xpos / 8 + ypos / 8 * self.blk8.stride;
        let ridx_c = self.get_cur_blk8_idx(0) + (xpos + bw) / 8 + ypos / 8 * self.blk8.stride - if (ypos & 4) == 0 { self.blk8.stride } else { 0 };

        let mv_a = self.blk4.data[midx - 1].mv[ref_l];
        let mv_b = self.blk4.data[midx - self.blk4.stride].mv[ref_l];
        let mut mv_c = self.blk4.data[midx - self.blk4.stride + bw / 4].mv[ref_l];

        let rx = if (xpos & 4) != 0 { 0 } else { 1 };
        let ry = if (ypos & 4) != 0 { 0 } else { self.blk8.stride };
        let ref_a = self.blk8.data[ridx - rx].ref_idx[ref_l];
        let ref_b = self.blk8.data[ridx - ry].ref_idx[ref_l];
        let mut ref_c = self.blk8.data[ridx_c].ref_idx[ref_l];

        if ref_c == MISSING_REF || (((xpos + bw) & 4) == 0 && (ypos & 4) != 0) {
            mv_c = self.blk4.data[midx - self.blk4.stride - 1].mv[ref_l];
            ref_c = self.blk8.data[ridx - rx - ry].ref_idx[ref_l];
        }

        let pred_mv = if bw == 16 && bh == 8 && ypos == 0 && ref_b == ref_idx {
                mv_b
            } else if bw == 16 && bh == 8 && ypos != 0 && ref_a == ref_idx {
                mv_a
            } else if bw == 8 && bh == 16 && xpos == 0 && ref_a == ref_idx {
                mv_a
            } else if bw == 8 && bh == 16 && xpos != 0 && ref_c == ref_idx {
                mv_c
            } else if ref_b == MISSING_REF && ref_c == MISSING_REF {
                mv_a
            } else {
                let count = ((ref_a == ref_idx) as u8) + ((ref_b == ref_idx) as u8) + ((ref_c == ref_idx) as u8);
                if count == 1 {
                    if ref_a == ref_idx {
                        mv_a
                    } else if ref_b == ref_idx {
                        mv_b
                    } else {
                        mv_c
                    }
                } else {
                    MV::pred(mv_a, mv_b, mv_c)
                }
            };

        let mv = pred_mv + diff_mv;
        self.fill_mv (xpos, ypos, bw, bh, ref_l, mv);
        self.fill_ref(xpos, ypos, bw, bh, ref_l, ref_idx);
    }
    pub fn predict_pskip(&mut self) {
        let midx = self.get_cur_blk4_idx(0);
        let ridx = self.get_cur_blk8_idx(0);

        let mv_a = self.blk4.data[midx - 1].mv[0];
        let mv_b = self.blk4.data[midx - self.blk4.stride].mv[0];
        let mut mv_c = self.blk4.data[midx - self.blk4.stride + 4].mv[0];

        let ref_a = self.blk8.data[ridx - 1].ref_idx[0];
        let ref_b = self.blk8.data[ridx - self.blk8.stride].ref_idx[0];
        let mut ref_c = self.blk8.data[ridx - self.blk8.stride + 2].ref_idx[0];

        if ref_c == MISSING_REF {
            mv_c = self.blk4.data[midx - self.blk4.stride - 1].mv[0];
            ref_c = self.blk8.data[ridx - self.blk8.stride - 1].ref_idx[0];
        }

        let ref_idx = ZERO_REF;
        let mv = if ref_a == MISSING_REF || ref_b == MISSING_REF || (ref_a == ZERO_REF && mv_a == ZERO_MV) || (ref_b == ZERO_REF && mv_b == ZERO_MV) {
                ZERO_MV
            } else {
                let count = ((ref_a == ref_idx) as u8) + ((ref_b == ref_idx) as u8) + ((ref_c == ref_idx) as u8);
                if count == 1 {
                    if ref_a == ref_idx {
                        mv_a
                    } else if ref_b == ref_idx {
                        mv_b
                    } else {
                        mv_c
                    }
                } else {
                    MV::pred(mv_a, mv_b, mv_c)
                }
            };

        self.fill_mv (0, 0, 16, 16, 0, mv);
        self.fill_ref(0, 0, 16, 16, 0, ref_idx);
    }
    pub fn fill_mv(&mut self, xpos: usize, ypos: usize, bw: usize, bh: usize, ref_l: usize, mv: MV) {
        let start = self.get_cur_blk4_idx(0) + xpos / 4 + ypos / 4 * self.blk4.stride;
        for row in self.blk4.data[start..].chunks_mut(self.blk4.stride).take(bh / 4) {
            for blk in row[..bw / 4].iter_mut() {
                blk.mv[ref_l] = mv;
            }
        }
    }
    pub fn fill_mvd(&mut self, xpos: usize, ypos: usize, bw: usize, bh: usize, ref_l: usize, mv: MV) {
        let mvd = MV{ x: mv.x.abs().min(128), y: mv.y.abs().min(128) };
        let start = self.get_cur_blk4_idx(0) + xpos / 4 + ypos / 4 * self.blk4.stride;
        for row in self.blk4.data[start..].chunks_mut(self.blk4.stride).take(bh / 4) {
            for blk in row[..bw / 4].iter_mut() {
                blk.mvd[ref_l] = mvd;
            }
        }
    }
    pub fn fill_ref(&mut self, xpos: usize, ypos: usize, bw: usize, bh: usize, ref_l: usize, ref_idx: PicRef) {
        let start = self.get_cur_blk8_idx(0) + xpos / 8 + ypos / 8 * self.blk8.stride;
        if bw < 8 || bh < 8 {
            self.blk8.data[start].ref_idx[ref_l] = ref_idx;
        } else {
            for row in self.blk8.data[start..].chunks_mut(self.blk8.stride).take(bh / 8) {
                for blk in row[..bw / 8].iter_mut() {
                    blk.ref_idx[ref_l] = ref_idx;
                }
            }
        }
    }
}

#[cfg(not(target_arch="x86_64"))]
pub fn mvdiff4(mv1: &[MV; 2], mv2: &[MV; 2]) -> bool {
    let mvd0 = mv1[0] - mv2[0];
    let mvd1 = mv1[1] - mv2[1];
    (mvd0.x.abs() >= 4) || (mvd0.y.abs() >= 4) || (mvd1.x.abs() >= 4) || (mvd1.y.abs() >= 4)
}

#[cfg(target_arch="x86_64")]
pub fn mvdiff4(mv1: &[MV; 2], mv2: &[MV; 2]) -> bool {
    unsafe {
        let mut flag = false;
        let ptr = std::mem::transmute::<*const MV, *const u64>(mv1.as_ptr());
        let mut m0 = *ptr;
        let ptr = std::mem::transmute::<*const MV, *const u64>(mv2.as_ptr());
        let mut m1 = *ptr;
        for _ in 0..4 {
            let tmp = m0.wrapping_sub(m1) as u16;
            flag |= tmp.wrapping_add(3) > 6;
            m0 >>= 16;
            m1 >>= 16;
        }
        flag
    }
}
