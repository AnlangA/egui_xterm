//! Kitty Keyboard Protocol progressive enhancement flags, per-screen stacks, and key encoder
//! (`kitty-keyboard` feature).

use std::fmt::Write as _;

/// Maximum entries retained per screen's Kitty keyboard stack.
pub const KITTY_STACK_CAP: usize = 256;

bitflags::bitflags! {
    /// Progressive enhancement flags for the Kitty Keyboard Protocol (`CSI > flags u`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct KittyKeyboardFlags: u8 {
        /// Bit 0 (`1`): disambiguate escape codes (`Esc`, `CSI u` on conflicts).
        const DISAMBIGUATE_ESCAPE_CODES     = 1 << 0;
        /// Bit 1 (`2`): report key repeat (`:2`) and key release (`:3`) event types.
        const REPORT_EVENT_TYPES            = 1 << 1;
        /// Bit 2 (`4`): report shifted and base-layout alternate key codes.
        const REPORT_ALTERNATE_KEYS         = 1 << 2;
        /// Bit 3 (`8`): report all keys (including plain keys and modifiers) as escape codes.
        const REPORT_ALL_KEYS_AS_ESCAPE_CODES = 1 << 3;
        /// Bit 4 (`16`): report associated UTF-8 text codepoints in the third parameter.
        const REPORT_ASSOCIATED_TEXT        = 1 << 4;
    }
}

/// Key press lifecycle phase for [`KittyKeyEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum KittyEventType {
    /// Key pressed (`1`, omitted in canonical CSI encoding).
    #[default]
    Press = 1,
    /// Key auto-repeated while held (`2`).
    Repeat = 2,
    /// Key released (`3`).
    Release = 3,
}

/// Keyboard modifier and lock-key state for [`KittyKeyEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KittyModifiers {
    /// Shift modifier (`bit 0`, `+1`).
    pub shift: bool,
    /// Alt / Option modifier (`bit 1`, `+2`).
    pub alt: bool,
    /// Control modifier (`bit 2`, `+4`).
    pub ctrl: bool,
    /// Super / Command / Windows modifier (`bit 3`, `+8`).
    pub super_key: bool,
    /// Hyper modifier (`bit 4`, `+16`).
    pub hyper: bool,
    /// Meta modifier (`bit 5`, `+32`).
    pub meta: bool,
    /// Caps Lock state (`bit 6`, `+64`).
    pub caps_lock: bool,
    /// Num Lock state (`bit 7`, `+128`).
    pub num_lock: bool,
}

impl KittyModifiers {
    /// Returns the raw Kitty modifier bitmask (`0..=255`).
    #[must_use]
    pub const fn bits(self) -> u8 {
        let mut mask = 0u8;
        if self.shift {
            mask |= 1;
        }
        if self.alt {
            mask |= 2;
        }
        if self.ctrl {
            mask |= 4;
        }
        if self.super_key {
            mask |= 8;
        }
        if self.hyper {
            mask |= 16;
        }
        if self.meta {
            mask |= 32;
        }
        if self.caps_lock {
            mask |= 64;
        }
        if self.num_lock {
            mask |= 128;
        }
        mask
    }

    /// Returns the Kitty protocol modifier parameter (`1 + bits`).
    #[must_use]
    pub const fn param_value(self) -> u16 {
        1 + self.bits() as u16
    }

    /// Returns `true` if any active non-lock modifier (`shift`, `alt`, `ctrl`, `super_key`,
    /// `hyper`, or `meta`) is held.
    #[must_use]
    pub const fn has_active_modifiers(self) -> bool {
        self.shift || self.alt || self.ctrl || self.super_key || self.hyper || self.meta
    }

    /// Returns `true` if any modifier other than `shift` (`alt`, `ctrl`, `super_key`, `hyper`,
    /// `meta`) is held.
    #[must_use]
    pub const fn has_non_shift_modifiers(self) -> bool {
        self.alt || self.ctrl || self.super_key || self.hyper || self.meta
    }
}

/// Logical key identifier for [`KittyKeyEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum KittyKey {
    /// Printable Unicode character (base unshifted lowercase key where applicable).
    Char(char),
    /// Enter / Return (`13`).
    Enter,
    /// Tab (`9`).
    Tab,
    /// Backspace (`127`).
    Backspace,
    /// Escape (`27`).
    Escape,
    /// Spacebar (`32`).
    Space,
    /// Up arrow (`CSI A`).
    Up,
    /// Down arrow (`CSI B`).
    Down,
    /// Right arrow (`CSI C`).
    Right,
    /// Left arrow (`CSI D`).
    Left,
    /// Home (`CSI H`).
    Home,
    /// End (`CSI F`).
    End,
    /// Insert (`CSI 2 ~`).
    Insert,
    /// Delete (`CSI 3 ~`).
    Delete,
    /// Page Up (`CSI 5 ~`).
    PageUp,
    /// Page Down (`CSI 6 ~`).
    PageDown,
    /// Function key `F1..=F35`.
    F(u8),
    /// Caps Lock (`57358`).
    CapsLock,
    /// Scroll Lock (`57359`).
    ScrollLock,
    /// Num Lock (`57360`).
    NumLock,
    /// Print Screen (`57361`).
    PrintScreen,
    /// Pause (`57362`).
    Pause,
    /// Menu / Application (`57363`).
    Menu,
    /// Left Shift (`57441`).
    LeftShift,
    /// Left Control (`57442`).
    LeftControl,
    /// Left Alt (`57443`).
    LeftAlt,
    /// Left Super (`57444`).
    LeftSuper,
    /// Left Hyper (`57445`).
    LeftHyper,
    /// Left Meta (`57446`).
    LeftMeta,
    /// Right Shift (`57447`).
    RightShift,
    /// Right Control (`57448`).
    RightControl,
    /// Right Alt (`57449`).
    RightAlt,
    /// Right Super (`57450`).
    RightSuper,
    /// Right Hyper (`57451`).
    RightHyper,
    /// Right Meta (`57452`).
    RightMeta,
    /// Raw Kitty functional or Unicode key code.
    KeyCode(u32),
}

/// Key event input to [`encode_kitty_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyKeyEvent<'a> {
    /// Logical key pressed, repeated, or released.
    pub key: KittyKey,
    /// Press, repeat, or release phase.
    pub event_type: KittyEventType,
    /// Held modifiers and lock states.
    pub modifiers: KittyModifiers,
    /// Optional shifted character equivalent on the active keyboard layout.
    pub shifted_key: Option<char>,
    /// Optional US-QWERTY / base-layout character equivalent.
    pub base_layout_key: Option<char>,
    /// Optional UTF-8 text associated with this key event.
    pub associated_text: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KittyKeyboardState {
    primary_stack: Vec<KittyKeyboardFlags>,
    alt_stack: Vec<KittyKeyboardFlags>,
}

impl KittyKeyboardState {
    pub(crate) fn flags(&self, alt_screen: bool) -> KittyKeyboardFlags {
        self.stack(alt_screen).last().copied().unwrap_or_default()
    }

    pub(crate) fn push(&mut self, alt_screen: bool, flags: KittyKeyboardFlags, mode: u16) {
        let current = self.flags(alt_screen);
        let next = apply_mode(current, flags, mode);
        let stack = self.stack_mut(alt_screen);
        if stack.len() >= KITTY_STACK_CAP {
            stack.remove(0);
        }
        stack.push(next);
    }

    pub(crate) fn pop(&mut self, alt_screen: bool, count: usize) {
        let stack = self.stack_mut(alt_screen);
        let new_len = stack.len().saturating_sub(count);
        stack.truncate(new_len);
    }

    pub(crate) fn set(&mut self, alt_screen: bool, flags: KittyKeyboardFlags, mode: u16) {
        let current = self.flags(alt_screen);
        let next = apply_mode(current, flags, mode);
        let stack = self.stack_mut(alt_screen);
        if let Some(top) = stack.last_mut() {
            *top = next;
        } else {
            stack.push(next);
        }
    }

    pub(crate) fn reset_alt(&mut self) {
        self.alt_stack.clear();
    }

    pub(crate) fn reset_all(&mut self) {
        self.primary_stack.clear();
        self.alt_stack.clear();
    }

    #[cfg(feature = "serde")]
    pub(crate) fn export_stacks(&self) -> (Vec<KittyKeyboardFlags>, Vec<KittyKeyboardFlags>) {
        (self.primary_stack.clone(), self.alt_stack.clone())
    }

    #[cfg(feature = "serde")]
    pub(crate) fn restore_stacks(
        &mut self,
        mut primary: Vec<KittyKeyboardFlags>,
        mut alt: Vec<KittyKeyboardFlags>,
    ) {
        if primary.len() > KITTY_STACK_CAP {
            primary.drain(..primary.len() - KITTY_STACK_CAP);
        }
        if alt.len() > KITTY_STACK_CAP {
            alt.drain(..alt.len() - KITTY_STACK_CAP);
        }
        for f in &mut primary {
            *f = KittyKeyboardFlags::from_bits_truncate(f.bits());
        }
        for f in &mut alt {
            *f = KittyKeyboardFlags::from_bits_truncate(f.bits());
        }
        self.primary_stack = primary;
        self.alt_stack = alt;
    }

    fn stack(&self, alt_screen: bool) -> &[KittyKeyboardFlags] {
        if alt_screen {
            &self.alt_stack
        } else {
            &self.primary_stack
        }
    }

    fn stack_mut(&mut self, alt_screen: bool) -> &mut Vec<KittyKeyboardFlags> {
        if alt_screen {
            &mut self.alt_stack
        } else {
            &mut self.primary_stack
        }
    }
}

const fn apply_mode(
    current: KittyKeyboardFlags,
    flags: KittyKeyboardFlags,
    mode: u16,
) -> KittyKeyboardFlags {
    match mode {
        2 => current.union(flags),
        3 => current.difference(flags),
        _ => flags,
    }
}

/// Encodes `ev` into VT/Kitty byte sequences according to `flags`, or returns `None` if no bytes
/// should be transmitted (e.g., key release when [`KittyKeyboardFlags::REPORT_EVENT_TYPES`] is off).
#[must_use]
pub fn encode_kitty_key(flags: KittyKeyboardFlags, ev: &KittyKeyEvent<'_>) -> Option<Vec<u8>> {
    let report_events = flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES);
    if !report_events && ev.event_type == KittyEventType::Release {
        return None;
    }

    if flags.is_empty() {
        return encode_legacy(ev);
    }

    let disambiguate = flags.contains(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
    let report_all = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let report_alt = flags.contains(KittyKeyboardFlags::REPORT_ALTERNATE_KEYS);
    let report_text = flags.contains(KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT);

    if is_modifier_key(ev.key) && !report_all {
        return None;
    }

    let need_event_suffix = report_events && ev.event_type != KittyEventType::Press;
    let mod_param = ev.modifiers.param_value();
    let has_active_mod = ev.modifiers.has_active_modifiers();
    let has_any_mod_bits = ev.modifiers.bits() != 0;
    let has_non_shift = ev.modifiers.has_non_shift_modifiers();

    // Legacy exceptions when `REPORT_ALL_KEYS_AS_ESCAPE_CODES` is not active.
    if !report_all && !need_event_suffix {
        match ev.key {
            KittyKey::Enter if !has_active_mod => return Some(b"\r".to_vec()),
            KittyKey::Tab if !has_active_mod => return Some(b"\t".to_vec()),
            KittyKey::Backspace if !has_active_mod => return Some(b"\x7f".to_vec()),
            KittyKey::Space if !has_active_mod => return Some(b" ".to_vec()),
            KittyKey::Char(ch) if !has_non_shift => {
                if let Some(txt) = ev.associated_text
                    && !txt.is_empty()
                {
                    return Some(txt.as_bytes().to_vec());
                }
                let out_ch = if ev.modifiers.shift {
                    ev.shifted_key.unwrap_or_else(|| ch.to_ascii_uppercase())
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                return Some(out_ch.encode_utf8(&mut buf).as_bytes().to_vec());
            }
            _ => {}
        }
    }

    // Special-key CSI encodings (`CSI [1 ; mods[:event]] A/B/C/D/H/F/P/Q/R/S` or `CSI num ; mods[:event] ~`).
    if ev.key == KittyKey::F(3)
        && (disambiguate || report_all || has_active_mod || need_event_suffix)
    {
        let mut out = String::from("\x1b[13");
        if need_event_suffix {
            let _ = write!(out, ";{mod_param}:{}", ev.event_type as u8);
        } else if has_active_mod || (report_all && has_any_mod_bits) {
            let _ = write!(out, ";{mod_param}");
        }
        out.push('~');
        return Some(out.into_bytes());
    }
    if let Some((prefix_num, suffix_ch)) = special_csi_key(ev.key) {
        if !has_active_mod && !need_event_suffix && !report_all {
            return match prefix_num {
                None => Some(format!("\x1b[{suffix_ch}").into_bytes()),
                Some(_) if suffix_ch != '~' => Some(format!("\x1bO{suffix_ch}").into_bytes()),
                Some(n) => Some(format!("\x1b[{n}~").into_bytes()),
            };
        }
        let mut out = String::from("\x1b[");
        let lead = prefix_num.unwrap_or(1);
        if need_event_suffix {
            let _ = write!(out, "{lead};{mod_param}:{}{suffix_ch}", ev.event_type as u8);
        } else if has_active_mod || (report_all && has_any_mod_bits) {
            let _ = write!(out, "{lead};{mod_param}{suffix_ch}");
        } else if let Some(n) = prefix_num {
            if suffix_ch == '~' {
                let _ = write!(out, "{n}~");
            } else {
                let _ = write!(out, "{n}{suffix_ch}");
            }
        } else {
            out.push(suffix_ch);
        }
        return Some(out.into_bytes());
    }

    // If neither disambiguate, report_all, nor event suffix applies, fall back to legacy.
    if !disambiguate && !report_all && !need_event_suffix && !report_text {
        return encode_legacy(ev);
    }

    // Canonical `CSI key[:shifted[:base]] [; mods[:event]] [; text...] u`
    let key_code = kitty_key_code(ev.key)?;
    let mut out = String::with_capacity(24);
    let _ = write!(out, "\x1b[{key_code}");

    if report_alt {
        let shifted_code = ev.shifted_key.map(u32::from).filter(|&s| s != key_code);
        let base_code = ev.base_layout_key.map(u32::from).filter(|&b| b != key_code);
        match (shifted_code, base_code) {
            (Some(s), Some(b)) => {
                let _ = write!(out, ":{s}:{b}");
            }
            (Some(s), None) => {
                let _ = write!(out, ":{s}");
            }
            (None, Some(b)) => {
                let _ = write!(out, "::{b}");
            }
            (None, None) => {}
        }
    }

    let text_cps: Vec<u32> = if report_text && ev.event_type != KittyEventType::Release {
        ev.associated_text
            .unwrap_or("")
            .chars()
            .filter(|&c| !c.is_control())
            .map(u32::from)
            .collect()
    } else {
        Vec::new()
    };

    if need_event_suffix {
        let _ = write!(out, ";{mod_param}:{}", ev.event_type as u8);
    } else if has_active_mod || (report_all && has_any_mod_bits) {
        let _ = write!(out, ";{mod_param}");
    } else if !text_cps.is_empty() {
        out.push(';');
    }

    if !text_cps.is_empty() {
        out.push(';');
        for (i, cp) in text_cps.iter().enumerate() {
            if i > 0 {
                out.push(':');
            }
            let _ = write!(out, "{cp}");
        }
    }

    out.push('u');
    Some(out.into_bytes())
}

fn encode_legacy(ev: &KittyKeyEvent<'_>) -> Option<Vec<u8>> {
    if ev.event_type == KittyEventType::Release {
        return None;
    }
    let mod_param = 1 + u16::from(ev.modifiers.bits() & 0x3F);
    let has_any_mod = ev.modifiers.has_active_modifiers();

    if let Some((prefix_num, suffix_ch)) = special_csi_key(ev.key) {
        if !has_any_mod {
            return match prefix_num {
                None => Some(format!("\x1b[{suffix_ch}").into_bytes()),
                Some(_) if suffix_ch != '~' => Some(format!("\x1bO{suffix_ch}").into_bytes()),
                Some(n) => Some(format!("\x1b[{n}~").into_bytes()),
            };
        }
        let lead = prefix_num.unwrap_or(1);
        return Some(format!("\x1b[{lead};{mod_param}{suffix_ch}").into_bytes());
    }

    let mut bytes = Vec::new();
    match ev.key {
        KittyKey::Enter => {
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            bytes.push(b'\r');
        }
        KittyKey::Tab => {
            if ev.modifiers.shift && !ev.modifiers.ctrl && !ev.modifiers.alt {
                return Some(b"\x1b[Z".to_vec());
            }
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            bytes.push(b'\t');
        }
        KittyKey::Backspace => {
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            bytes.push(if ev.modifiers.ctrl { 0x08 } else { 0x7F });
        }
        KittyKey::Escape => {
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            bytes.push(0x1B);
        }
        KittyKey::Space => {
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            bytes.push(if ev.modifiers.ctrl { 0x00 } else { b' ' });
        }
        KittyKey::Char(ch) => {
            if ev.modifiers.alt {
                bytes.push(0x1B);
            }
            if ev.modifiers.ctrl {
                let lower = ch.to_ascii_lowercase();
                match lower {
                    '@' => bytes.push(0x00),
                    'a'..='z' => bytes.push((lower as u8) - b'a' + 1),
                    '[' => bytes.push(0x1B),
                    '\\' => bytes.push(0x1C),
                    ']' => bytes.push(0x1D),
                    '^' => bytes.push(0x1E),
                    '_' => bytes.push(0x1F),
                    _ => return None,
                }
            } else {
                let out_ch = if ev.modifiers.shift {
                    ev.shifted_key.unwrap_or_else(|| ch.to_ascii_uppercase())
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(out_ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        _ => return None,
    }
    Some(bytes)
}

const fn is_modifier_key(key: KittyKey) -> bool {
    matches!(
        key,
        KittyKey::LeftShift
            | KittyKey::LeftControl
            | KittyKey::LeftAlt
            | KittyKey::LeftSuper
            | KittyKey::LeftHyper
            | KittyKey::LeftMeta
            | KittyKey::RightShift
            | KittyKey::RightControl
            | KittyKey::RightAlt
            | KittyKey::RightSuper
            | KittyKey::RightHyper
            | KittyKey::RightMeta
            | KittyKey::CapsLock
            | KittyKey::ScrollLock
            | KittyKey::NumLock
    )
}

const fn special_csi_key(key: KittyKey) -> Option<(Option<u16>, char)> {
    match key {
        KittyKey::Up => Some((None, 'A')),
        KittyKey::Down => Some((None, 'B')),
        KittyKey::Right => Some((None, 'C')),
        KittyKey::Left => Some((None, 'D')),
        KittyKey::Home => Some((None, 'H')),
        KittyKey::End => Some((None, 'F')),
        KittyKey::Insert => Some((Some(2), '~')),
        KittyKey::Delete => Some((Some(3), '~')),
        KittyKey::PageUp => Some((Some(5), '~')),
        KittyKey::PageDown => Some((Some(6), '~')),
        KittyKey::F(1) => Some((Some(1), 'P')),
        KittyKey::F(2) => Some((Some(1), 'Q')),
        KittyKey::F(3) => Some((Some(1), 'R')),
        KittyKey::F(4) => Some((Some(1), 'S')),
        KittyKey::F(5) => Some((Some(15), '~')),
        KittyKey::F(6) => Some((Some(17), '~')),
        KittyKey::F(7) => Some((Some(18), '~')),
        KittyKey::F(8) => Some((Some(19), '~')),
        KittyKey::F(9) => Some((Some(20), '~')),
        KittyKey::F(10) => Some((Some(21), '~')),
        KittyKey::F(11) => Some((Some(23), '~')),
        KittyKey::F(12) => Some((Some(24), '~')),
        _ => None,
    }
}

const fn kitty_key_code(key: KittyKey) -> Option<u32> {
    match key {
        KittyKey::Char(ch) => Some(ch as u32),
        KittyKey::Enter => Some(13),
        KittyKey::Tab => Some(9),
        KittyKey::Backspace => Some(127),
        KittyKey::Escape => Some(27),
        KittyKey::Space => Some(32),
        KittyKey::CapsLock => Some(57358),
        KittyKey::ScrollLock => Some(57359),
        KittyKey::NumLock => Some(57360),
        KittyKey::PrintScreen => Some(57361),
        KittyKey::Pause => Some(57362),
        KittyKey::Menu => Some(57363),
        KittyKey::F(n) if n >= 13 && n <= 35 => Some(57364 + (n as u32 - 13)),
        KittyKey::LeftShift => Some(57441),
        KittyKey::LeftControl => Some(57442),
        KittyKey::LeftAlt => Some(57443),
        KittyKey::LeftSuper => Some(57444),
        KittyKey::LeftHyper => Some(57445),
        KittyKey::LeftMeta => Some(57446),
        KittyKey::RightShift => Some(57447),
        KittyKey::RightControl => Some(57448),
        KittyKey::RightAlt => Some(57449),
        KittyKey::RightSuper => Some(57450),
        KittyKey::RightHyper => Some(57451),
        KittyKey::RightMeta => Some(57452),
        KittyKey::KeyCode(code) => Some(code),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{CoreConfig, Terminal};

    #[test]
    fn kitty_stacks_modes_overflow_and_reset() {
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(b"\x1b[?u\x1b[>1;1u\x1b[>4;2u\x1b[?u\x1b[=1;3u\x1b[?u");
        assert_eq!(term.drain_outgoing(), b"\x1b[?0u\x1b[?5u\x1b[?4u");
        assert_eq!(
            term.kitty_keyboard_flags(),
            KittyKeyboardFlags::REPORT_ALTERNATE_KEYS
        );

        // Alt screen gets independent stack and clears on ?1049h.
        term.advance(b"\x1b[?1049h\x1b[?u\x1b[>31u\x1b[?u\x1b[?1049l\x1b[?u");
        assert_eq!(term.drain_outgoing(), b"\x1b[?0u\x1b[?31u\x1b[?4u");

        // Overflow bounded at KITTY_STACK_CAP.
        for _ in 0..(KITTY_STACK_CAP + 40) {
            term.advance(b"\x1b[>2u");
        }
        assert_eq!(
            term.kitty_keyboard_flags(),
            KittyKeyboardFlags::REPORT_EVENT_TYPES
        );
        for _ in 0..KITTY_STACK_CAP {
            term.advance(b"\x1b[<u");
        }
        assert!(term.kitty_keyboard_flags().is_empty());

        // RIS clears primary & alt stacks.
        term.advance(b"\x1b[>7u\x1bc\x1b[?u");
        assert!(term.kitty_keyboard_flags().is_empty());
        assert_eq!(term.drain_outgoing(), b"\x1b[?0u");
    }

    #[test]
    fn encode_kitty_key_progressive_flags() {
        // Flag 0 legacy: release suppressed, Tab/Ctrl+I legacy.
        let rel = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Release,
            modifiers: KittyModifiers::default(),
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(encode_kitty_key(KittyKeyboardFlags::empty(), &rel), None);

        // Flag 1 disambiguates Escape and Ctrl+I while keeping plain Tab.
        let esc = KittyKeyEvent {
            key: KittyKey::Escape,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers::default(),
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(
            encode_kitty_key(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES, &esc).as_deref(),
            Some(b"\x1b[27u".as_slice())
        );
        let ctrl_i = KittyKeyEvent {
            key: KittyKey::Char('i'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(
            encode_kitty_key(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES, &ctrl_i).as_deref(),
            Some(b"\x1b[105;5u".as_slice())
        );

        // Flag 2 event types (repeat & release).
        let f12 =
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES | KittyKeyboardFlags::REPORT_EVENT_TYPES;
        let ctrl_a_rel = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Release,
            modifiers: KittyModifiers {
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(
            encode_kitty_key(f12, &ctrl_a_rel).as_deref(),
            Some(b"\x1b[97;5:3u".as_slice())
        );

        // Flags 1 | 4 | 8: shifted alternate key.
        let f13 = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
            | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS
            | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
        let shift_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..KittyModifiers::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        assert_eq!(
            encode_kitty_key(f13, &shift_a).as_deref(),
            Some(b"\x1b[97:65;2u".as_slice())
        );

        // Flags 1 | 8 | 16: associated text.
        let f25 = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
            | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            | KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
        let plain_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers::default(),
            shifted_key: None,
            base_layout_key: None,
            associated_text: Some("a"),
        };
        assert_eq!(
            encode_kitty_key(f25, &plain_a).as_deref(),
            Some(b"\x1b[97;;97u".as_slice())
        );
    }
}
