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
