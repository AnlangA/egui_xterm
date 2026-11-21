//! SGR (`CSI ... m`) decoder supporting semicolon and ITU-T T.416 colon subparameters.

use crate::cell::{CellFlags, UnderlineStyle};
use crate::color::PackedColor;
use crate::parser::CsiParams;
use crate::term::cursor::CursorAttrs;

fn parse_colon_color(sub: &[u16]) -> Option<PackedColor> {
    match *sub.get(1)? {
        5 => {
            let idx = u8::try_from(*sub.get(2)?).ok()?;
            Some(PackedColor::Indexed(idx))
        }
        2 => {
            // 6+ subparams: 38:2:<colorspace>:r:g:b
            // 5 subparams:  38:2:r:g:b (omitted colorspace slot)
            let (r, g, b) = if sub.len() >= 6 {
                (sub[3], sub[4], sub[5])
            } else if sub.len() == 5 {
                (sub[2], sub[3], sub[4])
            } else {
                return None;
            };
            let r = u8::try_from(r).ok()?;
            let g = u8::try_from(g).ok()?;
            let b = u8::try_from(b).ok()?;
            Some(PackedColor::rgb(r, g, b))
        }
        _ => None,
    }
}

fn parse_semicolon_color(params: &CsiParams, idx: &mut usize) -> Option<PackedColor> {
    let mode = params.get(*idx + 1)?;
    match mode {
        5 => {
            let Some(color) = params.get(*idx + 2) else {
                *idx = params.len();
                return None;
            };
            *idx += 2;
            u8::try_from(color).ok().map(PackedColor::Indexed)
        }
        2 => {
            let (Some(r), Some(g), Some(b)) = (
                params.get(*idx + 2),
                params.get(*idx + 3),
                params.get(*idx + 4),
            ) else {
                *idx = params.len();
                return None;
            };
            *idx += 4;
            let r = u8::try_from(r).ok()?;
            let g = u8::try_from(g).ok()?;
            let b = u8::try_from(b).ok()?;
            Some(PackedColor::rgb(r, g, b))
        }
        _ => {
            *idx = params.len();
            None
        }
    }
}

/// Applies `CSI ... m` SGR attributes in `params` onto `attrs`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "ANSI color index differences are in 0..=7"
)]
pub(crate) fn apply_sgr(params: &CsiParams, attrs: &mut CursorAttrs) {
    if params.is_empty() {
        attrs.reset_sgr();
        return;
    }

    let mut idx = 0usize;
    while idx < params.len() {
        let Some(sub) = params.subparams(idx) else {
            break;
        };

        if sub.len() > 1 {
            match sub[0] {
                4 => {
                    if let Some(style) = UnderlineStyle::from_sgr_subparam(sub[1]) {
                        attrs.flags.set_underline_style(style);
                    }
                }
                38 => {
                    if let Some(c) = parse_colon_color(sub) {
                        attrs.fg = c;
                    }
                }
                48 => {
                    if let Some(c) = parse_colon_color(sub) {
                        attrs.bg = c;
                    }
                }
                58 => {
                    if let Some(c) = parse_colon_color(sub) {
                        attrs.underline_color = Some(c);
                    }
                }
                _ => {}
            }
            idx += 1;
            continue;
        }

        let code = sub[0];
        match code {
            0 => attrs.reset_sgr(),
            1 => attrs.flags.insert(CellFlags::BOLD),
            2 => attrs.flags.insert(CellFlags::DIM),
            3 => attrs.flags.insert(CellFlags::ITALIC),
            4 => attrs.flags.set_underline_style(UnderlineStyle::Single),
            5 => attrs.flags.insert(CellFlags::BLINK_SLOW),
            6 => attrs.flags.insert(CellFlags::BLINK_RAPID),
            7 => attrs.flags.insert(CellFlags::INVERSE),
            8 => attrs.flags.insert(CellFlags::HIDDEN),
            9 => attrs.flags.insert(CellFlags::STRIKETHROUGH),
            21 => attrs.flags.set_underline_style(UnderlineStyle::Double),
            22 => attrs.flags.remove(CellFlags::BOLD | CellFlags::DIM),
            23 => attrs.flags.remove(CellFlags::ITALIC),
            24 => attrs.flags.set_underline_style(UnderlineStyle::None),
            25 => attrs
                .flags
                .remove(CellFlags::BLINK_SLOW | CellFlags::BLINK_RAPID),
            27 => attrs.flags.remove(CellFlags::INVERSE),
            28 => attrs.flags.remove(CellFlags::HIDDEN),
            29 => attrs.flags.remove(CellFlags::STRIKETHROUGH),
            30..=37 => attrs.fg = PackedColor::Indexed((code - 30) as u8),
            38 => {
                if let Some(c) = parse_semicolon_color(params, &mut idx) {
                    attrs.fg = c;
                }
            }
            39 => attrs.fg = PackedColor::Default,
            40..=47 => attrs.bg = PackedColor::Indexed((code - 40) as u8),
            48 => {
                if let Some(c) = parse_semicolon_color(params, &mut idx) {
                    attrs.bg = c;
                }
            }
            49 => attrs.bg = PackedColor::Default,
            53 => attrs.flags.insert(CellFlags::OVERLINE),
            55 => attrs.flags.remove(CellFlags::OVERLINE),
            58 => {
                if let Some(c) = parse_semicolon_color(params, &mut idx) {
                    attrs.underline_color = Some(c);
                }
            }
            59 => attrs.underline_color = None,
            90..=97 => attrs.fg = PackedColor::Indexed((code - 90 + 8) as u8),
            100..=107 => attrs.bg = PackedColor::Indexed((code - 100 + 8) as u8),
            _ => {}
        }

        idx += 1;
    }
}

const MAX_SGR_STACK: usize = 10;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SgrPushMask: u16 {
        const BOLD          = 1 << 0;
        const DIM           = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3;
        const BLINK_SLOW    = 1 << 4;
        const BLINK_RAPID   = 1 << 5;
        const INVERSE       = 1 << 6;
        const HIDDEN        = 1 << 7;
        const STRIKETHROUGH = 1 << 8;
        const OVERLINE      = 1 << 9;
        const FG            = 1 << 10;
        const BG            = 1 << 11;
    }
}

#[derive(Debug, Clone, Copy)]
struct SgrFrame {
    attrs: CursorAttrs,
    mask: SgrPushMask,
}

/// Bounded depth-10 stack for `XTPUSHSGR` (`CSI [Pm] # {` / `# p`) and `XTPOPSGR`
/// (`CSI # }` / `# q`), tracking overflow depth beyond 10 so balanced deep nesting restores
/// the stored levels intact.
#[derive(Debug, Clone, Default)]
pub(crate) struct SgrStack {
    frames: Vec<SgrFrame>,
    depth: usize,
}

impl SgrStack {
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.depth = 0;
    }

    pub(crate) fn push(&mut self, attrs: &CursorAttrs, params: &CsiParams) {
        let mask = if params.is_empty() {
            SgrPushMask::all()
        } else {
            let mut m = SgrPushMask::empty();
            for sub in params {
                let Some(&code) = sub.first() else {
                    continue;
                };
                match code {
                    0 => {}
                    1 => m.insert(SgrPushMask::BOLD),
                    2 => m.insert(SgrPushMask::DIM),
                    3 => m.insert(SgrPushMask::ITALIC),
                    4 | 21 => m.insert(SgrPushMask::UNDERLINE),
                    5 => m.insert(SgrPushMask::BLINK_SLOW),
                    6 => m.insert(SgrPushMask::BLINK_RAPID),
                    7 => m.insert(SgrPushMask::INVERSE),
                    8 => m.insert(SgrPushMask::HIDDEN),
                    9 => m.insert(SgrPushMask::STRIKETHROUGH),
                    10 | 30 => m.insert(SgrPushMask::FG),
                    11 | 31 => m.insert(SgrPushMask::BG),
                    53 => m.insert(SgrPushMask::OVERLINE),
                    _ => {}
                }
            }
            m
        };

        if self.depth < MAX_SGR_STACK {
            self.frames.push(SgrFrame {
                attrs: *attrs,
                mask,
            });
        }
        self.depth = self.depth.saturating_add(1);
    }

    pub(crate) fn pop(&mut self, attrs: &mut CursorAttrs) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth < MAX_SGR_STACK
            && let Some(frame) = self.frames.pop()
        {
            let m = frame.mask;
            if m == SgrPushMask::all() {
                let protected = attrs.flags & CellFlags::PROTECTED;
                let hyperlink = attrs.hyperlink_id;
                attrs.fg = frame.attrs.fg;
                attrs.bg = frame.attrs.bg;
                attrs.flags = (frame.attrs.flags & CellFlags::SGR_MASK) | protected;
                attrs.underline_color = frame.attrs.underline_color;
                attrs.hyperlink_id = hyperlink;
                return;
            }
            if m.contains(SgrPushMask::FG) {
                attrs.fg = frame.attrs.fg;
            }
            if m.contains(SgrPushMask::BG) {
                attrs.bg = frame.attrs.bg;
            }
            if m.contains(SgrPushMask::UNDERLINE) {
                attrs
                    .flags
                    .set_underline_style(frame.attrs.flags.underline_style());
                attrs.underline_color = frame.attrs.underline_color;
            }
            let flag_pairs = [
                (SgrPushMask::BOLD, CellFlags::BOLD),
                (SgrPushMask::DIM, CellFlags::DIM),
                (SgrPushMask::ITALIC, CellFlags::ITALIC),
                (SgrPushMask::BLINK_SLOW, CellFlags::BLINK_SLOW),
                (SgrPushMask::BLINK_RAPID, CellFlags::BLINK_RAPID),
                (SgrPushMask::INVERSE, CellFlags::INVERSE),
                (SgrPushMask::HIDDEN, CellFlags::HIDDEN),
                (SgrPushMask::STRIKETHROUGH, CellFlags::STRIKETHROUGH),
                (SgrPushMask::OVERLINE, CellFlags::OVERLINE),
            ];
            for (mask_bit, cell_bit) in flag_pairs {
                if m.contains(mask_bit) {
                    attrs
                        .flags
                        .set(cell_bit, frame.attrs.flags.contains(cell_bit));
                }
            }
        }
    }
}
