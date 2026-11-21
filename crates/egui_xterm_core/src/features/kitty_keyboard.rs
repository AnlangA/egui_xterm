//! Kitty Keyboard Protocol progressive enhancement flags, per-screen stacks, and key encoder
//! (`kitty-keyboard` feature).

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
    /// Keypad `0` (`57399`).
    Kp0,
    /// Keypad `1` (`57400`).
    Kp1,
    /// Keypad `2` (`57401`).
    Kp2,
    /// Keypad `3` (`57402`).
    Kp3,
    /// Keypad `4` (`57403`).
    Kp4,
    /// Keypad `5` (`57404`).
    Kp5,
    /// Keypad `6` (`57405`).
    Kp6,
    /// Keypad `7` (`57406`).
    Kp7,
    /// Keypad `8` (`57407`).
    Kp8,
    /// Keypad `9` (`57408`).
    Kp9,
    /// Keypad Decimal `.` (`57409`).
    KpDecimal,
    /// Keypad Divide `/` (`57410`).
    KpDivide,
    /// Keypad Multiply `*` (`57411`).
    KpMultiply,
    /// Keypad Subtract `-` (`57412`).
    KpSubtract,
    /// Keypad Add `+` (`57413`).
    KpAdd,
    /// Keypad Enter (`57414`).
    KpEnter,
    /// Keypad Equal `=` (`57415`).
    KpEqual,
    /// Keypad Separator `,` (`57416`).
    KpSeparator,
    /// Keypad Left (`57417`).
    KpLeft,
    /// Keypad Right (`57418`).
    KpRight,
    /// Keypad Up (`57419`).
    KpUp,
    /// Keypad Down (`57420`).
    KpDown,
    /// Keypad Page Up (`57421`).
    KpPageUp,
    /// Keypad Page Down (`57422`).
    KpPageDown,
    /// Keypad Home (`57423`).
    KpHome,
    /// Keypad End (`57424`).
    KpEnd,
    /// Keypad Insert (`57425`).
    KpInsert,
    /// Keypad Delete (`57426`).
    KpDelete,
    /// Keypad Begin (`57427`).
    KpBegin,
    /// Raw Kitty functional or Unicode key code.
    KeyCode(u32),
}

/// xterm `modifyOtherKeys` level configured via `CSI > 4 ; Pv m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum ModifyOtherKeys {
    /// Disabled (`Pv = 0`, default).
    #[default]
    Disabled = 0,
    /// Encode modified keys lacking standard legacy encodings (`Pv = 1`).
    Enabled = 1,
    /// Encode all modified keys except unmodified or plain-Shift printables (`Pv = 2`).
    Always = 2,
}

impl ModifyOtherKeys {
    /// Converts a `CSI > 4 ; Pv m` parameter value (`0..=2`) into [`ModifyOtherKeys`].
    #[must_use]
    pub const fn from_pv(pv: u16) -> Option<Self> {
        match pv {
            0 => Some(Self::Disabled),
            1 => Some(Self::Enabled),
            2 => Some(Self::Always),
            _ => None,
        }
    }

    /// Returns the numeric `Pv` representation (`0`, `1`, or `2`).
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
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

#[inline]
fn push_u32_dec(out: &mut Vec<u8>, mut val: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        if val == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

/// Encodes `ev` into `out` according to `flags`, returning `true` if bytes were written.
///
/// Leaves `out` unmodified if `false` is returned (e.g., key release when
/// [`KittyKeyboardFlags::REPORT_EVENT_TYPES`] is off).
pub fn encode_kitty_key_into(
    flags: KittyKeyboardFlags,
    ev: &KittyKeyEvent<'_>,
    out: &mut Vec<u8>,
) -> bool {
    let report_events = flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES);
    if !report_events && ev.event_type == KittyEventType::Release {
        return false;
    }

    if flags.is_empty() {
        return encode_legacy_into(ev, out);
    }

    let disambiguate = flags.contains(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
    let report_all = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let report_alt = flags.contains(KittyKeyboardFlags::REPORT_ALTERNATE_KEYS);
    let report_text = flags.contains(KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT);

    if is_modifier_key(ev.key) && !report_all {
        return false;
    }

    let need_event_suffix = report_events && ev.event_type != KittyEventType::Press;
    let mod_param = ev.modifiers.param_value();
    let has_active_mod = ev.modifiers.has_active_modifiers();
    let has_any_mod_bits = ev.modifiers.bits() != 0;
    let has_non_shift = ev.modifiers.has_non_shift_modifiers();

    // Legacy exceptions when `REPORT_ALL_KEYS_AS_ESCAPE_CODES` is not active.
    if !report_all && !need_event_suffix {
        match ev.key {
            KittyKey::Enter if !has_active_mod => {
                out.push(b'\r');
                return true;
            }
            KittyKey::Tab if !has_active_mod => {
                out.push(b'\t');
                return true;
            }
            KittyKey::Backspace if !has_active_mod => {
                out.push(0x7F);
                return true;
            }
            KittyKey::Space if !has_active_mod => {
                out.push(b' ');
                return true;
            }
            KittyKey::Char(ch) if !has_non_shift => {
                if let Some(txt) = ev.associated_text
                    && !txt.is_empty()
                {
                    out.extend_from_slice(txt.as_bytes());
                    return true;
                }
                let out_ch = if ev.modifiers.shift {
                    ev.shifted_key.unwrap_or_else(|| ch.to_ascii_uppercase())
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                out.extend_from_slice(out_ch.encode_utf8(&mut buf).as_bytes());
                return true;
            }
            _ => {}
        }
    }

    // Special-key CSI encodings (`CSI [1 ; mods[:event]] A/B/C/D/H/F/P/Q/R/S` or `CSI num ; mods[:event] ~`).
    if ev.key == KittyKey::F(3)
        && (disambiguate || report_all || has_active_mod || need_event_suffix)
    {
        out.extend_from_slice(b"\x1b[13");
        if need_event_suffix {
            out.push(b';');
            push_u32_dec(out, u32::from(mod_param));
            out.push(b':');
            out.push(b'0' + (ev.event_type as u8));
        } else if has_active_mod || (report_all && has_any_mod_bits) {
            out.push(b';');
            push_u32_dec(out, u32::from(mod_param));
        }
        out.push(b'~');
        return true;
    }
    if let Some((prefix_num, suffix_ch)) = special_csi_key(ev.key) {
        if !has_active_mod && !need_event_suffix && !report_all {
            match prefix_num {
                None => {
                    out.extend_from_slice(b"\x1b[");
                    out.push(suffix_ch as u8);
                }
                Some(_) if suffix_ch != '~' => {
                    out.extend_from_slice(b"\x1bO");
                    out.push(suffix_ch as u8);
                }
                Some(n) => {
                    out.extend_from_slice(b"\x1b[");
                    push_u32_dec(out, u32::from(n));
                    out.push(b'~');
                }
            }
            return true;
        }
        out.extend_from_slice(b"\x1b[");
        let lead = prefix_num.unwrap_or(1);
        if need_event_suffix {
            push_u32_dec(out, u32::from(lead));
            out.push(b';');
            push_u32_dec(out, u32::from(mod_param));
            out.push(b':');
            out.push(b'0' + (ev.event_type as u8));
            out.push(suffix_ch as u8);
        } else if has_active_mod || (report_all && has_any_mod_bits) {
            push_u32_dec(out, u32::from(lead));
            out.push(b';');
            push_u32_dec(out, u32::from(mod_param));
            out.push(suffix_ch as u8);
        } else if let Some(n) = prefix_num {
            push_u32_dec(out, u32::from(n));
            out.push(suffix_ch as u8);
        } else {
            out.push(suffix_ch as u8);
        }
        return true;
    }

    // If neither disambiguate, report_all, nor event suffix applies, fall back to legacy.
    if !disambiguate && !report_all && !need_event_suffix && !report_text {
        return encode_legacy_into(ev, out);
    }

    // Canonical `CSI key[:shifted[:base]] [; mods[:event]] [; text...] u`
    let Some(key_code) = kitty_key_code(ev.key) else {
        return false;
    };
    out.extend_from_slice(b"\x1b[");
    push_u32_dec(out, key_code);

    if report_alt {
        let shifted_code = ev.shifted_key.map(u32::from).filter(|&s| s != key_code);
        let base_code = ev.base_layout_key.map(u32::from).filter(|&b| b != key_code);
        match (shifted_code, base_code) {
            (Some(s), Some(b)) => {
                out.push(b':');
                push_u32_dec(out, s);
                out.push(b':');
                push_u32_dec(out, b);
            }
            (Some(s), None) => {
                out.push(b':');
                push_u32_dec(out, s);
            }
            (None, Some(b)) => {
                out.extend_from_slice(b"::");
                push_u32_dec(out, b);
            }
            (None, None) => {}
        }
    }

    let has_text = report_text
        && ev.event_type != KittyEventType::Release
        && ev
            .associated_text
            .is_some_and(|t| t.chars().any(|c| !c.is_control()));

    if need_event_suffix {
        out.push(b';');
        push_u32_dec(out, u32::from(mod_param));
        out.push(b':');
        out.push(b'0' + (ev.event_type as u8));
    } else if has_active_mod || (report_all && has_any_mod_bits) {
        out.push(b';');
        push_u32_dec(out, u32::from(mod_param));
    } else if has_text {
        out.push(b';');
    }

    if has_text && let Some(txt) = ev.associated_text {
        out.push(b';');
        let mut first = true;
        for ch in txt.chars().filter(|&c| !c.is_control()) {
            if !first {
                out.push(b':');
            }
            first = false;
            push_u32_dec(out, u32::from(ch));
        }
    }

    out.push(b'u');
    true
}

/// Encodes `ev` into VT/Kitty byte sequences according to `flags`, or returns `None` if no bytes
/// should be transmitted (e.g., key release when [`KittyKeyboardFlags::REPORT_EVENT_TYPES`] is off).
#[must_use]
pub fn encode_kitty_key(flags: KittyKeyboardFlags, ev: &KittyKeyEvent<'_>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(24);
    if encode_kitty_key_into(flags, ev, &mut out) {
        Some(out)
    } else {
        None
    }
}

/// Encodes `ev` using xterm `modifyOtherKeys` (`CSI 27 ; <mod> ; <code> ~`) into `out` when
/// `level != ModifyOtherKeys::Disabled` and applicable; returns `true` if bytes were written.
///
/// Callers implementing terminal key routing must check [`KittyKeyboardFlags`] first: when Kitty
/// keyboard flags are non-empty, Kitty encoding takes strict precedence over `modifyOtherKeys`.
pub fn encode_modify_other_keys_into(
    level: ModifyOtherKeys,
    ev: &KittyKeyEvent<'_>,
    out: &mut Vec<u8>,
) -> bool {
    if level == ModifyOtherKeys::Disabled || ev.event_type == KittyEventType::Release {
        return false;
    }

    let mod_mask = ev.modifiers.bits() & 0x07;
    if mod_mask == 0 {
        return false;
    }
    let mod_param = 1 + u32::from(mod_mask);

    let keycode: u32 = match ev.key {
        KittyKey::Enter => {
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => ev.modifiers.shift || ev.modifiers.ctrl,
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            13
        }
        KittyKey::Tab => {
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => ev.modifiers.ctrl,
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            9
        }
        KittyKey::Backspace => {
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => ev.modifiers.shift,
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            127
        }
        KittyKey::Escape => {
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => ev.modifiers.shift || ev.modifiers.ctrl,
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            27
        }
        KittyKey::Space => {
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => ev.modifiers.shift && ev.modifiers.ctrl,
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            32
        }
        KittyKey::Char(ch) => {
            // Plain Shift + printable character sends the shifted character normally in all modes.
            if !ev.modifiers.ctrl && !ev.modifiers.alt {
                return false;
            }
            let applies = match level {
                ModifyOtherKeys::Disabled => false,
                ModifyOtherKeys::Enabled => {
                    if !ev.modifiers.ctrl {
                        false
                    } else if ev.modifiers.shift && ch.is_ascii_alphabetic() {
                        true
                    } else {
                        !has_legacy_ctrl_mapping(ch)
                    }
                }
                ModifyOtherKeys::Always => true,
            };
            if !applies {
                return false;
            }
            let effective_ch = if ev.modifiers.shift {
                ev.shifted_key.unwrap_or_else(|| ch.to_ascii_uppercase())
            } else {
                ch
            };
            u32::from(effective_ch)
        }
        _ => return false,
    };

    out.extend_from_slice(b"\x1b[27;");
    push_u32_dec(out, mod_param);
    out.push(b';');
    push_u32_dec(out, keycode);
    out.push(b'~');
    true
}

const fn has_legacy_ctrl_mapping(ch: char) -> bool {
    let lower = ch.to_ascii_lowercase();
    matches!(lower, '@' | 'a'..='z' | '[' | '\\' | ']' | '^' | '_')
}

fn encode_legacy_into(ev: &KittyKeyEvent<'_>, out: &mut Vec<u8>) -> bool {
    if ev.event_type == KittyEventType::Release {
        return false;
    }
    let mod_param = 1 + u16::from(ev.modifiers.bits() & 0x3F);
    let has_any_mod = ev.modifiers.has_active_modifiers();

    let legacy_csi = special_csi_key(ev.key).or_else(|| legacy_keypad_csi_key(ev.key));
    if let Some((prefix_num, suffix_ch)) = legacy_csi {
        if !has_any_mod {
            match prefix_num {
                None => {
                    out.extend_from_slice(b"\x1b[");
                    out.push(suffix_ch as u8);
                }
                Some(_) if suffix_ch != '~' => {
                    out.extend_from_slice(b"\x1bO");
                    out.push(suffix_ch as u8);
                }
                Some(n) => {
                    out.extend_from_slice(b"\x1b[");
                    push_u32_dec(out, u32::from(n));
                    out.push(b'~');
                }
            }
            return true;
        }
        let lead = prefix_num.unwrap_or(1);
        out.extend_from_slice(b"\x1b[");
        push_u32_dec(out, u32::from(lead));
        out.push(b';');
        push_u32_dec(out, u32::from(mod_param));
        out.push(suffix_ch as u8);
        return true;
    }

    match ev.key {
        KittyKey::Enter | KittyKey::KpEnter => {
            if ev.modifiers.alt {
                out.push(0x1B);
            }
            out.push(b'\r');
        }
        KittyKey::Tab => {
            if ev.modifiers.shift && !ev.modifiers.ctrl && !ev.modifiers.alt {
                out.extend_from_slice(b"\x1b[Z");
                return true;
            }
            if ev.modifiers.alt {
                out.push(0x1B);
            }
            out.push(b'\t');
        }
        KittyKey::Backspace => {
            if ev.modifiers.alt {
                out.push(0x1B);
            }
            out.push(if ev.modifiers.ctrl { 0x08 } else { 0x7F });
        }
        KittyKey::Escape => {
            if ev.modifiers.alt {
                out.push(0x1B);
            }
            out.push(0x1B);
        }
        KittyKey::Space => {
            if ev.modifiers.alt {
                out.push(0x1B);
            }
            out.push(if ev.modifiers.ctrl { 0x00 } else { b' ' });
        }
        KittyKey::Kp0 => push_legacy_ascii_key(out, ev.modifiers.alt, b'0'),
        KittyKey::Kp1 => push_legacy_ascii_key(out, ev.modifiers.alt, b'1'),
        KittyKey::Kp2 => push_legacy_ascii_key(out, ev.modifiers.alt, b'2'),
        KittyKey::Kp3 => push_legacy_ascii_key(out, ev.modifiers.alt, b'3'),
        KittyKey::Kp4 => push_legacy_ascii_key(out, ev.modifiers.alt, b'4'),
        KittyKey::Kp5 => push_legacy_ascii_key(out, ev.modifiers.alt, b'5'),
        KittyKey::Kp6 => push_legacy_ascii_key(out, ev.modifiers.alt, b'6'),
        KittyKey::Kp7 => push_legacy_ascii_key(out, ev.modifiers.alt, b'7'),
        KittyKey::Kp8 => push_legacy_ascii_key(out, ev.modifiers.alt, b'8'),
        KittyKey::Kp9 => push_legacy_ascii_key(out, ev.modifiers.alt, b'9'),
        KittyKey::KpDecimal => push_legacy_ascii_key(out, ev.modifiers.alt, b'.'),
        KittyKey::KpDivide => push_legacy_ascii_key(out, ev.modifiers.alt, b'/'),
        KittyKey::KpMultiply => push_legacy_ascii_key(out, ev.modifiers.alt, b'*'),
        KittyKey::KpSubtract => push_legacy_ascii_key(out, ev.modifiers.alt, b'-'),
        KittyKey::KpAdd => push_legacy_ascii_key(out, ev.modifiers.alt, b'+'),
        KittyKey::KpEqual => push_legacy_ascii_key(out, ev.modifiers.alt, b'='),
        KittyKey::KpSeparator => push_legacy_ascii_key(out, ev.modifiers.alt, b','),
        KittyKey::Char(ch) => {
            if ev.modifiers.ctrl {
                let lower = ch.to_ascii_lowercase();
                let ctrl_byte = match lower {
                    '@' => 0x00,
                    'a'..='z' => (lower as u8) - b'a' + 1,
                    '[' => 0x1B,
                    '\\' => 0x1C,
                    ']' => 0x1D,
                    '^' => 0x1E,
                    '_' => 0x1F,
                    _ => return false,
                };
                if ev.modifiers.alt {
                    out.push(0x1B);
                }
                out.push(ctrl_byte);
            } else {
                if ev.modifiers.alt {
                    out.push(0x1B);
                }
                let out_ch = if ev.modifiers.shift {
                    ev.shifted_key.unwrap_or_else(|| ch.to_ascii_uppercase())
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                out.extend_from_slice(out_ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        _ => return false,
    }
    true
}

#[inline]
fn push_legacy_ascii_key(out: &mut Vec<u8>, alt: bool, byte: u8) {
    if alt {
        out.push(0x1B);
    }
    out.push(byte);
}

const fn legacy_keypad_csi_key(key: KittyKey) -> Option<(Option<u16>, char)> {
    match key {
        KittyKey::KpUp => Some((None, 'A')),
        KittyKey::KpDown => Some((None, 'B')),
        KittyKey::KpRight => Some((None, 'C')),
        KittyKey::KpLeft => Some((None, 'D')),
        KittyKey::KpBegin => Some((None, 'E')),
        KittyKey::KpEnd => Some((None, 'F')),
        KittyKey::KpHome => Some((None, 'H')),
        KittyKey::KpInsert => Some((Some(2), '~')),
        KittyKey::KpDelete => Some((Some(3), '~')),
        KittyKey::KpPageUp => Some((Some(5), '~')),
        KittyKey::KpPageDown => Some((Some(6), '~')),
        _ => None,
    }
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
        KittyKey::Kp0 => Some(57399),
        KittyKey::Kp1 => Some(57400),
        KittyKey::Kp2 => Some(57401),
        KittyKey::Kp3 => Some(57402),
        KittyKey::Kp4 => Some(57403),
        KittyKey::Kp5 => Some(57404),
        KittyKey::Kp6 => Some(57405),
        KittyKey::Kp7 => Some(57406),
        KittyKey::Kp8 => Some(57407),
        KittyKey::Kp9 => Some(57408),
        KittyKey::KpDecimal => Some(57409),
        KittyKey::KpDivide => Some(57410),
        KittyKey::KpMultiply => Some(57411),
        KittyKey::KpSubtract => Some(57412),
        KittyKey::KpAdd => Some(57413),
        KittyKey::KpEnter => Some(57414),
        KittyKey::KpEqual => Some(57415),
        KittyKey::KpSeparator => Some(57416),
        KittyKey::KpLeft => Some(57417),
        KittyKey::KpRight => Some(57418),
        KittyKey::KpUp => Some(57419),
        KittyKey::KpDown => Some(57420),
        KittyKey::KpPageUp => Some(57421),
        KittyKey::KpPageDown => Some(57422),
        KittyKey::KpHome => Some(57423),
        KittyKey::KpEnd => Some(57424),
        KittyKey::KpInsert => Some(57425),
        KittyKey::KpDelete => Some(57426),
        KittyKey::KpBegin => Some(57427),
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

    #[test]
    fn encode_kitty_key_into_reuses_buffer_and_handles_keypad_keys() {
        let mut out = Vec::from(b"prefix:".as_slice());
        let invalid = KittyKeyEvent {
            key: KittyKey::Char('1'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                alt: true,
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(!encode_kitty_key_into(
            KittyKeyboardFlags::empty(),
            &invalid,
            &mut out
        ));
        assert_eq!(out, b"prefix:");

        out.clear();
        let kp_enter = KittyKeyEvent {
            key: KittyKey::KpEnter,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers::default(),
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(encode_kitty_key_into(
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES,
            &kp_enter,
            &mut out
        ));
        assert_eq!(out, b"\x1b[57414u");

        out.clear();
        assert!(encode_kitty_key_into(
            KittyKeyboardFlags::empty(),
            &kp_enter,
            &mut out
        ));
        assert_eq!(out, b"\r");

        let kp_cases = [
            (KittyKey::Kp0, b"\x1b[57399u".as_slice(), b"0".as_slice()),
            (KittyKey::Kp9, b"\x1b[57408u".as_slice(), b"9".as_slice()),
            (
                KittyKey::KpDecimal,
                b"\x1b[57409u".as_slice(),
                b".".as_slice(),
            ),
            (
                KittyKey::KpDivide,
                b"\x1b[57410u".as_slice(),
                b"/".as_slice(),
            ),
            (
                KittyKey::KpMultiply,
                b"\x1b[57411u".as_slice(),
                b"*".as_slice(),
            ),
            (
                KittyKey::KpSubtract,
                b"\x1b[57412u".as_slice(),
                b"-".as_slice(),
            ),
            (KittyKey::KpAdd, b"\x1b[57413u".as_slice(), b"+".as_slice()),
            (
                KittyKey::KpEqual,
                b"\x1b[57415u".as_slice(),
                b"=".as_slice(),
            ),
            (
                KittyKey::KpSeparator,
                b"\x1b[57416u".as_slice(),
                b",".as_slice(),
            ),
            (
                KittyKey::KpBegin,
                b"\x1b[57427u".as_slice(),
                b"\x1b[E".as_slice(),
            ),
        ];
        for (key, kitty_bytes, legacy_bytes) in kp_cases {
            let ev = KittyKeyEvent {
                key,
                event_type: KittyEventType::Press,
                modifiers: KittyModifiers::default(),
                shifted_key: None,
                base_layout_key: None,
                associated_text: None,
            };
            out.clear();
            assert!(encode_kitty_key_into(
                KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES,
                &ev,
                &mut out
            ));
            assert_eq!(out, kitty_bytes);

            out.clear();
            assert!(encode_kitty_key_into(
                KittyKeyboardFlags::empty(),
                &ev,
                &mut out
            ));
            assert_eq!(out, legacy_bytes);
        }

        // Multi-codepoint associated text + base layout key without heap allocs.
        let flags = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
            | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS
            | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            | KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
        let multi = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..KittyModifiers::default()
            },
            shifted_key: Some('A'),
            base_layout_key: Some('q'),
            associated_text: Some("A\x00B"),
        };
        out.clear();
        assert!(encode_kitty_key_into(flags, &multi, &mut out));
        assert_eq!(out, b"\x1b[97:65:113;2;65:66u");
    }

    #[test]
    fn modify_other_keys_mode_1_and_2_behavior() {
        assert_eq!(ModifyOtherKeys::from_pv(0), Some(ModifyOtherKeys::Disabled));
        assert_eq!(ModifyOtherKeys::from_pv(1), Some(ModifyOtherKeys::Enabled));
        assert_eq!(ModifyOtherKeys::from_pv(2), Some(ModifyOtherKeys::Always));
        assert_eq!(ModifyOtherKeys::from_pv(3), None);

        let mut out = Vec::new();
        let ctrl_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(!encode_modify_other_keys_into(
            ModifyOtherKeys::Disabled,
            &ctrl_a,
            &mut out
        ));
        assert!(!encode_modify_other_keys_into(
            ModifyOtherKeys::Enabled,
            &ctrl_a,
            &mut out
        ));
        assert!(encode_modify_other_keys_into(
            ModifyOtherKeys::Always,
            &ctrl_a,
            &mut out
        ));
        assert_eq!(out, b"\x1b[27;5;97~");

        // Plain Shift+Char never emits CSI 27;...~ even in Always (mode 2).
        out.clear();
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
        assert!(!encode_modify_other_keys_into(
            ModifyOtherKeys::Always,
            &shift_a,
            &mut out
        ));
        assert!(out.is_empty());

        // Mode 1 encodes Shift+Enter, Ctrl+1, Ctrl+Shift+A, Ctrl+Tab, but skips Alt+Enter.
        let shift_enter = KittyKeyEvent {
            key: KittyKey::Enter,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(encode_modify_other_keys_into(
            ModifyOtherKeys::Enabled,
            &shift_enter,
            &mut out
        ));
        assert_eq!(out, b"\x1b[27;2;13~");

        out.clear();
        let ctrl_1 = KittyKeyEvent {
            key: KittyKey::Char('1'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(encode_modify_other_keys_into(
            ModifyOtherKeys::Enabled,
            &ctrl_1,
            &mut out
        ));
        assert_eq!(out, b"\x1b[27;5;49~");

        out.clear();
        let ctrl_shift_a = KittyKeyEvent {
            key: KittyKey::Char('a'),
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                shift: true,
                ctrl: true,
                ..KittyModifiers::default()
            },
            shifted_key: Some('A'),
            base_layout_key: None,
            associated_text: None,
        };
        assert!(encode_modify_other_keys_into(
            ModifyOtherKeys::Enabled,
            &ctrl_shift_a,
            &mut out
        ));
        assert_eq!(out, b"\x1b[27;6;65~");

        out.clear();
        let alt_enter = KittyKeyEvent {
            key: KittyKey::Enter,
            event_type: KittyEventType::Press,
            modifiers: KittyModifiers {
                alt: true,
                ..KittyModifiers::default()
            },
            shifted_key: None,
            base_layout_key: None,
            associated_text: None,
        };
        assert!(!encode_modify_other_keys_into(
            ModifyOtherKeys::Enabled,
            &alt_enter,
            &mut out
        ));
        assert!(encode_modify_other_keys_into(
            ModifyOtherKeys::Always,
            &alt_enter,
            &mut out
        ));
        assert_eq!(out, b"\x1b[27;3;13~");
    }
}
