//! Kitty Graphics Protocol (`APC G <key=val,...>;<base64>`) parser, executor, and Unicode
//! placeholder helpers.

use std::fmt::Write as _;

use crate::cell::{Cell, CellExtra};
use crate::color::PackedColor;
use crate::grid::{AbsLine, ScreenKind};

use super::base64::decode_base64;
use super::inflate::inflate_zlib;
use super::png::decode_png;
use super::store::{
    AnimationPlaybackState, ImageFormat, ImagePlacement, ImageStore, MAX_IMAGE_DIMENSION,
    MAX_RELATIVE_PLACEMENT_DEPTH, PendingChunkKey, PendingKittyUpload, StoredImage,
};

/// Maximum total queued base64 bytes across all concurrent chunked Kitty transfers (32 MiB).
const MAX_KITTY_PENDING_BYTES: usize = 32 * 1024 * 1024;

/// Maximum number of concurrent in-flight chunked Kitty transfers.
const MAX_KITTY_PENDING_TRANSFERS: usize = 16;

/// Private-use Unicode scalar value used as the base cell for Kitty Unicode placeholders.
///
/// # Examples
///
/// Create a Kitty virtual placement (`U=1`) and decode a placeholder cell written
/// with `U+10EEEE` and [`KITTY_DIACRITICS`]:
///
/// ```
/// use egui_xterm_core::{
///     CoreConfig, KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, Terminal, decode_placeholder_cell,
/// };
///
/// let mut term = Terminal::new(20, 4, CoreConfig::default());
/// term.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,p=7,U=1,c=2,r=1,q=2;/wAA/w==\x1b\\");
///
/// let seq = format!(
///     "\x1b[38;5;42m\x1b[58;5;7m{KITTY_PLACEHOLDER_CHAR}{}{}",
///     KITTY_DIACRITICS[0], KITTY_DIACRITICS[1]
/// );
/// term.advance(seq.as_bytes());
///
/// let snap = term.active_snapshot();
/// let vp = snap
///     .virtual_placement(42, 7)
///     .ok_or("expected virtual placement")?;
/// assert_eq!((vp.cols, vp.rows), (2, 1));
///
/// let row = snap.visible_rows.get(0).ok_or("expected row 0")?;
/// let cell = row.cells().first().ok_or("expected col 0")?;
/// let decoded = decode_placeholder_cell(cell, snap.cell_extra(cell.extra_id))
///     .ok_or("expected decoded Kitty placeholder cell")?;
/// assert_eq!((decoded.image_id, decoded.placement_id), (42, 7));
/// assert_eq!((decoded.row, decoded.col), (Some(0), Some(1)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub const KITTY_PLACEHOLDER_CHAR: char = '\u{10EEEE}';

/// Canonical ordered table of 297 combining diacritics used by Kitty Unicode placeholders to
/// encode row, column, and image-ID MSB indices (`0..=296`).
pub const KITTY_DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// Maps a combining diacritic `ch` to its 0-based index (`0..=296`) in [`KITTY_DIACRITICS`].
#[must_use]
pub fn diacritic_index(ch: char) -> Option<u16> {
    KITTY_DIACRITICS
        .binary_search(&ch)
        .ok()
        .and_then(|idx| u16::try_from(idx).ok())
}

/// Decoded metadata from a single Kitty Unicode placeholder cell (`U+10EEEE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyPlaceholderCell {
    /// Lower 24 bits of the image ID extracted from cell foreground color.
    pub image_id_low24: u32,
    /// Full 32-bit image ID combining `id_msb` (`bits 24..31`) and `image_id_low24`.
    pub image_id: u32,
    /// Placement ID extracted from `SGR 58` underline color (`0` = unspecified / any).
    pub placement_id: u32,
    /// Zero-based tile row encoded in the 1st combining diacritic, if present.
    pub row: Option<u16>,
    /// Zero-based tile column encoded in the 2nd combining diacritic, if present.
    pub col: Option<u16>,
    /// Optional high byte (`bits 24..31`) of `image_id` from the 3rd combining diacritic.
    pub id_msb: Option<u8>,
}

impl KittyPlaceholderCell {
    /// Applies Kitty's left-to-right omission inheritance rules against the immediately preceding
    /// placeholder cell on the same row.
    #[must_use]
    pub const fn resolve_with_left(mut self, left: Option<Self>) -> Self {
        if let Some(prev) = left
            && prev.image_id_low24 == self.image_id_low24
            && prev.placement_id == self.placement_id
        {
            match (self.row, self.col, self.id_msb) {
                (None, None, None) => {
                    self.row = prev.row;
                    self.col = match prev.col {
                        Some(c) => Some(c.saturating_add(1)),
                        None => None,
                    };
                    self.id_msb = prev.id_msb;
                }
                (Some(r), None, None) if matches!(prev.row, Some(pr) if pr == r) => {
                    self.col = match prev.col {
                        Some(c) => Some(c.saturating_add(1)),
                        None => None,
                    };
                    self.id_msb = prev.id_msb;
                }
                (Some(r), Some(c), None)
                    if matches!(prev.row, Some(pr) if pr == r)
                        && matches!(prev.col, Some(pc) if pc + 1 == c) =>
                {
                    self.id_msb = prev.id_msb;
                }
                _ => {}
            }
        }
        let msb = match self.id_msb {
            Some(b) => b as u32,
            None => 0,
        };
        self.image_id = (msb << 24) | self.image_id_low24;
        self
    }
}

const fn color_to_id24(color: PackedColor) -> Option<u32> {
    match color {
        PackedColor::Default => None,
        PackedColor::Indexed(idx) => Some(idx as u32),
        PackedColor::Rgb(r, g, b) => Some(((r as u32) << 16) | ((g as u32) << 8) | (b as u32)),
    }
}

/// Decodes a Kitty Unicode placeholder [`Cell`] (`U+10EEEE`) and its optional [`CellExtra`].
///
/// Returns `None` if `cell.codepoint != KITTY_PLACEHOLDER_CHAR` or `cell.fg` is
/// [`PackedColor::Default`].
///
/// # Examples
///
/// ```
/// use egui_xterm_core::{
///     Cell, CellExtra, CellFlags, KITTY_DIACRITICS, KITTY_PLACEHOLDER_CHAR, PackedColor,
///     decode_placeholder_cell,
/// };
///
/// let mut first_extra = CellExtra::default();
/// first_extra.underline_color = Some(PackedColor::indexed(3));
/// first_extra.push_combining(KITTY_DIACRITICS[0]); // row 0
/// first_extra.push_combining(KITTY_DIACRITICS[0]); // col 0
///
/// let first_cell = Cell::new(
///     KITTY_PLACEHOLDER_CHAR,
///     PackedColor::rgb(0, 0, 42),
///     PackedColor::Default,
///     CellFlags::empty(),
/// );
/// let first = decode_placeholder_cell(&first_cell, Some(&first_extra))
///     .ok_or("expected valid first placeholder")?;
/// assert_eq!((first.image_id, first.placement_id), (42, 3));
/// assert_eq!((first.row, first.col), (Some(0), Some(0)));
///
/// // A trailing cell on the same row can omit diacritics and inherit `(row, col + 1)`.
/// let mut trailing_extra = CellExtra::default();
/// trailing_extra.underline_color = Some(PackedColor::indexed(3));
/// let trailing = decode_placeholder_cell(&first_cell, Some(&trailing_extra))
///     .ok_or("expected valid trailing placeholder")?
///     .resolve_with_left(Some(first));
/// assert_eq!((trailing.row, trailing.col), (Some(0), Some(1)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn decode_placeholder_cell(
    cell: &Cell,
    extra: Option<&CellExtra>,
) -> Option<KittyPlaceholderCell> {
    if cell.codepoint != KITTY_PLACEHOLDER_CHAR {
        return None;
    }
    let image_id_low24 = color_to_id24(cell.fg)?;
    let placement_id = extra
        .and_then(|e| e.underline_color)
        .and_then(color_to_id24)
        .unwrap_or(0);

    let mut diacritics = extra
        .map(|e| e.combining.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|&ch| diacritic_index(ch));

    let row = diacritics.next();
    let col = diacritics.next();
    let id_msb = diacritics.next().and_then(|idx| u8::try_from(idx).ok());
    let image_id = (u32::from(id_msb.unwrap_or(0)) << 24) | image_id_low24;

    Some(KittyPlaceholderCell {
        image_id_low24,
        image_id,
        placement_id,
        row,
        col,
        id_msb,
    })
}

/// Policy governing local filesystem and shared-memory Kitty Graphics mediums (`t=f`, `t=t`, `t=s`).
///
/// # Examples
///
/// Restrict or disable filesystem/temp-file Kitty uploads at construction or runtime:
///
/// ```
/// use egui_xterm_core::{CoreConfig, KittyFileMediumPolicy, Terminal};
///
/// let cfg = CoreConfig {
///     kitty_file_medium_policy: KittyFileMediumPolicy::Disabled,
///     ..CoreConfig::default()
/// };
/// let mut term = Terminal::new(40, 4, cfg);
/// assert_eq!(term.kitty_file_medium_policy(), KittyFileMediumPolicy::Disabled);
///
/// // Direct file (`t=f`) uploads are rejected with EBADF under Disabled.
/// term.advance(b"\x1b_Ga=t,t=f,i=1,s=1,v=1,f=32;L3RtcC9pbWcucmdi\x1b\\");
/// let reply = String::from_utf8(term.drain_outgoing())?;
/// assert!(reply.starts_with("\x1b_Gi=1;EBADF:"));
/// assert!(term.image_data(1).is_none());
///
/// // TempOnly still rejects temp-file (`t=t`) paths outside `"tty-graphics-protocol"`.
/// term.set_kitty_file_medium_policy(KittyFileMediumPolicy::TempOnly);
/// assert_eq!(term.kitty_file_medium_policy(), KittyFileMediumPolicy::TempOnly);
/// term.advance(b"\x1b_Ga=t,t=t,i=2,s=1,v=1,f=32;L3RtcC9pbWcucmdi\x1b\\");
/// let reply2 = String::from_utf8(term.drain_outgoing())?;
/// assert!(reply2.starts_with("\x1b_Gi=2;EBADF:"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum KittyFileMediumPolicy {
    /// Reject all `t=f`, `t=t`, and `t=s` transfers with `EBADF`.
    Disabled,
    /// Allow only temp-file (`t=t`) transfers inside a recognized temp directory whose path
    /// contains `"tty-graphics-protocol"`.
    TempOnly,
    /// Allow safe regular files (`t=f`), temp files (`t=t`), and shared-memory (`t=s`) transfers
    /// on non-WASM targets (ignored with `EBADF` on `wasm32`).
    #[default]
    LocalFiles,
}

/// Optional host callback for reading and unlinking POSIX/Windows shared-memory objects (`t=s`)
/// without introducing `unsafe` code in `egui_xterm_core`.
pub trait KittyShmReader {
    /// Reads up to `size` bytes (`0` = read to end, bounded by `max_bytes`) starting at `offset`
    /// from shared-memory object `name`, and unlinks/closes the object on completion.
    fn read_and_unlink(
        &self,
        name: &str,
        offset: u64,
        size: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, &'static str>;
}

/// Transport policy and optional shared-memory reader for Kitty Graphics APC handling.
#[derive(Clone, Copy)]
pub struct KittyTransportPolicy<'a> {
    /// Policy governing `t=f`, `t=t`, and `t=s` transfers.
    pub file_policy: KittyFileMediumPolicy,
    /// Maximum raw byte count read from a file or shared-memory object.
    pub max_read_bytes: usize,
    /// Optional host-provided shared-memory reader for `t=s` on platforms lacking `/dev/shm`.
    pub shm_reader: Option<&'a dyn KittyShmReader>,
}

impl std::fmt::Debug for KittyTransportPolicy<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KittyTransportPolicy")
            .field("file_policy", &self.file_policy)
            .field("max_read_bytes", &self.max_read_bytes)
            .field("has_shm_reader", &self.shm_reader.is_some())
            .finish()
    }
}

impl Default for KittyTransportPolicy<'_> {
    fn default() -> Self {
        Self {
            file_policy: KittyFileMediumPolicy::default(),
            max_read_bytes: MAX_KITTY_PENDING_BYTES,
            shm_reader: None,
        }
    }
}

/// Execution context for processing a Kitty Graphics Protocol `APC` sequence.
#[derive(Debug, Clone, Copy)]
pub struct KittyApcContext<'a> {
    /// Active screen buffer (`Primary` or `Alternate`).
    pub screen: ScreenKind,
    /// Absolute line corresponding to visible viewport row `0`.
    pub viewport_top_abs_line: AbsLine,
    /// Number of visible rows in the active viewport (`0` when unbounded).
    pub viewport_rows: u16,
    /// Cursor absolute line at the time the APC is dispatched.
    pub cursor_abs_line: AbsLine,
    /// Cursor column (`0`-based) at the time the APC is dispatched.
    pub cursor_col: u16,
    /// Effective cell dimensions `(width_px, height_px)`.
    pub cell_px: (u16, u16),
    /// Filesystem/shared-memory medium access policy.
    pub policy: KittyTransportPolicy<'a>,
}

/// Kitty Graphics Protocol action (`a=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum KittyAction {
    /// Transmit image data without displaying (`a=t`).
    #[default]
    Transmit,
    /// Transmit image data and display immediately (`a=T`).
    TransmitAndDisplay,
    /// Put/display a previously transmitted image (`a=p`).
    Put,
    /// Delete images/placements/frames (`a=d`).
    Delete,
    /// Upload or edit an animation frame (`a=f`).
    Frame,
    /// Control animation playback/state/frame gaps (`a=a`).
    Animation,
    /// Compose one animation frame rectangle onto another (`a=c`).
    Compose,
    /// Query protocol or feature support (`a=q`).
    Query,
}

/// Outcome returned to [`Terminal`](crate::term::Terminal) after executing a Kitty Graphics APC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyOutcome {
    /// Whether image store or placements changed.
    pub modified: bool,
    /// Whether the cursor should advance after placing an image.
    pub move_cursor: bool,
    /// Number of columns to advance when `move_cursor` is `true`.
    pub advance_cols: u16,
    /// Number of rows to advance when `move_cursor` is `true`.
    pub advance_rows: u16,
}

#[derive(Debug, Clone, Copy)]
struct KittyParams {
    action: Option<KittyAction>,
    feature_query: Option<u8>,
    format: u32,
    has_format: bool,
    medium: u8,
    has_medium: bool,
    compressed: bool,
    has_compression: bool,
    more: bool,
    quiet: u8,
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    width: u32,
    has_width: bool,
    height: u32,
    has_height: bool,
    file_size: u64,
    file_offset: u64,
    c_val: u32,
    has_c: bool,
    r_val: u32,
    has_r: bool,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    cap_x: u32,
    has_cap_x: bool,
    cap_y: u32,
    has_cap_y: bool,
    cap_c: u32,
    has_cap_c: bool,
    u_val: u32,
    has_u: bool,
    z_val: i32,
    has_z: bool,
    parent_image_id: u32,
    parent_placement_id: u32,
    offset_cols: i32,
    offset_rows: i32,
    delete_target: u8,
}

impl Default for KittyParams {
    fn default() -> Self {
        Self {
            action: None,
            feature_query: None,
            format: 32,
            has_format: false,
            medium: b'd',
            has_medium: false,
            compressed: false,
            has_compression: false,
            more: false,
            quiet: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            width: 0,
            has_width: false,
            height: 0,
            has_height: false,
            file_size: 0,
            file_offset: 0,
            c_val: 0,
            has_c: false,
            r_val: 0,
            has_r: false,
            src_x: 0,
            src_y: 0,
            src_w: 0,
            src_h: 0,
            cap_x: 0,
            has_cap_x: false,
            cap_y: 0,
            has_cap_y: false,
            cap_c: 0,
            has_cap_c: false,
            u_val: 0,
            has_u: false,
            z_val: 0,
            has_z: false,
            parent_image_id: 0,
            parent_placement_id: 0,
            offset_cols: 0,
            offset_rows: 0,
            delete_target: b'a',
        }
    }
}

impl KittyParams {
    fn is_continuation_header(&self) -> bool {
        (self.action.is_none() || self.action == Some(KittyAction::Frame))
            && self.feature_query.is_none()
            && !self.has_format
            && !self.has_medium
            && !self.has_compression
            && !self.has_width
            && !self.has_height
            && self.file_size == 0
            && self.file_offset == 0
            && !self.has_c
            && !self.has_r
            && self.placement_id == 0
            && self.src_x == 0
            && self.src_y == 0
            && self.src_w == 0
            && self.src_h == 0
            && !self.has_cap_x
            && !self.has_cap_y
            && !self.has_cap_c
            && !self.has_u
            && !self.has_z
            && self.parent_image_id == 0
            && self.parent_placement_id == 0
            && self.offset_cols == 0
            && self.offset_rows == 0
    }
}

fn parse_u32(val: &[u8]) -> Option<u32> {
    std::str::from_utf8(val).ok()?.parse::<u32>().ok()
}

fn parse_u64(val: &[u8]) -> Option<u64> {
    std::str::from_utf8(val).ok()?.parse::<u64>().ok()
}

fn parse_i32(val: &[u8]) -> Option<i32> {
    std::str::from_utf8(val).ok()?.parse::<i32>().ok()
}

fn parse_header(header: &[u8]) -> KittyParams {
    let mut params = KittyParams::default();
    if header.is_empty() {
        return params;
    }
    for pair in header.split(|&b| b == b',') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = &pair[..eq];
        let val = &pair[eq + 1..];
        match key {
            b"a" => {
                params.action = match val {
                    b"t" => Some(KittyAction::Transmit),
                    b"T" => Some(KittyAction::TransmitAndDisplay),
                    b"p" => Some(KittyAction::Put),
                    b"d" => Some(KittyAction::Delete),
                    b"f" => Some(KittyAction::Frame),
                    b"a" => Some(KittyAction::Animation),
                    b"c" => Some(KittyAction::Compose),
                    b"q" => Some(KittyAction::Query),
                    _ => params.action,
                };
            }
            b"e" => {
                if let Some(&b) = val.first()
                    && val.len() == 1
                {
                    params.feature_query = Some(b);
                }
            }
            b"t" => {
                if let Some(&b) = val.first()
                    && val.len() == 1
                {
                    params.medium = b;
                    params.has_medium = true;
                }
            }
            b"f" => {
                if let Some(v) = parse_u32(val) {
                    params.format = v;
                    params.has_format = true;
                }
            }
            b"o" => {
                params.compressed = val == b"z";
                params.has_compression = true;
            }
            b"m" => params.more = val == b"1",
            b"q" => {
                if let Some(v) = parse_u32(val) {
                    params.quiet = u8::try_from(v.min(2)).unwrap_or(2);
                }
            }
            b"i" => {
                if let Some(v) = parse_u32(val) {
                    params.image_id = v;
                }
            }
            b"I" => {
                if let Some(v) = parse_u32(val) {
                    params.image_number = v;
                }
            }
            b"p" => {
                if let Some(v) = parse_u32(val) {
                    params.placement_id = v;
                }
            }
            b"s" => {
                if let Some(v) = parse_u32(val) {
                    params.width = v;
                    params.has_width = true;
                }
            }
            b"v" => {
                if let Some(v) = parse_u32(val) {
                    params.height = v;
                    params.has_height = true;
                }
            }
            b"S" => {
                if let Some(v) = parse_u64(val) {
                    params.file_size = v;
                }
            }
            b"O" => {
                if let Some(v) = parse_u64(val) {
                    params.file_offset = v;
                }
            }
            b"c" => {
                if let Some(v) = parse_u32(val) {
                    params.c_val = v;
                    params.has_c = true;
                }
            }
            b"r" => {
                if let Some(v) = parse_u32(val) {
                    params.r_val = v;
                    params.has_r = true;
                }
            }
            b"x" => {
                if let Some(v) = parse_u32(val) {
                    params.src_x = v;
                }
            }
            b"y" => {
                if let Some(v) = parse_u32(val) {
                    params.src_y = v;
                }
            }
            b"w" => {
                if let Some(v) = parse_u32(val) {
                    params.src_w = v;
                }
            }
            b"h" => {
                if let Some(v) = parse_u32(val) {
                    params.src_h = v;
                }
            }
            b"X" => {
                if let Some(v) = parse_u32(val) {
                    params.cap_x = v;
                    params.has_cap_x = true;
                }
            }
            b"Y" => {
                if let Some(v) = parse_u32(val) {
                    params.cap_y = v;
                    params.has_cap_y = true;
                }
            }
            b"z" => {
                if let Some(v) = parse_i32(val) {
                    params.z_val = v;
                    params.has_z = true;
                }
            }
            b"C" => {
                if let Some(v) = parse_u32(val) {
                    params.cap_c = v;
                    params.has_cap_c = true;
                }
            }
            b"U" => {
                if let Some(v) = parse_u32(val) {
                    params.u_val = v;
                    params.has_u = true;
                }
            }
            b"P" => {
                if let Some(v) = parse_u32(val) {
                    params.parent_image_id = v;
                }
            }
            b"Q" => {
                if let Some(v) = parse_u32(val) {
                    params.parent_placement_id = v;
                }
            }
            b"H" => {
                if let Some(v) = parse_i32(val) {
                    params.offset_cols = v;
                }
            }
            b"V" => {
                if let Some(v) = parse_i32(val) {
                    params.offset_rows = v;
                }
            }
            b"d" => {
                if let Some(&b) = val.first() {
                    params.delete_target = b;
                }
            }
            _ => {}
        }
    }
    params
}

fn write_response(
    outgoing: &mut Vec<u8>,
    image_id: u32,
    image_number: u32,
    placement_id: u32,
    quiet: u8,
    status: Result<(), &str>,
) {
    let is_ok = status.is_ok();
    if is_ok && quiet >= 1 {
        return;
    }
    if !is_ok && quiet >= 2 {
        return;
    }
    if image_id == 0 && image_number == 0 && is_ok {
        return;
    }

    let mut resp = String::from("\x1b_G");
    if image_id != 0 {
        if write!(resp, "i={image_id}").is_err() {
            // `String`'s `fmt::Write` impl is infallible.
        }
    } else if image_number != 0 && write!(resp, "I={image_number}").is_err() {
        // `String`'s `fmt::Write` impl is infallible.
    }
    if placement_id != 0 && write!(resp, ",p={placement_id}").is_err() {
        // `String`'s `fmt::Write` impl is infallible.
    }
    match status {
        Ok(()) => resp.push_str(";OK\x1b\\"),
        Err(msg) => {
            if write!(resp, ";{msg}\x1b\\").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
    }
    outgoing.extend_from_slice(resp.as_bytes());
}

fn push_fmt(out: &mut String, args: std::fmt::Arguments<'_>) {
    if std::fmt::write(out, args).is_err() {
        // `String`'s `fmt::Write` impl is infallible.
    }
}

fn execute_feature_query(
    params: &KittyParams,
    policy: &KittyTransportPolicy<'_>,
    outgoing: &mut Vec<u8>,
) {
    if params.quiet >= 1 {
        return;
    }
    if params.image_id == 0 && params.image_number == 0 {
        return;
    }
    let Some(target) = params.feature_query else {
        return;
    };

    let mut resp = String::from("\x1b_G");
    if params.image_id != 0 {
        push_fmt(&mut resp, format_args!("i={}", params.image_id));
    } else if params.image_number != 0 {
        push_fmt(&mut resp, format_args!("I={}", params.image_number));
    }

    let supported_action = matches!(
        target,
        b't' | b'T' | b'p' | b'd' | b'f' | b'a' | b'c' | b'q'
    );
    if supported_action {
        let ch = target as char;
        push_fmt(&mut resp, format_args!(",e={ch}"));
        if matches!(target, b't' | b'T' | b'f' | b'q') {
            if params.has_format && matches!(params.format, 24 | 32 | 100) {
                push_fmt(&mut resp, format_args!(",f={}", params.format));
            }
            if params.has_compression && params.compressed {
                resp.push_str(",o=z");
            }
            if params.has_medium {
                let medium_ok = match params.medium {
                    b'd' => true,
                    b'f' => {
                        !cfg!(target_arch = "wasm32")
                            && policy.file_policy == KittyFileMediumPolicy::LocalFiles
                    }
                    b't' => {
                        !cfg!(target_arch = "wasm32")
                            && policy.file_policy != KittyFileMediumPolicy::Disabled
                    }
                    b's' => {
                        !cfg!(target_arch = "wasm32")
                            && policy.file_policy == KittyFileMediumPolicy::LocalFiles
                            && (cfg!(any(target_os = "linux", target_os = "freebsd"))
                                || policy.shm_reader.is_some())
                    }
                    _ => false,
                };
                if medium_ok && params.medium != b'd' {
                    let mch = params.medium as char;
                    push_fmt(&mut resp, format_args!(",t={mch}"));
                }
            }
        }
        if matches!(target, b'T' | b'p') {
            if params.has_cap_c && params.cap_c == 1 {
                resp.push_str(",C=1");
            }
            if params.has_u && params.u_val == 1 {
                resp.push_str(",U=1");
            }
            if params.parent_image_id != 0 {
                push_fmt(&mut resp, format_args!(",P={}", params.parent_image_id));
            }
            if params.parent_placement_id != 0 {
                push_fmt(&mut resp, format_args!(",Q={}", params.parent_placement_id));
            }
            if params.offset_cols != 0 {
                push_fmt(&mut resp, format_args!(",H={}", params.offset_cols));
            }
            if params.offset_rows != 0 {
                push_fmt(&mut resp, format_args!(",V={}", params.offset_rows));
            }
        }
        if target == b'f' {
            if params.has_r && params.r_val != 0 {
                push_fmt(&mut resp, format_args!(",r={}", params.r_val));
            }
            if params.has_c && params.c_val != 0 {
                push_fmt(&mut resp, format_args!(",c={}", params.c_val));
            }
            if params.has_cap_x && params.cap_x == 1 {
                resp.push_str(",X=1");
            }
            if params.has_cap_y && params.cap_y != 0 {
                push_fmt(&mut resp, format_args!(",Y={}", params.cap_y));
            }
        }
        if target == b'c' && params.has_cap_c && params.cap_c == 1 {
            resp.push_str(",C=1");
        }
    }

    resp.push_str(";OK\x1b\\");
    outgoing.extend_from_slice(resp.as_bytes());
}

#[cfg(not(target_arch = "wasm32"))]
fn path_has_sensitive_prefix(raw: &str) -> bool {
    let trimmed = raw.trim_end_matches('/');
    if trimmed == "/proc"
        || trimmed.starts_with("/proc/")
        || trimmed == "/sys"
        || trimmed.starts_with("/sys/")
    {
        return true;
    }
    if trimmed == "/dev" {
        return true;
    }
    if trimmed.starts_with("/dev/") && trimmed != "/dev/shm" && !trimmed.starts_with("/dev/shm/") {
        return true;
    }
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn is_safe_temp_graphics_path(canon: &std::path::Path) -> bool {
    if !canon.to_string_lossy().contains("tty-graphics-protocol") {
        return false;
    }
    let mut temp_roots: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::fs::canonicalize(std::env::temp_dir()) {
        temp_roots.push(p);
    }
    for static_root in ["/tmp", "/var/tmp", "/dev/shm"] {
        if let Ok(p) = std::fs::canonicalize(static_root) {
            temp_roots.push(p);
        }
    }
    temp_roots.iter().any(|root| canon.starts_with(root))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_sliced_file(
    file: &mut std::fs::File,
    offset: u64,
    size: u64,
    max_read_bytes: usize,
) -> Result<Vec<u8>, &'static str> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let meta = file
        .metadata()
        .map_err(|_| "EBADF:Failed to read image file")?;
    if !meta.is_file() {
        return Err("EBADF:Failed to read image file");
    }
    let file_len = meta.len();
    if offset > file_len {
        return Err("EBADF:Failed to read image file");
    }
    let avail = file_len - offset;
    let to_read = if size == 0 {
        avail
    } else if size > avail {
        return Err("EBADF:Failed to read image file");
    } else {
        size
    };
    let to_read_usize = usize::try_from(to_read).map_err(|_| "EBADF:Failed to read image file")?;
    if to_read_usize > max_read_bytes {
        return Err("EBADF:Failed to read image file");
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "EBADF:Failed to read image file")?;
    let mut buf = vec![0u8; to_read_usize];
    file.read_exact(&mut buf)
        .map_err(|_| "EBADF:Failed to read image file")?;
    Ok(buf)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file_medium(
    raw_path_bytes: &[u8],
    is_temp: bool,
    offset: u64,
    size: u64,
    policy: &KittyTransportPolicy<'_>,
) -> Result<Vec<u8>, &'static str> {
    match policy.file_policy {
        KittyFileMediumPolicy::Disabled => return Err("EBADF:Failed to read image file"),
        KittyFileMediumPolicy::TempOnly if !is_temp => {
            return Err("EBADF:Failed to read image file");
        }
        KittyFileMediumPolicy::TempOnly | KittyFileMediumPolicy::LocalFiles => {}
    }

    if usize::try_from(size)
        .ok()
        .is_none_or(|s| s > policy.max_read_bytes)
    {
        return Err("EBADF:Failed to read image file");
    }
    if raw_path_bytes.is_empty() || raw_path_bytes.contains(&0) {
        return Err("EBADF:Failed to read image file");
    }
    let path_str =
        std::str::from_utf8(raw_path_bytes).map_err(|_| "EBADF:Failed to read image file")?;
    if path_has_sensitive_prefix(path_str) {
        return Err("EBADF:Failed to read image file");
    }

    let canon = std::fs::canonicalize(path_str).map_err(|_| "EBADF:Failed to read image file")?;
    let canon_str = canon.to_string_lossy();
    if path_has_sensitive_prefix(&canon_str) {
        return Err("EBADF:Failed to read image file");
    }
    let is_safe_temp = is_safe_temp_graphics_path(&canon);
    if is_temp && policy.file_policy == KittyFileMediumPolicy::TempOnly && !is_safe_temp {
        return Err("EBADF:Failed to read image file");
    }

    // Inspect metadata before opening so FIFOs/sockets/devices cannot block `File::open`.
    let pre_meta = std::fs::metadata(&canon).map_err(|_| "EBADF:Failed to read image file")?;
    if !pre_meta.is_file() {
        return Err("EBADF:Failed to read image file");
    }

    let mut file = std::fs::File::open(&canon).map_err(|_| "EBADF:Failed to read image file")?;
    let bytes = read_sliced_file(&mut file, offset, size, policy.max_read_bytes)?;
    drop(file);

    if is_temp && is_safe_temp && std::fs::remove_file(&canon).is_err() {
        // Best-effort cleanup of temp graphics file; read bytes remain valid.
    }
    Ok(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_shm_medium(
    raw_name_bytes: &[u8],
    offset: u64,
    size: u64,
    policy: &KittyTransportPolicy<'_>,
) -> Result<Vec<u8>, &'static str> {
    if policy.file_policy != KittyFileMediumPolicy::LocalFiles {
        return Err("EBADF:Failed to read image file");
    }
    if usize::try_from(size)
        .ok()
        .is_none_or(|s| s > policy.max_read_bytes)
    {
        return Err("EBADF:Failed to read image file");
    }
    if raw_name_bytes.is_empty() || raw_name_bytes.len() > 255 || raw_name_bytes.contains(&0) {
        return Err("EBADF:Failed to read image file");
    }
    let name =
        std::str::from_utf8(raw_name_bytes).map_err(|_| "EBADF:Failed to read image file")?;
    if name.split('/').any(|seg| seg == "..") {
        return Err("EBADF:Failed to read image file");
    }

    if let Some(reader) = policy.shm_reader {
        return reader.read_and_unlink(name, offset, size, policy.max_read_bytes);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let clean = name.strip_prefix('/').unwrap_or(name);
        if clean.is_empty() || clean.contains('/') {
            return Err("EBADF:Failed to read image file");
        }
        let shm_path = std::path::Path::new("/dev/shm").join(clean);
        let meta =
            std::fs::symlink_metadata(&shm_path).map_err(|_| "EBADF:Failed to read image file")?;
        if !meta.is_file() {
            return Err("EBADF:Failed to read image file");
        }
        let mut file =
            std::fs::File::open(&shm_path).map_err(|_| "EBADF:Failed to read image file")?;
        let bytes = read_sliced_file(&mut file, offset, size, policy.max_read_bytes)?;
        drop(file);
        if std::fs::remove_file(&shm_path).is_err() {
            // Best-effort shm unlink after read.
        }
        Ok(bytes)
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _unused = (offset, size);
        Err("EBADF:shared memory unsupported")
    }
}

#[cfg(target_arch = "wasm32")]
fn read_file_medium(
    _raw_path_bytes: &[u8],
    _is_temp: bool,
    _offset: u64,
    _size: u64,
    _policy: &KittyTransportPolicy<'_>,
) -> Result<Vec<u8>, &'static str> {
    Err("EBADF:file transmission unsupported on wasm32")
}

#[cfg(target_arch = "wasm32")]
fn read_shm_medium(
    _raw_name_bytes: &[u8],
    _offset: u64,
    _size: u64,
    _policy: &KittyTransportPolicy<'_>,
) -> Result<Vec<u8>, &'static str> {
    Err("EBADF:shared memory unsupported on wasm32")
}

fn read_medium_bytes(
    raw_b64: &[u8],
    medium: u8,
    offset: u64,
    size: u64,
    policy: &KittyTransportPolicy<'_>,
) -> Result<Vec<u8>, &'static str> {
    let decoded = decode_base64(raw_b64).ok_or("EINVAL:invalid base64")?;
    match medium {
        b'd' => Ok(decoded),
        b'f' => read_file_medium(&decoded, false, offset, size, policy),
        b't' => read_file_medium(&decoded, true, offset, size, policy),
        b's' => read_shm_medium(&decoded, offset, size, policy),
        _ => Err("EINVAL:unsupported transmission medium"),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "decodes raw medium payload through optional zlib decompression and format conversion"
)]
fn decode_image_payload(
    raw_b64: &[u8],
    medium: u8,
    file_offset: u64,
    file_size: u64,
    format: u32,
    compressed: bool,
    width: u32,
    height: u32,
    max_budget: usize,
    policy: &KittyTransportPolicy<'_>,
) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let raw_bytes = read_medium_bytes(raw_b64, medium, file_offset, file_size, policy)?;
    let inflate_cap = max_budget.max(policy.max_read_bytes);
    let payload = if compressed {
        inflate_zlib(&raw_bytes, inflate_cap).map_err(|_| "EINVAL:zlib inflate failed")?
    } else {
        raw_bytes
    };

    match format {
        100 => {
            let png = decode_png(&payload, max_budget).map_err(|_| "EINVAL:png decode failed")?;
            Ok((png.width, png.height, png.rgba))
        }
        24 => {
            if width == 0
                || height == 0
                || width > MAX_IMAGE_DIMENSION
                || height > MAX_IMAGE_DIMENSION
            {
                return Err("EINVAL:invalid dimensions");
            }
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(3))
                .ok_or("EINVAL:dimension overflow")?;
            if payload.len() < expected {
                return Err("EINVAL:truncated rgb payload");
            }
            let rgba_len = (width as usize) * (height as usize) * 4;
            if rgba_len > max_budget {
                return Err("ENOMEM:image exceeds budget");
            }
            let mut rgba = Vec::with_capacity(rgba_len);
            for rgb in payload[..expected].as_chunks::<3>().0 {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Ok((width, height, rgba))
        }
        32 => {
            if width == 0
                || height == 0
                || width > MAX_IMAGE_DIMENSION
                || height > MAX_IMAGE_DIMENSION
            {
                return Err("EINVAL:invalid dimensions");
            }
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or("EINVAL:dimension overflow")?;
            if payload.len() < expected {
                return Err("EINVAL:truncated rgba payload");
            }
            if expected > max_budget {
                return Err("ENOMEM:image exceeds budget");
            }
            Ok((width, height, payload[..expected].to_vec()))
        }
        _ => Err("EINVAL:unsupported format"),
    }
}

fn compute_cursor_advance(
    cols: u16,
    rows: u16,
    img_w: u32,
    img_h: u32,
    cell_px: (u16, u16),
    no_cursor_move: bool,
) -> (bool, u16, u16) {
    if no_cursor_move {
        return (false, 0, 0);
    }
    let cw = u32::from(cell_px.0.max(1));
    let ch = u32::from(cell_px.1.max(1));
    let eff_cols = if cols > 0 {
        cols
    } else {
        u16::try_from(img_w.div_ceil(cw)).unwrap_or(u16::MAX)
    };
    let eff_rows = if rows > 0 {
        rows
    } else {
        u16::try_from(img_h.div_ceil(ch)).unwrap_or(u16::MAX)
    };
    (true, eff_cols, eff_rows)
}

fn resolve_target_image_id(store: &ImageStore, image_id: u32, image_number: u32) -> Option<u32> {
    if image_id != 0 {
        Some(image_id)
    } else if image_number != 0 {
        store.resolve_image_number(image_number)
    } else {
        None
    }
}

fn validate_relative_placement(
    base_slice: &[ImagePlacement],
    image_id: u32,
    placement_id: u32,
    is_virtual: bool,
    parent_image_id: u32,
    parent_placement_id: u32,
) -> Result<(), &'static str> {
    if parent_image_id == 0 {
        return Ok(());
    }
    if is_virtual {
        return Err("EINVAL:virtual placement cannot be relative");
    }
    if placement_id != 0 && parent_image_id == image_id && parent_placement_id == placement_id {
        if base_slice
            .iter()
            .any(|p| p.image_id == image_id && p.placement_id == placement_id)
        {
            return Err("ECYCLE:relative placement cycle");
        }
        return Err("ENOPARENT:parent placement not found");
    }
    if !base_slice.iter().any(|p| p.image_id == parent_image_id) {
        return Err("ENOPARENT:parent placement not found");
    }

    let candidate = ImagePlacement {
        image_id,
        placement_id,
        abs_line: AbsLine(0),
        col: 0,
        cols: 0,
        rows: 0,
        src_x: 0,
        src_y: 0,
        src_w: 0,
        src_h: 0,
        z_index: 0,
        is_virtual,
        sub_x: 0,
        sub_y: 0,
        parent_image_id,
        parent_placement_id,
        offset_cols: 0,
        offset_rows: 0,
    };

    let mut sim = Vec::with_capacity(base_slice.len() + 1);
    sim.extend_from_slice(base_slice);
    let new_idx = if placement_id != 0
        && let Some(pos) = sim
            .iter()
            .position(|p| p.image_id == image_id && p.placement_id == placement_id)
    {
        sim[pos] = candidate;
        pos
    } else {
        sim.push(candidate);
        sim.len() - 1
    };

    // Walk from the candidate itself first so direct ENOPARENT / ECYCLE / ETOODEEP errors
    // report accurately for the newly requested placement.
    let mut visited = [0usize; MAX_RELATIVE_PLACEMENT_DEPTH + 2];
    visited[0] = new_idx;
    let mut cur_idx = new_idx;
    for depth in 1..=MAX_RELATIVE_PLACEMENT_DEPTH {
        let cur = sim[cur_idx];
        let Some(p_idx) = ImageStore::find_parent_index_in_slice(
            &sim,
            Some(cur_idx),
            cur.parent_image_id,
            cur.parent_placement_id,
        ) else {
            return Err("ENOPARENT:parent placement not found");
        };
        if visited[..depth].contains(&p_idx) {
            return Err("ECYCLE:relative placement cycle");
        }
        if sim[p_idx].parent_image_id == 0 {
            break;
        }
        if depth == MAX_RELATIVE_PLACEMENT_DEPTH {
            return Err("ETOODEEP:relative placement chain too deep");
        }
        visited[depth] = p_idx;
        cur_idx = p_idx;
    }

    // Adding a `placement_id == 0` placement or replacing an existing parent can dynamically
    // re-bind existing `Q=0` (or `Q=placement_id`) dependents into a cycle or excess depth.
    if sim
        .iter()
        .enumerate()
        .any(|(idx, p)| idx != new_idx && p.parent_image_id == image_id)
    {
        let mut state = vec![0u8; sim.len()];
        let mut depths = vec![0u8; sim.len()];
        let mut path = Vec::with_capacity(MAX_RELATIVE_PLACEMENT_DEPTH + 1);
        for start in 0..sim.len() {
            if sim[start].parent_image_id == 0 || state[start] == 2 {
                continue;
            }
            path.clear();
            let mut cur = start;
            let base_depth: usize = loop {
                if sim[cur].parent_image_id == 0 {
                    break 0;
                }
                if state[cur] == 2 {
                    break usize::from(depths[cur]);
                }
                if state[cur] == 1 {
                    return Err("ECYCLE:relative placement cycle");
                }
                state[cur] = 1;
                path.push(cur);
                let Some(p_idx) = ImageStore::find_parent_index_in_slice(
                    &sim,
                    Some(cur),
                    sim[cur].parent_image_id,
                    sim[cur].parent_placement_id,
                ) else {
                    break 0;
                };
                cur = p_idx;
            };
            for (offset, &node) in path.iter().rev().enumerate() {
                let d = base_depth + offset + 1;
                if d > MAX_RELATIVE_PLACEMENT_DEPTH {
                    return Err("ETOODEEP:relative placement chain too deep");
                }
                state[node] = 2;
                depths[node] = u8::try_from(d).unwrap_or(u8::MAX);
            }
        }
    }

    Ok(())
}

fn alpha_blend_pixel(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = u32::from(src[3]);
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let da = u32::from(dst[3]);
    let inv_sa = 255 - sa;
    let out_a_scaled = sa * 255 + da * inv_sa;
    if out_a_scaled == 0 {
        return [0, 0, 0, 0];
    }
    let out_a = u8::try_from(((out_a_scaled + 127) / 255).min(255)).unwrap_or(255);
    let blend_ch = |sc: u8, dc: u8| -> u8 {
        let num = u32::from(sc) * sa * 255 + u32::from(dc) * da * inv_sa + out_a_scaled / 2;
        u8::try_from((num / out_a_scaled).min(255)).unwrap_or(255)
    };
    [
        blend_ch(src[0], dst[0]),
        blend_ch(src[1], dst[1]),
        blend_ch(src[2], dst[2]),
        out_a,
    ]
}

#[derive(Clone, Copy)]
struct BlitRect {
    src_x: u32,
    src_y: u32,
    dst_x: u32,
    dst_y: u32,
    w: u32,
    h: u32,
    overwrite: bool,
}

fn blit_rect(dst: &mut [u8], dst_canvas_w: u32, src: &[u8], src_canvas_w: u32, rect: BlitRect) {
    for row in 0..rect.h {
        let sy = (rect.src_y + row) as usize;
        let dy = (rect.dst_y + row) as usize;
        for col in 0..rect.w {
            let sx = (rect.src_x + col) as usize;
            let dx = (rect.dst_x + col) as usize;
            let s_off = (sy * src_canvas_w as usize + sx) * 4;
            let d_off = (dy * dst_canvas_w as usize + dx) * 4;
            let s_px = [src[s_off], src[s_off + 1], src[s_off + 2], src[s_off + 3]];
            let out_px = if rect.overwrite {
                s_px
            } else {
                let d_px = [dst[d_off], dst[d_off + 1], dst[d_off + 2], dst[d_off + 3]];
                alpha_blend_pixel(d_px, s_px)
            };
            dst[d_off..d_off + 4].copy_from_slice(&out_px);
        }
    }
}

/// Processes a Kitty Graphics Protocol `APC` payload (`G...`) with explicit viewport and
/// transport policy context.
pub fn handle_kitty_apc_with_context(
    store: &mut ImageStore,
    data: &[u8],
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    if !data.starts_with(b"G") {
        return KittyOutcome::default();
    }
    store.set_cell_pixel_size(ctx.cell_px.0, ctx.cell_px.1);
    let rest = &data[1..];
    let (header, payload_b64) = match rest.iter().position(|&b| b == b';') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, &[][..]),
    };

    let params = parse_header(header);
    if params.image_id != 0 && params.image_number != 0 {
        write_response(
            outgoing,
            params.image_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("EINVAL:cannot specify both image id and image number"),
        );
        return KittyOutcome::default();
    }

    if let Some(outcome) = try_handle_chunk_continuation(store, &params, payload_b64, ctx, outgoing)
    {
        return outcome;
    }

    let action = params.action.unwrap_or(KittyAction::Transmit);
    match action {
        KittyAction::Query => {
            if params.feature_query.is_some() {
                execute_feature_query(&params, &ctx.policy, outgoing);
            } else if params.u_val == 1
                && payload_b64.is_empty()
                && !params.has_width
                && !params.has_height
            {
                write_response(
                    outgoing,
                    params.image_id,
                    params.image_number,
                    params.placement_id,
                    params.quiet,
                    Ok(()),
                );
            } else {
                let status = decode_image_payload(
                    payload_b64,
                    params.medium,
                    params.file_offset,
                    params.file_size,
                    params.format,
                    params.compressed,
                    params.width,
                    params.height,
                    store.memory_budget(),
                    &ctx.policy,
                )
                .map(|_| ());
                write_response(
                    outgoing,
                    params.image_id,
                    params.image_number,
                    params.placement_id,
                    params.quiet,
                    status,
                );
            }
            KittyOutcome::default()
        }
        KittyAction::Delete => {
            execute_delete(
                store,
                &params,
                ctx.screen,
                ctx.viewport_top_abs_line,
                ctx.viewport_rows,
                ctx.cursor_abs_line,
                ctx.cursor_col,
            );
            KittyOutcome {
                modified: true,
                ..KittyOutcome::default()
            }
        }
        KittyAction::Put => execute_put(store, &params, ctx, outgoing),
        KittyAction::Animation => execute_animation_control(store, &params, outgoing),
        KittyAction::Compose => execute_compose(store, &params, outgoing),
        KittyAction::Transmit | KittyAction::TransmitAndDisplay | KittyAction::Frame => {
            start_or_finish_upload(store, &params, action, payload_b64, ctx, outgoing)
        }
    }
}

fn find_continuation_key(store: &ImageStore, params: &KittyParams) -> Option<PendingChunkKey> {
    if params.image_id != 0 {
        let key = PendingChunkKey::ImageId(params.image_id);
        return (store.pending_kitty.contains_key(&key) && params.is_continuation_header())
            .then_some(key);
    }
    if params.image_number != 0 {
        let key = PendingChunkKey::ImageNumber(params.image_number);
        return (store.pending_kitty.contains_key(&key) && params.is_continuation_header())
            .then_some(key);
    }
    if !params.is_continuation_header() {
        return None;
    }
    if store
        .pending_kitty
        .contains_key(&PendingChunkKey::Anonymous)
    {
        return Some(PendingChunkKey::Anonymous);
    }
    store
        .last_pending_key
        .filter(|k| store.pending_kitty.contains_key(k))
}

fn remove_pending_entry(
    store: &mut ImageStore,
    key: PendingChunkKey,
) -> Option<PendingKittyUpload> {
    let entry = store.pending_kitty.remove(&key)?;
    store.pending_kitty_bytes = store
        .pending_kitty_bytes
        .saturating_sub(entry.payload.len());
    if store.last_pending_key == Some(key) {
        store.last_pending_key = store.pending_kitty.keys().next_back().copied();
    }
    Some(entry)
}

fn try_handle_chunk_continuation(
    store: &mut ImageStore,
    params: &KittyParams,
    payload_b64: &[u8],
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> Option<KittyOutcome> {
    if let Some(act) = params.action
        && !matches!(act, KittyAction::Frame)
    {
        return None;
    }
    let key = find_continuation_key(store, params)?;
    let pending = store.pending_kitty.get(&key)?;
    if params.action == Some(KittyAction::Frame) && pending.action != KittyAction::Frame {
        return None;
    }

    if store.pending_kitty_bytes.saturating_add(payload_b64.len()) > MAX_KITTY_PENDING_BYTES {
        if let Some(evicted) = remove_pending_entry(store, key) {
            write_response(
                outgoing,
                evicted.image_id,
                evicted.image_number,
                evicted.placement_id,
                evicted.quiet,
                Err("ENOMEM:chunked payload too large"),
            );
        }
        return Some(KittyOutcome::default());
    }

    if let Some(pending_mut) = store.pending_kitty.get_mut(&key) {
        pending_mut.payload.extend_from_slice(payload_b64);
        store.pending_kitty_bytes = store.pending_kitty_bytes.saturating_add(payload_b64.len());
        store.last_pending_key = Some(key);
    }

    if params.more {
        return Some(KittyOutcome::default());
    }

    let finished = remove_pending_entry(store, key)?;
    Some(dispatch_finished_upload(store, finished, ctx, outgoing))
}

fn start_or_finish_upload(
    store: &mut ImageStore,
    params: &KittyParams,
    action: KittyAction,
    payload_b64: &[u8],
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let pending = PendingKittyUpload {
        image_id: params.image_id,
        image_number: params.image_number,
        placement_id: params.placement_id,
        action,
        format: params.format,
        medium: params.medium,
        compressed: params.compressed,
        width: params.width,
        height: params.height,
        file_size: params.file_size,
        file_offset: params.file_offset,
        cols: u16::try_from(params.c_val).unwrap_or(u16::MAX),
        rows: u16::try_from(params.r_val).unwrap_or(u16::MAX),
        src_x: params.src_x,
        src_y: params.src_y,
        src_w: params.src_w,
        src_h: params.src_h,
        cap_x: params.cap_x,
        cap_y: params.cap_y,
        c_param: params.c_val,
        r_param: params.r_val,
        z_index: params.z_val,
        no_cursor_move: params.cap_c == 1,
        is_virtual: params.u_val == 1,
        parent_image_id: params.parent_image_id,
        parent_placement_id: params.parent_placement_id,
        offset_cols: params.offset_cols,
        offset_rows: params.offset_rows,
        quiet: params.quiet,
        payload: payload_b64.to_vec(),
    };

    let key = if params.image_id != 0 {
        PendingChunkKey::ImageId(params.image_id)
    } else if params.image_number != 0 {
        PendingChunkKey::ImageNumber(params.image_number)
    } else {
        PendingChunkKey::Anonymous
    };

    if params.more {
        // Evict any abandoned incomplete transfer under the exact same key before starting fresh.
        drop(remove_pending_entry(store, key));
        if store.pending_kitty.len() >= MAX_KITTY_PENDING_TRANSFERS {
            write_response(
                outgoing,
                pending.image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err("ENOMEM:too many concurrent chunked transfers"),
            );
            return KittyOutcome::default();
        }
        if store
            .pending_kitty_bytes
            .saturating_add(pending.payload.len())
            > MAX_KITTY_PENDING_BYTES
        {
            write_response(
                outgoing,
                pending.image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err("ENOMEM:chunked payload too large"),
            );
            return KittyOutcome::default();
        }
        store.pending_kitty_bytes = store
            .pending_kitty_bytes
            .saturating_add(pending.payload.len());
        store.pending_kitty.insert(key, pending);
        store.last_pending_key = Some(key);
        return KittyOutcome::default();
    }

    dispatch_finished_upload(store, pending, ctx, outgoing)
}

fn dispatch_finished_upload(
    store: &mut ImageStore,
    pending: PendingKittyUpload,
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    match pending.action {
        KittyAction::Frame => finish_frame(store, pending, &ctx.policy, outgoing),
        _ => finish_transmit(store, pending, ctx, outgoing),
    }
}

fn execute_put(
    store: &mut ImageStore,
    params: &KittyParams,
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let Some(img_id) = resolve_target_image_id(store, params.image_id, params.image_number) else {
        write_response(
            outgoing,
            params.image_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };
    let Some((img_w, img_h)) = store.get_image(img_id).map(|img| (img.width, img.height)) else {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };

    let is_virtual = params.u_val == 1;
    if let Err(err) = validate_relative_placement(
        store.placements(ctx.screen),
        img_id,
        params.placement_id,
        is_virtual,
        params.parent_image_id,
        params.parent_placement_id,
    ) {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err(err),
        );
        return KittyOutcome::default();
    }

    let cols = u16::try_from(params.c_val).unwrap_or(u16::MAX);
    let rows = u16::try_from(params.r_val).unwrap_or(u16::MAX);
    let placement = ImagePlacement {
        image_id: img_id,
        placement_id: params.placement_id,
        abs_line: ctx.cursor_abs_line,
        col: ctx.cursor_col,
        cols,
        rows,
        src_x: params.src_x,
        src_y: params.src_y,
        src_w: params.src_w,
        src_h: params.src_h,
        z_index: params.z_val,
        is_virtual,
        sub_x: u16::try_from(params.cap_x).unwrap_or(u16::MAX),
        sub_y: u16::try_from(params.cap_y).unwrap_or(u16::MAX),
        parent_image_id: params.parent_image_id,
        parent_placement_id: params.parent_placement_id,
        offset_cols: params.offset_cols,
        offset_rows: params.offset_rows,
    };
    store.add_placement(ctx.screen, placement);
    write_response(
        outgoing,
        img_id,
        params.image_number,
        params.placement_id,
        params.quiet,
        Ok(()),
    );
    let suppress_cursor = params.cap_c == 1 || is_virtual || params.parent_image_id != 0;
    let (move_cursor, advance_cols, advance_rows) =
        compute_cursor_advance(cols, rows, img_w, img_h, ctx.cell_px, suppress_cursor);
    KittyOutcome {
        modified: true,
        move_cursor,
        advance_cols,
        advance_rows,
    }
}

fn finish_transmit(
    store: &mut ImageStore,
    pending: PendingKittyUpload,
    ctx: KittyApcContext<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let decoded = decode_image_payload(
        &pending.payload,
        pending.medium,
        pending.file_offset,
        pending.file_size,
        pending.format,
        pending.compressed,
        pending.width,
        pending.height,
        store.memory_budget(),
        &ctx.policy,
    );

    let (w, h, rgba) = match decoded {
        Ok(v) => v,
        Err(msg) => {
            write_response(
                outgoing,
                pending.image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err(msg),
            );
            return KittyOutcome::default();
        }
    };

    let image_id = if pending.image_id != 0 {
        pending.image_id
    } else {
        store.allocate_id()
    };

    let replacing_existing = pending.image_id != 0 && store.get_image(image_id).is_some();
    if pending.action == KittyAction::TransmitAndDisplay && pending.parent_image_id != 0 {
        let post_buf: Vec<ImagePlacement>;
        let validation_slice = if replacing_existing
            && store
                .placements(ctx.screen)
                .iter()
                .any(|p| p.image_id == image_id)
        {
            let current = store.placements(ctx.screen);
            let mut next = Vec::with_capacity(current.len());
            let mut removed_keys = Vec::new();
            for &p in current {
                if p.image_id == image_id {
                    removed_keys.push((p.image_id, p.placement_id));
                } else {
                    next.push(p);
                }
            }
            ImageStore::cascade_delete_children_in_vec(&mut next, &removed_keys);
            post_buf = next;
            &post_buf[..]
        } else {
            store.placements(ctx.screen)
        };
        if let Err(err) = validate_relative_placement(
            validation_slice,
            image_id,
            pending.placement_id,
            pending.is_virtual,
            pending.parent_image_id,
            pending.parent_placement_id,
        ) {
            write_response(
                outgoing,
                image_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err(err),
            );
            return KittyOutcome::default();
        }
    }

    // Per Kitty protocol: replacing an existing `image_id` deletes its old placements.
    if replacing_existing {
        store.delete_placements_where(ScreenKind::Primary, false, |p| p.image_id == image_id);
        store.delete_placements_where(ScreenKind::Alternate, false, |p| p.image_id == image_id);
    }

    let stored = StoredImage {
        id: image_id,
        width: w,
        height: h,
        format: ImageFormat::Rgba8,
        rgba,
        revision: 0,
    };

    if !store.insert_image(stored) {
        write_response(
            outgoing,
            image_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOMEM:image exceeds memory budget"),
        );
        return KittyOutcome::default();
    }

    if pending.image_number != 0 {
        store.register_image_number(pending.image_number, image_id);
    }

    let mut outcome = KittyOutcome {
        modified: true,
        ..KittyOutcome::default()
    };

    if pending.action == KittyAction::TransmitAndDisplay {
        let placement = ImagePlacement {
            image_id,
            placement_id: pending.placement_id,
            abs_line: ctx.cursor_abs_line,
            col: ctx.cursor_col,
            cols: pending.cols,
            rows: pending.rows,
            src_x: pending.src_x,
            src_y: pending.src_y,
            src_w: pending.src_w,
            src_h: pending.src_h,
            z_index: pending.z_index,
            is_virtual: pending.is_virtual,
            sub_x: u16::try_from(pending.cap_x).unwrap_or(u16::MAX),
            sub_y: u16::try_from(pending.cap_y).unwrap_or(u16::MAX),
            parent_image_id: pending.parent_image_id,
            parent_placement_id: pending.parent_placement_id,
            offset_cols: pending.offset_cols,
            offset_rows: pending.offset_rows,
        };
        store.add_placement(ctx.screen, placement);
        let suppress_cursor =
            pending.no_cursor_move || pending.is_virtual || pending.parent_image_id != 0;
        let (move_cursor, advance_cols, advance_rows) = compute_cursor_advance(
            pending.cols,
            pending.rows,
            w,
            h,
            ctx.cell_px,
            suppress_cursor,
        );
        outcome.move_cursor = move_cursor;
        outcome.advance_cols = advance_cols;
        outcome.advance_rows = advance_rows;
    }

    write_response(
        outgoing,
        image_id,
        pending.image_number,
        pending.placement_id,
        pending.quiet,
        Ok(()),
    );
    outcome
}

fn finish_frame(
    store: &mut ImageStore,
    pending: PendingKittyUpload,
    policy: &KittyTransportPolicy<'_>,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let Some(img_id) = resolve_target_image_id(store, pending.image_id, pending.image_number)
    else {
        write_response(
            outgoing,
            pending.image_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };
    let Some((img_w, img_h)) = store.get_image(img_id).map(|img| (img.width, img.height)) else {
        write_response(
            outgoing,
            img_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };

    let frame_count = store.animation_frame_count(img_id);
    if pending.r_param != 0
        && usize::try_from(pending.r_param - 1).unwrap_or(usize::MAX) >= frame_count
    {
        write_response(
            outgoing,
            img_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOENT:target frame not found"),
        );
        return KittyOutcome::default();
    }
    if pending.c_param != 0
        && usize::try_from(pending.c_param - 1).unwrap_or(usize::MAX) >= frame_count
    {
        write_response(
            outgoing,
            img_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("ENOENT:base frame not found"),
        );
        return KittyOutcome::default();
    }

    let (in_w, in_h, in_rgba) =
        if pending.payload.is_empty() && pending.width == 0 && pending.height == 0 {
            (0, 0, Vec::new())
        } else {
            match decode_image_payload(
                &pending.payload,
                pending.medium,
                pending.file_offset,
                pending.file_size,
                pending.format,
                pending.compressed,
                pending.width,
                pending.height,
                store.memory_budget(),
                policy,
            ) {
                Ok(v) => v,
                Err(msg) => {
                    write_response(
                        outgoing,
                        img_id,
                        pending.image_number,
                        pending.placement_id,
                        pending.quiet,
                        Err(msg),
                    );
                    return KittyOutcome::default();
                }
            }
        };

    if pending
        .src_x
        .checked_add(in_w)
        .is_none_or(|end_x| end_x > img_w)
        || pending
            .src_y
            .checked_add(in_h)
            .is_none_or(|end_y| end_y > img_h)
    {
        write_response(
            outgoing,
            img_id,
            pending.image_number,
            pending.placement_id,
            pending.quiet,
            Err("EINVAL:frame rect out of bounds"),
        );
        return KittyOutcome::default();
    }

    let overwrite = pending.cap_x == 1;
    let gap_ms = if pending.z_index == 0 {
        40
    } else {
        pending.z_index
    };
    let mut active_modified = false;

    if pending.r_param != 0 {
        let target_idx = (pending.r_param - 1) as usize;
        if store.ensure_animation_entry(img_id).is_none() {
            write_response(
                outgoing,
                img_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err("ENOENT:image not found"),
            );
            return KittyOutcome::default();
        }
        if let Some((_, is_active)) = store.with_frame_rgba_mut(img_id, target_idx, |buf| {
            blit_rect(
                buf,
                img_w,
                &in_rgba,
                in_w,
                BlitRect {
                    src_x: 0,
                    src_y: 0,
                    dst_x: pending.src_x,
                    dst_y: pending.src_y,
                    w: in_w,
                    h: in_h,
                    overwrite,
                },
            );
        }) {
            active_modified = is_active;
        }
        if pending.z_index != 0
            && let Err(err) =
                store.set_animation_params(img_id, None, 0, Some((target_idx, pending.z_index)))
        {
            write_response(
                outgoing,
                img_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err(err),
            );
            return KittyOutcome::default();
        }
    } else {
        let mut canvas = if pending.c_param != 0 {
            let Some(copied) = store.copy_frame_rgba(img_id, (pending.c_param - 1) as usize) else {
                write_response(
                    outgoing,
                    img_id,
                    pending.image_number,
                    pending.placement_id,
                    pending.quiet,
                    Err("ENOENT:base frame not found"),
                );
                return KittyOutcome::default();
            };
            copied
        } else {
            let fill = [
                ((pending.cap_y >> 24) & 0xFF) as u8,
                ((pending.cap_y >> 16) & 0xFF) as u8,
                ((pending.cap_y >> 8) & 0xFF) as u8,
                (pending.cap_y & 0xFF) as u8,
            ];
            let px_count = (img_w as usize).saturating_mul(img_h as usize);
            let mut buf = Vec::with_capacity(px_count.saturating_mul(4));
            for _ in 0..px_count {
                buf.extend_from_slice(&fill);
            }
            buf
        };
        blit_rect(
            &mut canvas,
            img_w,
            &in_rgba,
            in_w,
            BlitRect {
                src_x: 0,
                src_y: 0,
                dst_x: pending.src_x,
                dst_y: pending.src_y,
                w: in_w,
                h: in_h,
                overwrite,
            },
        );
        if let Err(err) = store.append_animation_frame(img_id, canvas, gap_ms) {
            write_response(
                outgoing,
                img_id,
                pending.image_number,
                pending.placement_id,
                pending.quiet,
                Err(err),
            );
            return KittyOutcome::default();
        }
    }

    write_response(
        outgoing,
        img_id,
        pending.image_number,
        pending.placement_id,
        pending.quiet,
        Ok(()),
    );
    KittyOutcome {
        modified: active_modified,
        ..KittyOutcome::default()
    }
}

fn execute_animation_control(
    store: &mut ImageStore,
    params: &KittyParams,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let Some(img_id) = resolve_target_image_id(store, params.image_id, params.image_number) else {
        write_response(
            outgoing,
            params.image_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };
    if store.get_image(img_id).is_none() {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    }

    let frame_count = store.animation_frame_count(img_id);
    if params.r_val != 0 && usize::try_from(params.r_val - 1).unwrap_or(usize::MAX) >= frame_count {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:frame not found"),
        );
        return KittyOutcome::default();
    }
    if params.c_val != 0 && usize::try_from(params.c_val - 1).unwrap_or(usize::MAX) >= frame_count {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:frame not found"),
        );
        return KittyOutcome::default();
    }

    let new_state = match params.width {
        0 => None,
        1 => Some(AnimationPlaybackState::Stopped),
        2 => Some(AnimationPlaybackState::Loading),
        3 => Some(AnimationPlaybackState::Playing),
        _ => {
            write_response(
                outgoing,
                img_id,
                params.image_number,
                params.placement_id,
                params.quiet,
                Err("EINVAL:invalid animation state"),
            );
            return KittyOutcome::default();
        }
    };
    let gap_edit = if params.r_val != 0 && params.has_z {
        Some(((params.r_val - 1) as usize, params.z_val))
    } else {
        None
    };
    if let Err(err) = store.set_animation_params(img_id, new_state, params.height, gap_edit) {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err(err),
        );
        return KittyOutcome::default();
    }

    let mut modified = false;
    if params.c_val != 0 {
        modified = store.set_active_frame(img_id, (params.c_val - 1) as usize);
    }

    write_response(
        outgoing,
        img_id,
        params.image_number,
        params.placement_id,
        params.quiet,
        Ok(()),
    );
    KittyOutcome {
        modified,
        ..KittyOutcome::default()
    }
}

fn execute_compose(
    store: &mut ImageStore,
    params: &KittyParams,
    outgoing: &mut Vec<u8>,
) -> KittyOutcome {
    let Some(img_id) = resolve_target_image_id(store, params.image_id, params.image_number) else {
        write_response(
            outgoing,
            params.image_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };
    let Some((img_w, img_h)) = store.get_image(img_id).map(|img| (img.width, img.height)) else {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:image not found"),
        );
        return KittyOutcome::default();
    };

    let frame_count = store.animation_frame_count(img_id);
    if params.r_val == 0
        || params.c_val == 0
        || usize::try_from(params.r_val - 1).unwrap_or(usize::MAX) >= frame_count
        || usize::try_from(params.c_val - 1).unwrap_or(usize::MAX) >= frame_count
    {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:frame not found"),
        );
        return KittyOutcome::default();
    }

    let src_x = params.cap_x;
    let src_y = params.cap_y;
    let dst_x = params.src_x;
    let dst_y = params.src_y;
    if src_x >= img_w || dst_x >= img_w || src_y >= img_h || dst_y >= img_h {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("EINVAL:compose rect out of bounds"),
        );
        return KittyOutcome::default();
    }
    let w = if params.src_w == 0 {
        img_w - src_x.max(dst_x)
    } else {
        params.src_w
    };
    let h = if params.src_h == 0 {
        img_h - src_y.max(dst_y)
    } else {
        params.src_h
    };
    if src_x.checked_add(w).is_none_or(|e| e > img_w)
        || dst_x.checked_add(w).is_none_or(|e| e > img_w)
        || src_y.checked_add(h).is_none_or(|e| e > img_h)
        || dst_y.checked_add(h).is_none_or(|e| e > img_h)
    {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("EINVAL:compose rect out of bounds"),
        );
        return KittyOutcome::default();
    }

    if params.r_val == params.c_val
        && src_x < dst_x + w
        && dst_x < src_x + w
        && src_y < dst_y + h
        && dst_y < src_y + h
    {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("EINVAL:overlapping compose rects on same frame"),
        );
        return KittyOutcome::default();
    }

    let Some(src_buf) = store.copy_frame_rgba(img_id, (params.r_val - 1) as usize) else {
        write_response(
            outgoing,
            img_id,
            params.image_number,
            params.placement_id,
            params.quiet,
            Err("ENOENT:frame not found"),
        );
        return KittyOutcome::default();
    };
    let overwrite = params.cap_c == 1;
    let dst_idx = (params.c_val - 1) as usize;
    let modified = store
        .with_frame_rgba_mut(img_id, dst_idx, |dst_buf| {
            blit_rect(
                dst_buf,
                img_w,
                &src_buf,
                img_w,
                BlitRect {
                    src_x,
                    src_y,
                    dst_x,
                    dst_y,
                    w,
                    h,
                    overwrite,
                },
            );
        })
        .is_some_and(|(_, is_active)| is_active);

    write_response(
        outgoing,
        img_id,
        params.image_number,
        params.placement_id,
        params.quiet,
        Ok(()),
    );
    KittyOutcome {
        modified,
        ..KittyOutcome::default()
    }
}

fn execute_delete(
    store: &mut ImageStore,
    params: &KittyParams,
    screen: ScreenKind,
    viewport_top_abs_line: AbsLine,
    viewport_rows: u16,
    cursor_abs_line: AbsLine,
    cursor_col: u16,
) {
    let free_images = params.delete_target.is_ascii_uppercase();
    match params.delete_target {
        b'a' | b'A' => {
            store.clear_viewport_placements(
                screen,
                viewport_top_abs_line,
                viewport_rows,
                free_images,
            );
        }
        b'i' | b'I' => {
            let id = params.image_id;
            let pid = params.placement_id;
            if pid == 0
                && let Some(entry) = store.pending_kitty.remove(&PendingChunkKey::ImageId(id))
            {
                store.pending_kitty_bytes = store
                    .pending_kitty_bytes
                    .saturating_sub(entry.payload.len());
            }
            store.delete_placements_where(screen, free_images, |p| {
                p.image_id == id && (pid == 0 || p.placement_id == pid)
            });
            if free_images && pid == 0 && store.get_image(id).is_some() {
                store.gc_unreferenced();
            }
        }
        b'n' | b'N' => {
            if params.placement_id == 0
                && let Some(entry) = store
                    .pending_kitty
                    .remove(&PendingChunkKey::ImageNumber(params.image_number))
            {
                store.pending_kitty_bytes = store
                    .pending_kitty_bytes
                    .saturating_sub(entry.payload.len());
            }
            if let Some(id) = store.resolve_image_number(params.image_number) {
                let pid = params.placement_id;
                store.delete_placements_where(screen, free_images, |p| {
                    p.image_id == id && (pid == 0 || p.placement_id == pid)
                });
            }
        }
        b'c' | b'C' => {
            store.delete_placements_where_with_ctx(
                screen,
                free_images,
                |p, (eff_cols, eff_rows)| {
                    if p.is_virtual {
                        return false;
                    }
                    let end_line = p.abs_line.0.saturating_add(u64::from(eff_rows));
                    let end_col = u32::from(p.col) + u32::from(eff_cols);
                    let cur_col = u32::from(cursor_col);
                    cursor_abs_line.0 >= p.abs_line.0
                        && cursor_abs_line.0 < end_line
                        && cur_col >= u32::from(p.col)
                        && cur_col < end_col
                },
            );
        }
        b'p' | b'P' => {
            let pid = params.placement_id;
            let img_filter = params.image_id;
            if params.src_x == 0 && params.src_y == 0 && pid != 0 {
                store.delete_placements_where(screen, free_images, |p| {
                    p.placement_id == pid && (img_filter == 0 || p.image_id == img_filter)
                });
            } else {
                let target_col = params.src_x.saturating_sub(1);
                let target_line = viewport_top_abs_line
                    .0
                    .saturating_add(u64::from(params.src_y.saturating_sub(1)));
                store.delete_placements_where_with_ctx(
                    screen,
                    free_images,
                    |p, (eff_cols, eff_rows)| {
                        if p.is_virtual {
                            return false;
                        }
                        let end_line = p.abs_line.0.saturating_add(u64::from(eff_rows));
                        let end_col = u32::from(p.col) + u32::from(eff_cols);
                        target_line >= p.abs_line.0
                            && target_line < end_line
                            && target_col >= u32::from(p.col)
                            && target_col < end_col
                            && (pid == 0 || p.placement_id == pid)
                    },
                );
            }
        }
        b'q' | b'Q' => {
            let target_col = params.src_x.saturating_sub(1);
            let target_line = viewport_top_abs_line
                .0
                .saturating_add(u64::from(params.src_y.saturating_sub(1)));
            let z = params.z_val;
            store.delete_placements_where_with_ctx(
                screen,
                free_images,
                |p, (eff_cols, eff_rows)| {
                    if p.is_virtual || p.z_index != z {
                        return false;
                    }
                    let end_line = p.abs_line.0.saturating_add(u64::from(eff_rows));
                    let end_col = u32::from(p.col) + u32::from(eff_cols);
                    target_line >= p.abs_line.0
                        && target_line < end_line
                        && target_col >= u32::from(p.col)
                        && target_col < end_col
                },
            );
        }
        b'r' | b'R' => {
            let min_id = params.src_x;
            let max_id = params.src_y;
            store.delete_placements_where(screen, free_images, |p| {
                p.image_id >= min_id && p.image_id <= max_id
            });
        }
        b'x' | b'X' => {
            let target_col = params.src_x.saturating_sub(1);
            store.delete_placements_where_with_ctx(screen, free_images, |p, (eff_cols, _)| {
                if p.is_virtual {
                    return false;
                }
                let end_col = u32::from(p.col) + u32::from(eff_cols);
                target_col >= u32::from(p.col) && target_col < end_col
            });
        }
        b'y' | b'Y' => {
            let target_line = viewport_top_abs_line
                .0
                .saturating_add(u64::from(params.src_y.saturating_sub(1)));
            store.delete_placements_where_with_ctx(screen, free_images, |p, (_, eff_rows)| {
                if p.is_virtual {
                    return false;
                }
                let end_line = p.abs_line.0.saturating_add(u64::from(eff_rows));
                target_line >= p.abs_line.0 && target_line < end_line
            });
        }
        b'z' | b'Z' => {
            let z = params.z_val;
            store.delete_placements_where(screen, free_images, |p| !p.is_virtual && p.z_index == z);
        }
        b'f' | b'F' => {
            let target_frame = params.r_val;
            if let Some(id) = resolve_target_image_id(store, params.image_id, params.image_number) {
                store.delete_animation_frames(id, target_frame);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellFlags;

    fn encode_b64(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = if chunk.len() > 1 {
                u32::from(chunk[1])
            } else {
                0
            };
            let b2 = if chunk.len() > 2 {
                u32::from(chunk[2])
            } else {
                0
            };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[((triple >> 6) & 0x3F) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(triple & 0x3F) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    fn ctx<'a>(policy: KittyTransportPolicy<'a>) -> KittyApcContext<'a> {
        KittyApcContext {
            screen: ScreenKind::Primary,
            viewport_top_abs_line: AbsLine(100),
            viewport_rows: 24,
            cursor_abs_line: AbsLine(102),
            cursor_col: 3,
            cell_px: (8, 16),
            policy,
        }
    }

    #[test]
    fn concurrent_chunked_transfers_do_not_collide_or_swallow_commands() {
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let c = ctx(KittyTransportPolicy::default());

        let px1 = encode_b64(&[10, 20, 30, 255, 40, 50, 60, 255]);
        let (p1a, p1b) = px1.split_at(4);
        let px2 = encode_b64(&[99, 88, 77, 255]);
        let (p2a, p2b) = px2.split_at(4);

        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=T,i=1,f=32,s=2,v=1,m=1;{p1a}").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=2,f=32,s=1,v=1,m=1;{p2a}").as_bytes(),
            c,
            &mut out,
        );
        // Query and delete must run immediately without being swallowed into chunk buffers.
        let q_px = encode_b64(&[1, 2, 3, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=q,i=3,f=32,s=1,v=1;{q_px}").as_bytes(),
            c,
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b_Gi=3;OK\x1b\\"
        );
        out.clear();

        handle_kitty_apc_with_context(
            &mut store,
            format!("Gi=1,m=0;{p1b}").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(
            &mut store,
            format!("Gi=2,m=0;{p2b}").as_bytes(),
            c,
            &mut out,
        );

        assert_eq!(
            store.get_image(1).unwrap().active_rgba(),
            &[10, 20, 30, 255, 40, 50, 60, 255]
        );
        assert_eq!(
            store.get_image(2).unwrap().active_rgba(),
            &[99, 88, 77, 255]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\"
        );
    }

    #[test]
    fn chunked_transfer_count_and_byte_bounds_enforced() {
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let c = ctx(KittyTransportPolicy::default());

        for id in 1..=16u32 {
            handle_kitty_apc_with_context(
                &mut store,
                format!("Ga=t,i={id},f=32,s=1,v=1,m=1;AA").as_bytes(),
                c,
                &mut out,
            );
        }
        assert!(out.is_empty());
        handle_kitty_apc_with_context(&mut store, b"Ga=t,i=17,f=32,s=1,v=1,m=1;AA", c, &mut out);
        assert!(String::from_utf8(out).unwrap().contains("ENOMEM"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_and_temp_medium_slicing_policy_and_unlink_guard() {
        let dir = std::env::temp_dir();
        let temp_file = dir.join(format!(
            "tty-graphics-protocol-test-{}.bin",
            std::process::id()
        ));
        let regular_file = dir.join(format!("egui-xterm-keep-{}.bin", std::process::id()));
        let payload = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        std::fs::write(&temp_file, payload).unwrap();
        std::fs::write(&regular_file, payload).unwrap();

        let mut store = ImageStore::default();
        let mut out = Vec::new();

        // 1) Disabled policy rejects with EBADF and does not unlink.
        let disabled = KittyTransportPolicy {
            file_policy: KittyFileMediumPolicy::Disabled,
            ..KittyTransportPolicy::default()
        };
        let b64_temp = encode_b64(temp_file.to_string_lossy().as_bytes());
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=1,t=t,f=32,s=1,v=1,O=2,S=4;{b64_temp}").as_bytes(),
            ctx(disabled),
            &mut out,
        );
        assert!(String::from_utf8(out.clone()).unwrap().contains("EBADF"));
        assert!(temp_file.exists());
        out.clear();

        // 2) TempOnly rejects t=f and un-named temp file, allows t=t with substring & S/O slice.
        let temp_only = KittyTransportPolicy {
            file_policy: KittyFileMediumPolicy::TempOnly,
            ..KittyTransportPolicy::default()
        };
        let b64_reg = encode_b64(regular_file.to_string_lossy().as_bytes());
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=2,t=f,f=32,s=1,v=1,O=2,S=4;{b64_reg}").as_bytes(),
            ctx(temp_only),
            &mut out,
        );
        assert!(String::from_utf8(out.clone()).unwrap().contains("EBADF"));
        out.clear();

        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=2,t=t,f=32,s=1,v=1,O=2,S=4;{b64_reg}").as_bytes(),
            ctx(temp_only),
            &mut out,
        );
        assert!(String::from_utf8(out.clone()).unwrap().contains("EBADF"));
        assert!(regular_file.exists());
        out.clear();

        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=2,t=t,f=32,s=1,v=1,O=2,S=4;{b64_temp}").as_bytes(),
            ctx(temp_only),
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b_Gi=2;OK\x1b\\"
        );
        assert_eq!(store.get_image(2).unwrap().active_rgba(), &[3, 4, 5, 6]);
        assert!(!temp_file.exists());
        out.clear();

        // 3) LocalFiles reads t=f with S/O slicing and never deletes regular_file even if t=t lacks marker.
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=3,t=t,f=32,s=1,v=1,O=4,S=4;{b64_reg}").as_bytes(),
            ctx(KittyTransportPolicy::default()),
            &mut out,
        );
        assert_eq!(store.get_image(3).unwrap().active_rgba(), &[5, 6, 7, 8]);
        assert!(regular_file.exists());
        std::fs::remove_file(&regular_file).unwrap();
    }

    #[test]
    fn shm_host_callback_works() {
        struct MockShm;
        impl KittyShmReader for MockShm {
            fn read_and_unlink(
                &self,
                name: &str,
                offset: u64,
                size: u64,
                _max_bytes: usize,
            ) -> Result<Vec<u8>, &'static str> {
                if name != "/kitty-test-shm" {
                    return Err("EBADF:Failed to read image file");
                }
                let data = [11u8, 22, 33, 44, 55, 66];
                let start = usize::try_from(offset).unwrap_or(0);
                let len = usize::try_from(size).unwrap_or(0);
                Ok(data[start..start + len].to_vec())
            }
        }

        let shm = MockShm;
        let policy = KittyTransportPolicy {
            file_policy: KittyFileMediumPolicy::LocalFiles,
            max_read_bytes: 1024,
            shm_reader: Some(&shm),
        };
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let b64_name = encode_b64(b"/kitty-test-shm");
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=5,t=s,f=32,s=1,v=1,O=1,S=4;{b64_name}").as_bytes(),
            ctx(policy),
            &mut out,
        );
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b_Gi=5;OK\x1b\\");
        assert_eq!(store.get_image(5).unwrap().active_rgba(), &[22, 33, 44, 55]);
    }

    #[test]
    fn animation_frames_compose_playback_and_delete() {
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let c = ctx(KittyTransportPolicy::default());

        // Base 2x1 opaque red image (frame 1).
        let f1 = encode_b64(&[255, 0, 0, 255, 255, 0, 0, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=t,i=10,f=32,s=2,v=1;{f1}").as_bytes(),
            c,
            &mut out,
        );
        let rev1 = store.get_image(10).unwrap().revision;

        // Frame 2: copy frame 1 (c=1), overwrite pixel (1, 0) with green (X=1, x=1, y=0), gap 100ms.
        let green = encode_b64(&[0, 255, 0, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=f,i=10,c=1,x=1,y=0,s=1,v=1,X=1,z=100;{green}").as_bytes(),
            c,
            &mut out,
        );

        // Frame 3: blank Y=0x0000FFFF (blue opaque), compose (0,0) of frame 1 onto (1,0) of frame 3 via a=c.
        handle_kitty_apc_with_context(&mut store, b"Ga=f,i=10,Y=65535,z=50;", c, &mut out);
        out.clear();
        handle_kitty_apc_with_context(
            &mut store,
            b"Ga=c,i=10,r=1,c=3,w=1,h=1,X=0,Y=0,x=1,y=0,C=1",
            c,
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b_Gi=10;OK\x1b\\"
        );
        out.clear();

        // Overlapping self-compose returns EINVAL.
        handle_kitty_apc_with_context(
            &mut store,
            b"Ga=c,i=10,r=1,c=1,w=2,h=1,X=0,Y=0,x=0,y=0",
            c,
            &mut out,
        );
        assert!(String::from_utf8(out.clone()).unwrap().contains("EINVAL"));
        out.clear();

        // Set frame 1 gap to 50ms and start Playing (s=3, v=2 => 1 loop).
        handle_kitty_apc_with_context(&mut store, b"Ga=a,i=10,r=1,z=50,s=3,v=2", c, &mut out);
        assert!(store.next_animation_deadline().is_some());

        // Tick t=1.000 anchors deadline 1.050 (stays on frame 1).
        assert!(!store.tick_animations(1.000));
        assert_eq!(store.next_animation_deadline(), Some(1.050));

        // Tick t=1.050 switches to frame 2 (red, green) and bumps revision!
        assert!(store.tick_animations(1.050));
        let img = store.get_image(10).unwrap();
        assert!(img.revision > rev1);
        assert_eq!(img.active_rgba(), &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!((store.next_animation_deadline().unwrap() - 1.150).abs() < 1e-6);

        // Tick t=1.150 switches to frame 3 (blue, red).
        assert!(store.tick_animations(1.150));
        assert_eq!(
            store.get_image(10).unwrap().active_rgba(),
            &[0, 0, 255, 255, 255, 0, 0, 255]
        );

        // Tick t=1.200: loop_remaining=1 stops at frame 3!
        assert!(!store.tick_animations(1.200));
        assert!(store.next_animation_deadline().is_none());

        // d=f resets extra frames back to root frame 1.
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=f,i=10", c, &mut out);
        assert_eq!(
            store.get_image(10).unwrap().active_rgba(),
            &[255, 0, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn diacritics_table_and_unicode_placeholder_decoding() {
        assert_eq!(KITTY_DIACRITICS.len(), 297);
        for w in KITTY_DIACRITICS.windows(2) {
            assert!(w[0] < w[1]);
        }
        assert_eq!(diacritic_index('\u{0305}'), Some(0));
        assert_eq!(diacritic_index('\u{030D}'), Some(1));
        assert_eq!(diacritic_index('\u{030E}'), Some(2));
        assert_eq!(diacritic_index('\u{1D244}'), Some(296));
        assert_eq!(diacritic_index('a'), None);

        let c0 = Cell::new(
            KITTY_PLACEHOLDER_CHAR,
            PackedColor::rgb(0x11, 0x22, 0x33),
            PackedColor::Default,
            CellFlags::empty(),
        );
        let e0 = CellExtra {
            combining: vec!['\u{0305}', '\u{0305}', '\u{030E}'],
            underline_color: Some(PackedColor::indexed(7)),
            ..CellExtra::default()
        };
        let p0 = decode_placeholder_cell(&c0, Some(&e0)).unwrap();
        assert_eq!(p0.image_id, (2 << 24) | 0x11_2233);
        assert_eq!(p0.placement_id, 7);
        assert_eq!((p0.row, p0.col, p0.id_msb), (Some(0), Some(0), Some(2)));

        // Bare U+10EEEE cell to the right inherits row=0, col=1, id_msb=2.
        let p1 = decode_placeholder_cell(
            &c0,
            Some(&CellExtra {
                underline_color: Some(PackedColor::indexed(7)),
                ..CellExtra::default()
            }),
        )
        .unwrap()
        .resolve_with_left(Some(p0));
        assert_eq!(p1.image_id, (2 << 24) | 0x11_2233);
        assert_eq!((p1.row, p1.col, p1.id_msb), (Some(0), Some(1), Some(2)));
    }

    #[test]
    fn virtual_placements_feature_query_relative_and_delete_fixes() {
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let c = ctx(KittyTransportPolicy::default());

        // Feature query e=T,U=1,C=1
        handle_kitty_apc_with_context(&mut store, b"Ga=q,i=31,e=T,U=1,C=1", c, &mut out);
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b_Gi=31,e=T,C=1,U=1;OK\x1b\\"
        );
        out.clear();

        // Direct query a=q,U=1
        handle_kitty_apc_with_context(&mut store, b"Ga=q,i=32,U=1", c, &mut out);
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b_Gi=32;OK\x1b\\"
        );
        out.clear();

        // Upload image 1 + virtual placement (1, 10)
        let px = encode_b64(&[1, 2, 3, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=T,i=1,p=10,U=1,c=2,r=2,f=32,s=1,v=1;{px}").as_bytes(),
            c,
            &mut out,
        );
        assert!(store.virtual_placement(1, 10).is_some());
        assert!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(100), 24)
                .is_empty()
        );

        // d=a must NOT delete virtual placement, and must NOT free stored images.
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=T,i=2,p=20,X=3,Y=4,c=2,r=2,f=32,s=1,v=1;{px}").as_bytes(),
            c,
            &mut out,
        );
        assert_eq!(
            (
                store.visible_placements(ScreenKind::Primary, AbsLine(100), 24)[0].sub_x,
                store.visible_placements(ScreenKind::Primary, AbsLine(100), 24)[0].sub_y,
            ),
            (3, 4)
        );
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=a", c, &mut out);
        assert!(store.get_image(2).is_some());
        assert!(store.virtual_placement(1, 10).is_some());

        // Missing parent -> ENOPARENT.
        out.clear();
        handle_kitty_apc_with_context(&mut store, b"Ga=p,i=2,p=21,P=99,Q=1", c, &mut out);
        assert!(
            String::from_utf8(out.clone())
                .unwrap()
                .contains("ENOPARENT")
        );
        out.clear();

        // Valid relative child + cycle check -> ECYCLE.
        handle_kitty_apc_with_context(&mut store, b"Ga=p,i=2,p=21,c=1,r=1", c, &mut out);
        handle_kitty_apc_with_context(
            &mut store,
            b"Ga=p,i=2,p=22,P=2,Q=21,H=1,V=2,c=1,r=1",
            c,
            &mut out,
        );
        out.clear();
        handle_kitty_apc_with_context(&mut store, b"Ga=p,i=2,p=21,P=2,Q=22", c, &mut out);
        assert!(String::from_utf8(out.clone()).unwrap().contains("ECYCLE"));
        out.clear();

        // d=p with viewport_top_abs_line=100: child is at (102 + 2 = 104, col 3 + 1 = 4) -> x=5, y=5.
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=p,x=5,y=5", c, &mut out);
        assert_eq!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(100), 24)
                .len(),
            1
        );

        // d=x and d=y test.
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=x,x=4", c, &mut out);
        assert!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(100), 24)
                .is_empty()
        );

        // d=I frees virtual placement and unreferenced image data.
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=I,i=1,p=10", c, &mut out);
        assert!(store.virtual_placement(1, 10).is_none());
        assert!(store.get_image(1).is_none());
    }

    #[test]
    fn animation_loading_mode_frame_edit_alpha_blend_and_delete_targets() {
        let mut store = ImageStore::default();
        let mut out = Vec::new();
        let c = ctx(KittyTransportPolicy::default());

        // Mutual exclusion i + I -> EINVAL.
        handle_kitty_apc_with_context(&mut store, b"Ga=q,i=1,I=1", c, &mut out);
        assert!(String::from_utf8(out.clone()).unwrap().contains("EINVAL"));
        out.clear();

        // Virtual + relative -> EINVAL.
        let px = encode_b64(&[100, 0, 0, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=T,i=20,p=1,U=1,P=1,f=32,s=1,v=1;{px}").as_bytes(),
            c,
            &mut out,
        );
        assert!(String::from_utf8(out.clone()).unwrap().contains("EINVAL"));
        out.clear();

        // Chunked ImageNumber(I=44) upload.
        let px_b64 = encode_b64(&[0, 0, 200, 255]);
        let (a, b) = px_b64.split_at(4);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=T,I=44,f=32,s=1,v=1,m=1;{a}").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(&mut store, format!("GI=44,m=0;{b}").as_bytes(), c, &mut out);
        let id44 = store.resolve_image_number(44).unwrap();
        assert_eq!(
            store.get_image(id44).unwrap().active_rgba(),
            &[0, 0, 200, 255]
        );
        out.clear();

        // Loading mode (s=2) + chunked a=f frame 2 + alpha blend edit (r=2, X=0).
        let f2 = encode_b64(&[200, 0, 0, 255]);
        let (f2a, f2b) = f2.split_at(4);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=f,i={id44},f=32,s=1,v=1,z=50,m=1;{f2a}").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=f,i={id44},m=0;{f2b}").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=a,i={id44},r=1,z=50,s=2").as_bytes(),
            c,
            &mut out,
        );
        assert!(!store.tick_animations(0.0));
        assert!(store.tick_animations(0.050));
        assert!(store.next_animation_deadline().is_none());

        // Append frame 3 -> wakes Loading mode!
        let f3 = encode_b64(&[0, 200, 0, 255]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=f,i={id44},f=32,s=1,v=1,z=50;{f3}").as_bytes(),
            c,
            &mut out,
        );
        assert!(store.next_animation_deadline().is_some());

        // Edit existing frame 2 with alpha blend (r=2, X=0).
        let semi_white = encode_b64(&[255, 255, 255, 128]);
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=f,i={id44},r=2,f=32,s=1,v=1,X=0;{semi_white}").as_bytes(),
            c,
            &mut out,
        );
        let blended = store.get_image(id44).unwrap().active_rgba();
        assert!(blended[0] > 200 && blended[1] > 100 && blended[3] == 255);

        // d=q and d=Y tests.
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=p,i={id44},p=1,c=1,r=1,z=-5").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=q,x=4,y=3,z=0", c, &mut out);
        assert_eq!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(100), 24)
                .len(),
            1
        );
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=q,x=4,y=3,z=-5", c, &mut out);
        assert!(
            store
                .visible_placements(ScreenKind::Primary, AbsLine(100), 24)
                .is_empty()
        );
        handle_kitty_apc_with_context(
            &mut store,
            format!("Ga=p,i={id44},p=1,c=1,r=1").as_bytes(),
            c,
            &mut out,
        );
        handle_kitty_apc_with_context(&mut store, b"Ga=d,d=Y,y=3", c, &mut out);
        assert!(store.get_image(id44).is_none());
    }
}
