//! Clean-room, WASM-safe interactive shell simulator implementing [`TerminalInput`].
//!
//! Used by `egui_xterm_demo` on all targets (including `wasm32-unknown-unknown` and
//! `--no-default-features`) to demonstrate trait-driven VT output, line editing,
//! command history, resize tracking, and modern terminal showcase sequences.

use std::collections::VecDeque;
use std::fmt::Write as _;

use egui_xterm::{TerminalInput, TerminalIoError, TerminalSize};

/// Maximum number of entered commands retained in [`DemoShellInput`] history.
const MAX_HISTORY_ENTRIES: usize = 64;

/// Built-in showcase command descriptors `(name, summary)` exposed for the demo sidebar.
pub const SHOWCASE_COMMANDS: &[(&str, &str)] = &[
    ("help", "List commands & terminal shortcuts"),
    ("colors", "16/256/TrueColor gradients & SGR styles"),
    ("unicode", "CJK, ZWJ emoji & vector box/braille glyphs"),
    ("links", "OSC 8 hyperlinks & implicit URLs"),
    ("prompts", "OSC 133 semantic prompt zones & exit codes"),
    ("sync", "DEC Mode 2026 synchronized frame update"),
    ("size", "Show latest debounced TerminalSize"),
    ("scroll", "Emit 120 lines for scrollback & search"),
    ("clear", "Clear screen & reset cursor"),
];

/// In-memory interactive console implementing [`TerminalInput`] for all targets.
#[derive(Debug)]
pub struct DemoShellInput {
    rx_buf: VecDeque<u8>,
    utf8_pending: Vec<u8>,
    esc_state: EscapeParseState,
    line_buf: String,
    history: Vec<String>,
    history_cursor: Option<usize>,
    command_seq: u64,
    last_size: TerminalSize,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeParseState {
    Ground,
    Esc,
    Csi,
    Ss3,
}

impl Default for DemoShellInput {
    fn default() -> Self {
        Self::new("egui_xterm Trait Shell")
    }
}

impl DemoShellInput {
    /// Creates a new interactive [`DemoShellInput`] seeded with a welcome banner and prompt.
    #[must_use]
    pub fn new(title: &str) -> Self {
        let mut shell = Self {
            rx_buf: VecDeque::new(),
            utf8_pending: Vec::new(),
            esc_state: EscapeParseState::Ground,
            line_buf: String::new(),
            history: Vec::new(),
            history_cursor: None,
            command_seq: 0,
            last_size: TerminalSize::default(),
            closed: false,
        };
        shell.push_welcome(title);
        shell
    }

    /// Programmatically submits a showcase command as if the user typed and executed it.
    pub fn run_showcase(&mut self, command: &str) {
        if self.closed {
            return;
        }
        self.clear_current_input();
        self.push_str(command);
        self.line_buf.push_str(command);
        self.execute_current_line();
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        self.rx_buf.extend(bytes);
    }

    fn push_str(&mut self, text: &str) {
        self.push_bytes(text.as_bytes());
    }

    fn push_welcome(&mut self, title: &str) {
        let mut out = String::new();
        // OSC 0 window title + OSC 7 initial working directory.
        if write!(
            out,
            "\x1b]0;{title}\x07\x1b]7;file://localhost/workspace/demo\x07"
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        out.push_str(
            "\x1b[1;36megui_xterm\x1b[0m — Clean-Room Trait-Driven Console (`TerminalInput`)\r\n",
        );
        out.push_str(
            "Type \x1b[33mhelp\x1b[0m or click a showcase command in the right panel.\r\n",
        );
        self.push_str(&out);
        self.push_prompt(0);
    }

    fn push_prompt(&mut self, last_exit_code: i32) {
        self.command_seq = self.command_seq.saturating_add(1);
        let mut out = String::new();
        // Emit OSC 133;D (if previous command finished) -> OSC 133;A -> prompt -> OSC 133;B.
        if self.command_seq > 1 && write!(out, "\x1b]133;D;{last_exit_code}\x07").is_err() {
            // `String`'s `fmt::Write` impl is infallible.
        }
        out.push_str("\x1b]133;A\x07");
        if last_exit_code == 0 {
            out.push_str("\x1b[1;32mdemo\x1b[0m \x1b[34m~/egui_xterm\x1b[0m \x1b[1;35m>\x1b[0m ");
        } else if write!(
            out,
            "\x1b[1;31m[{last_exit_code}]\x1b[0m \x1b[1;32mdemo\x1b[0m \x1b[34m~/egui_xterm\x1b[0m \x1b[1;31m>\x1b[0m "
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        out.push_str("\x1b]133;B\x07");
        self.push_str(&out);
    }

    fn process_incoming_byte(&mut self, byte: u8) {
        match self.esc_state {
            EscapeParseState::Ground => match byte {
                0x1b => {
                    self.utf8_pending.clear();
                    self.esc_state = EscapeParseState::Esc;
                }
                b'\r' | b'\n' => {
                    self.utf8_pending.clear();
                    self.execute_current_line();
                }
                0x08 | 0x7f => {
                    self.utf8_pending.clear();
                    self.handle_backspace();
                }
                0x03 => {
                    self.utf8_pending.clear();
                    self.line_buf.clear();
                    self.history_cursor = None;
                    self.push_str("^C\r\n");
                    self.push_prompt(130);
                }
                0x0c => {
                    self.utf8_pending.clear();
                    self.line_buf.clear();
                    self.history_cursor = None;
                    self.push_str("\x1b[2J\x1b[H");
                    self.push_prompt(0);
                }
                b if b < 0x20 => {
                    // Ignore unhandled ASCII control bytes.
                }
                b => {
                    self.utf8_pending.push(b);
                    self.flush_utf8_pending();
                }
            },
            EscapeParseState::Esc => {
                self.esc_state = match byte {
                    b'[' => EscapeParseState::Csi,
                    b'O' => EscapeParseState::Ss3,
                    _ => EscapeParseState::Ground,
                };
            }
            EscapeParseState::Csi => {
                if byte == 0x1b {
                    self.esc_state = EscapeParseState::Esc;
                } else if (0x40..=0x7e).contains(&byte) {
                    match byte {
                        b'A' => self.recall_history(-1),
                        b'B' => self.recall_history(1),
                        _ => {}
                    }
                    self.esc_state = EscapeParseState::Ground;
                }
            }
            EscapeParseState::Ss3 => {
                if byte == 0x1b {
                    self.esc_state = EscapeParseState::Esc;
                } else {
                    match byte {
                        b'A' => self.recall_history(-1),
                        b'B' => self.recall_history(1),
                        _ => {}
                    }
                    self.esc_state = EscapeParseState::Ground;
                }
            }
        }
    }

    fn flush_utf8_pending(&mut self) {
        while !self.utf8_pending.is_empty() {
            match std::str::from_utf8(&self.utf8_pending) {
                Ok(valid) => {
                    let echo = valid.to_owned();
                    self.line_buf.push_str(&echo);
                    self.utf8_pending.clear();
                    self.history_cursor = None;
                    self.push_str(&echo);
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        let valid =
                            String::from_utf8_lossy(&self.utf8_pending[..valid_up_to]).into_owned();
                        self.line_buf.push_str(&valid);
                        self.push_str(&valid);
                        self.history_cursor = None;
                    }
                    if let Some(invalid_len) = err.error_len() {
                        let drop_end = (valid_up_to + invalid_len).min(self.utf8_pending.len());
                        self.utf8_pending.drain(..drop_end.max(1));
                    } else {
                        if valid_up_to > 0 {
                            self.utf8_pending.drain(..valid_up_to);
                        }
                        break;
                    }
                }
            }
        }
    }

    fn handle_backspace(&mut self) {
        if self.line_buf.is_empty() {
            return;
        }
        let width = pop_last_grapheme_width(&mut self.line_buf);
        for _ in 0..width {
            self.push_str("\x08 \x08");
        }
        self.history_cursor = None;
    }

    fn clear_current_input(&mut self) {
        while !self.line_buf.is_empty() {
            self.handle_backspace();
        }
    }

    fn recall_history(&mut self, delta: isize) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match (self.history_cursor, delta) {
            (None, -1) => Some(self.history.len() - 1),
            (Some(idx), -1) => Some(idx.saturating_sub(1)),
            (Some(idx), 1) => (idx + 1 < self.history.len()).then_some(idx + 1),
            (Some(idx), _) => Some(idx),
            (None, _) => None,
        };
        self.clear_current_input();
        self.history_cursor = next_idx;
        if let Some(idx) = next_idx {
            let cmd = self.history[idx].clone();
            self.line_buf.push_str(&cmd);
            self.push_str(&cmd);
        }
    }

    fn execute_current_line(&mut self) {
        let cmd = self.line_buf.trim().to_owned();
        self.line_buf.clear();
        self.history_cursor = None;

        self.push_str("\r\n\x1b]133;C\x07");

        if !cmd.is_empty() && self.history.last().is_none_or(|prev| prev != &cmd) {
            self.history.push(cmd.clone());
            if self.history.len() > MAX_HISTORY_ENTRIES {
                self.history.remove(0);
            }
        }

        let exit_code = match cmd.as_str() {
            "" => 0,
            "help" => {
                self.emit_help();
                0
            }
            "colors" => {
                self.emit_colors();
                0
            }
            "unicode" => {
                self.emit_unicode();
                0
            }
            "links" => {
                self.emit_links();
                0
            }
            "prompts" => {
                self.emit_prompts();
                0
            }
            "sync" => {
                self.emit_sync();
                0
            }
            "size" => {
                self.emit_size();
                0
            }
            "scroll" => {
                self.emit_scroll();
                0
            }
            "clear" => {
                self.push_str("\x1b[2J\x1b[H");
                0
            }
            other => {
                let mut msg = String::new();
                if write!(
                    msg,
                    "\x1b[33mecho:\x1b[0m {other}\r\n\x1b[2m(type 'help' for built-in showcase commands)\x1b[0m\r\n"
                )
                .is_err()
                {
                    // `String`'s `fmt::Write` impl is infallible.
                }
                self.push_str(&msg);
                0
            }
        };

        self.push_prompt(exit_code);
    }

    fn emit_help(&mut self) {
        let mut out = String::from("\x1b[1;36mBuilt-in Showcase Commands:\x1b[0m\r\n");
        for (name, desc) in SHOWCASE_COMMANDS {
            if write!(out, "  \x1b[1;33m{name:<9}\x1b[0m {desc}\r\n").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
        out.push_str(
            "\x1b[1;36mShortcuts:\x1b[0m Ctrl+Shift+C copy | Ctrl+Shift+V paste | Ctrl+Shift+F find | Shift+Drag select\r\n",
        );
        self.push_str(&out);
    }

    fn emit_colors(&mut self) {
        let mut out = String::from("\x1b[1mANSI 16:\x1b[0m ");
        for code in 0..8 {
            if write!(out, "\x1b[4{code}m  \x1b[0m").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
        out.push(' ');
        for code in 0..8 {
            if write!(out, "\x1b[10{code}m  \x1b[0m").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
        out.push_str("\r\n\x1b[1mTrueColor:\x1b[0m ");
        for step in 0..48u8 {
            let r = step.saturating_mul(5);
            let g = 255u8.saturating_sub(r);
            let b = 128u8.wrapping_add(step.saturating_mul(2));
            if write!(out, "\x1b[48;2;{r};{g};{b}m \x1b[0m").is_err() {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
        out.push_str(
            "\r\n\x1b[1mSGR:\x1b[0m \x1b[1mBold\x1b[0m \x1b[2mDim\x1b[0m \x1b[3mItalic\x1b[0m \
             \x1b[4:1mSingle\x1b[0m \x1b[4:2mDouble\x1b[0m \x1b[4:3;58:2::255:90:90mCurly\x1b[0m \
             \x1b[4:4mDotted\x1b[0m \x1b[4:5mDashed\x1b[0m \x1b[9mStrike\x1b[0m \x1b[53mOverline\x1b[0m\r\n",
        );
        self.push_str(&out);
    }

    fn emit_unicode(&mut self) {
        let out = concat!(
            "\x1b[1mCJK & Wide:\x1b[0m 你好世界  日本語テスト  한국어 테스트\r\n",
            "\x1b[1mEmoji/ZWJ:\x1b[0m  🚀 🔥 ✨  👨‍👩‍👧‍👦  🏳️‍🌈  👩‍💻\r\n",
            "\x1b[1mVector:\x1b[0m     ┌─────────┐  ░▒▓█▓▒░  ⣿⡇⣆⣀\r\n",
            "            │ \x1b[32mvector\x1b[0m  │  ▀▄▀▄▀▄▀  ⣶⣷⣿⣿\r\n",
            "            └─────────┘\r\n",
        );
        self.push_str(out);
    }

    fn emit_links(&mut self) {
        let out = "\
\x1b[1mOSC 8 Link:\x1b[0m \x1b]8;;https://github.com/AnlangA/egui_xterm\x07\x1b[36;4megui_xterm on GitHub\x1b[0m\x1b]8;;\x07\r\n\
\x1b[1mPlain URL:\x1b[0m  https://egui.rs\r\n";
        self.push_str(out);
    }

    fn emit_prompts(&mut self) {
        let out = "\
\x1b[2mEmitting OSC 133 semantic command blocks:\x1b[0m\r\n\
\x1b]133;A\x07\x1b[32m$ cargo test --workspace\x1b[0m\x1b]133;B\x07\r\n\
\x1b]133;C\x07test result: ok. 120 passed; 0 failed\r\n\
\x1b]133;D;0\x07\
\x1b]133;A\x07\x1b[31m$ false\x1b[0m\x1b]133;B\x07\r\n\
\x1b]133;C\x07command exited with status 2\r\n\
\x1b]133;D;2\x07";
        self.push_str(out);
    }

    fn emit_sync(&mut self) {
        let mut out = String::from("\x1b[?2026h");
        out.push_str("\x1b[1;35m[DEC Mode 2026 Synchronized Frame]\x1b[0m\r\n");
        out.push_str("┌──────┬──────────┬──────────┐\r\n");
        out.push_str("│ Pane │ Status   │ Mode     │\r\n");
        out.push_str("├──────┼──────────┼──────────┤\r\n");
        out.push_str("│ #1   │ \x1b[32mSmooth\x1b[0m   │ Reflow   │\r\n");
        out.push_str("│ #2   │ \x1b[36mLatched\x1b[0m  │ Sync2026 │\r\n");
        out.push_str("└──────┴──────────┴──────────┘\r\n");
        out.push_str("\x1b[?2026l");
        self.push_str(&out);
    }

    fn emit_size(&mut self) {
        let sz = self.last_size;
        let mut out = String::new();
        if writeln!(
            out,
            "Latest TerminalSize: {}x{} cols/rows ({}x{} px)\r",
            sz.cols, sz.rows, sz.pixel_width, sz.pixel_height
        )
        .is_err()
        {
            // `String`'s `fmt::Write` impl is infallible.
        }
        self.push_str(&out);
    }

    fn emit_scroll(&mut self) {
        let mut out = String::new();
        for idx in 1..=120u32 {
            if write!(
                out,
                "[{idx:03}/120] Scrollback line #{idx} — egui_xterm ring buffer & search target\r\n"
            )
            .is_err()
            {
                // `String`'s `fmt::Write` impl is infallible.
            }
        }
        self.push_str(&out);
    }
}

fn is_zero_width_scalar(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0900..=0x0902
            | 0x093C
            | 0x0941..=0x0948
            | 0x094D
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x1160..=0x11FF
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x20D0..=0x20FF
            | 0x3099..=0x309A
            | 0xD7B0..=0xD7FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0xFEFF
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

fn is_skin_tone_modifier(ch: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&ch)
}

fn is_regional_indicator(ch: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)
}

fn pop_last_grapheme_width(line_buf: &mut String) -> usize {
    let mut cluster_width = 0usize;
    let mut has_vs16 = false;
    let mut has_base = false;

    while let Some(ch) = line_buf.pop() {
        if is_zero_width_scalar(ch) {
            has_vs16 |= ch == '\u{FE0F}';
            continue;
        }
        if is_skin_tone_modifier(ch)
            && line_buf
                .chars()
                .next_back()
                .is_some_and(|prev| !prev.is_ascii() && !is_zero_width_scalar(prev))
        {
            continue;
        }

        has_base = true;
        cluster_width = cluster_width.max(unicode_width_approx(ch));

        if line_buf.ends_with('\u{200D}') {
            line_buf.pop(); // strip trailing ZWJ so the joined cluster replaces it
            cluster_width = 2;
            continue;
        }

        if is_regional_indicator(ch) {
            let trailing_ri = line_buf
                .chars()
                .rev()
                .take_while(|c| is_regional_indicator(*c))
                .count();
            if trailing_ri % 2 == 1 {
                line_buf.pop(); // pair two regional indicators into one flag cluster
                cluster_width = 2;
            }
        }
        break;
    }

    if !has_base {
        return 0;
    }
    if has_vs16 {
        cluster_width = 2;
    }
    cluster_width.clamp(1, 2)
}

fn unicode_width_approx(ch: char) -> usize {
    if is_zero_width_scalar(ch) {
        0
    } else if ch.is_ascii() {
        1
    } else if matches!(
        ch as u32,
        0x1100..=0x115F
            | 0x2329..=0x232A
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    ) {
        2
    } else {
        1
    }
}

impl TerminalInput for DemoShellInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        if buf.is_empty() {
            return if self.is_closed() {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }
        if self.rx_buf.is_empty() {
            return if self.closed {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }

        let (first, second) = self.rx_buf.as_slices();
        let n1 = first.len().min(buf.len());
        buf[..n1].copy_from_slice(&first[..n1]);
        let rem = buf.len() - n1;
        let n2 = if rem > 0 {
            let count = second.len().min(rem);
            buf[n1..n1 + count].copy_from_slice(&second[..count]);
            count
        } else {
            0
        };
        let total = n1 + n2;
        self.rx_buf.drain(..total);
        Ok(total)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        if self.closed {
            return Err(TerminalIoError::Closed);
        }
        for &byte in bytes {
            self.process_incoming_byte(byte);
        }
        Ok(())
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        self.last_size = size;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed && self.rx_buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use egui_xterm::{CoreConfig, Terminal};

    use super::*;

    fn drain_bytes(shell: &mut DemoShellInput) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            match shell.poll_read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        out
    }

    fn drain_all(shell: &mut DemoShellInput) -> String {
        String::from_utf8_lossy(&drain_bytes(shell)).into_owned()
    }

    #[test]
    fn welcome_echo_backspace_and_history() {
        let mut shell = DemoShellInput::new("Test Shell");
        let banner = drain_all(&mut shell);
        assert!(banner.contains("egui_xterm"));

        shell.write_bytes(b"helx\x7fp\r").expect("write help");
        let help_out = drain_all(&mut shell);
        assert!(help_out.contains("Built-in Showcase Commands"));

        // Up arrow recalls `help`
        shell.write_bytes(b"\x1b[A").expect("up arrow");
        let recalled = drain_all(&mut shell);
        assert!(recalled.contains("help"));
    }

    #[test]
    fn showcase_commands_and_resize_tracking() {
        let mut shell = DemoShellInput::default();
        let _ = drain_all(&mut shell);

        let size = TerminalSize::with_pixels(96, 32, 768, 512);
        shell.resize(size).expect("resize");
        assert_eq!(shell.last_size, size);

        shell.run_showcase("size");
        let out = drain_all(&mut shell);
        assert!(out.contains("96x32 cols/rows"));

        shell.run_showcase("colors");
        let colors = drain_all(&mut shell);
        assert!(colors.contains("TrueColor"));
    }

    #[test]
    fn backspace_erases_combining_mark_and_zwj_emoji_clusters() {
        let mut shell = DemoShellInput::new("Cluster Test");
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(&drain_bytes(&mut shell));

        // Combining acute accent `e\u{0301}` + 1x Backspace + Enter must leave no residual 'e'.
        shell
            .write_bytes("e\u{0301}\x7f\r".as_bytes())
            .expect("combining + bs");
        let bytes = drain_bytes(&mut shell);
        term.advance(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("echo:"), "residual output leaked: {text:?}");
        assert_eq!(
            term.visible_line_text(3).as_deref(),
            Some("demo ~/egui_xterm >")
        );

        // ZWJ family emoji + 1x Backspace + Enter must erase the whole cluster cleanly.
        shell
            .write_bytes("a👨\u{200D}👩\u{200D}👧\u{200D}👦\x7f\r".as_bytes())
            .expect("zwj + bs");
        let bytes = drain_bytes(&mut shell);
        term.advance(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("echo:\x1b[0m a\r\n"),
            "expected only 'a' to remain after single BS on ZWJ emoji, got: {text:?}"
        );
        assert!(
            term.full_text().contains("echo: a"),
            "terminal grid did not render 'echo: a': {:?}",
            term.full_text()
        );

        // Skin-tone ZWJ emoji + 1x Backspace leaves line empty.
        shell
            .write_bytes("👩\u{1F3FD}\u{200D}💻\x7f\r".as_bytes())
            .expect("skin tone zwj + bs");
        let text = drain_all(&mut shell);
        assert!(!text.contains("echo:"), "residual output leaked: {text:?}");

        // Regional indicator flag pair + 1x Backspace leaves line empty.
        shell.write_bytes("🇺🇸\x7f\r".as_bytes()).expect("flag + bs");
        let text = drain_all(&mut shell);
        assert!(!text.contains("echo:"), "residual output leaked: {text:?}");
    }

    #[test]
    fn malformed_utf8_preserves_valid_trailing_bytes() {
        let mut shell = DemoShellInput::new("UTF-8 Recovery");
        let _ = drain_all(&mut shell);

        // Truncated 3-byte UTF-8 [0xE4, 0xBD] followed by ASCII 'A'.
        shell
            .write_bytes(&[0xE4, 0xBD, b'A', b'\r'])
            .expect("write");
        let out1 = drain_all(&mut shell);
        assert!(
            out1.contains("echo:\x1b[0m A\r\n"),
            "valid trailing 'A' was dropped: {out1:?}"
        );

        // Stale lead byte 0xE4 followed by valid 3-byte UTF-8 "你" ([0xE4, 0xBD, 0xA0]).
        shell
            .write_bytes(&[0xE4, 0xE4, 0xBD, 0xA0, b'\r'])
            .expect("write");
        let out2 = drain_all(&mut shell);
        assert!(
            out2.contains("echo:\x1b[0m 你\r\n"),
            "valid trailing '你' was dropped: {out2:?}"
        );

        // Invalid byte in the middle of ASCII preserves both sides.
        shell
            .write_bytes(&[b'x', 0xFF, b'y', b'\r'])
            .expect("write");
        let out3 = drain_all(&mut shell);
        assert!(
            out3.contains("echo:\x1b[0m xy\r\n"),
            "surrounding ASCII corrupted: {out3:?}"
        );
    }

    #[test]
    fn ss3_sequences_do_not_leak_final_byte_into_line_buf() {
        let mut shell = DemoShellInput::new("SS3 Test");
        let _ = drain_all(&mut shell);

        shell.write_bytes(b"size\r").expect("run size");
        let _ = drain_all(&mut shell);

        // SS3 Up (`\x1bOA`) recalls `size` without leaking `'A'`; SS3 Right (`\x1bOC`) is ignored.
        shell
            .write_bytes(b"\x1bOA\x1bOC\r")
            .expect("ss3 up + right + enter");
        let out = drain_all(&mut shell);
        assert!(
            out.contains("Latest TerminalSize:"),
            "expected recalled `size` execution without leaked 'A'/'C', got: {out:?}"
        );
        assert!(!out.contains("echo:"));
    }

    #[test]
    fn unicode_showcase_vector_box_rows_align_at_column_12() {
        let mut shell = DemoShellInput::new("Unicode Box Test");
        let mut term = Terminal::new(80, 24, CoreConfig::default());
        term.advance(&drain_bytes(&mut shell));

        shell.run_showcase("unicode");
        term.advance(&drain_bytes(&mut shell));

        let lines = term.visible_lines();
        let top = lines
            .iter()
            .find(|l| l.contains("┌─────────┐"))
            .expect("top border row");
        let mid = lines
            .iter()
            .find(|l| l.contains("vector"))
            .expect("middle vector row");
        let bot = lines
            .iter()
            .find(|l| l.contains("└─────────┘"))
            .expect("bottom border row");

        let char_col = |line: &str, ch: char| {
            line.chars()
                .position(|c| c == ch)
                .unwrap_or_else(|| panic!("missing {ch:?} in {line:?}"))
        };
        assert_eq!(char_col(top, '┌'), 12, "top row: {top:?}");
        assert_eq!(char_col(mid, '│'), 12, "mid row: {mid:?}");
        assert_eq!(char_col(bot, '└'), 12, "bot row: {bot:?}");
    }
}
