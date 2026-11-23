//! Cross-platform clipboard integration for `egui_xterm`.
//!
//! - On native desktop targets with the `native-clipboard` feature enabled, reads and writes use
//!   `arboard::Clipboard` synchronously alongside `egui::Context::copy_text`.
//! - On `wasm32-unknown-unknown` with the `native-clipboard` feature enabled, reads and writes
//!   use the browser Web Clipboard API (`window.navigator.clipboard.writeText` / `readText`).
//!   Because `readText()` returns a `Promise`, results are delivered through a bounded,
//!   drop-safe completion inbox that wakes `egui` via `Context::request_repaint()`.
//! - When `native-clipboard` is disabled or unavailable (for example, insecure HTTP origins or
//!   headless environments), operations gracefully fall back to `egui`'s built-in clipboard
//!   events and `ViewportCommand::RequestPaste`.

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
use std::collections::VecDeque;
#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use egui::{Context, Ui, ViewportCommand};
use egui_xterm_core::{ClipboardSelection, Terminal};

/// Maximum number of completed asynchronous clipboard operations buffered between frames.
#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
const MAX_CLIPBOARD_QUEUE: usize = 16;

/// Upper bound (`16 MiB`) on text accepted from an external clipboard read.
#[cfg(any(feature = "native-clipboard", test))]
const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

/// Result of an asynchronous platform clipboard read ready for consumption on the next frame.
#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipboardCompletion {
    /// Clipboard text retrieved for a paste action (`Ctrl+Shift+V` or context menu).
    Paste(String),
    /// Asynchronous paste read failed or was denied by browser permissions; fall back to
    /// `ViewportCommand::RequestPaste`.
    PasteRejected,
    /// Clipboard text retrieved to fulfill an `OSC 52` read request.
    Osc52Read {
        /// Target clipboard selection advertised by the `OSC 52` query.
        selection: ClipboardSelection,
        /// Retrieved UTF-8 clipboard payload.
        text: String,
    },
}

/// Immediate outcome of dispatching an `OSC 52` platform clipboard read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Osc52ReadDispatch {
    /// Synchronously read the platform clipboard and populated the terminal reply buffer.
    #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
    CompletedSync,
    /// Initiated an asynchronous browser clipboard read that will complete on a future frame.
    #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
    StartedAsync,
    /// Platform clipboard read is disabled or unavailable on the current host.
    Unavailable,
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
#[derive(Debug, Default)]
struct ClipboardInbox {
    paste_in_flight: bool,
    paste_epoch: u64,
    osc52_in_flight: bool,
    osc52_epoch: u64,
    osc52_pending_selections: Vec<ClipboardSelection>,
    completed: VecDeque<ClipboardCompletion>,
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
#[derive(Debug, Default)]
struct ClipboardShared {
    has_completions: AtomicBool,
    inbox: Mutex<ClipboardInbox>,
}

/// Per-runtime bridge coordinating synchronous desktop clipboard access and asynchronous
/// browser Web Clipboard API reads.
#[derive(Debug, Default)]
pub(crate) struct ClipboardBridge {
    #[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
    shared: Arc<ClipboardShared>,
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InFlightKind {
    Paste,
    Osc52,
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
struct InFlightGuard {
    shared: Weak<ClipboardShared>,
    kind: InFlightKind,
    epoch: u64,
    armed: bool,
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
impl InFlightGuard {
    fn new(shared: &Arc<ClipboardShared>, kind: InFlightKind, epoch: u64) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            kind,
            epoch,
            armed: true,
        }
    }

    fn finish_paste(mut self, completion: Option<ClipboardCompletion>) -> bool {
        self.armed = false;
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut inbox = lock_inbox(&shared.inbox);
        if inbox.paste_epoch != self.epoch {
            return false;
        }
        inbox.paste_in_flight = false;
        if let Some(item) = completion
            && inbox.completed.len() < MAX_CLIPBOARD_QUEUE
        {
            inbox.completed.push_back(item);
            shared.has_completions.store(true, Ordering::Release);
            return true;
        }
        false
    }

    fn finish_osc52(mut self, text: Option<String>) -> bool {
        self.armed = false;
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let mut inbox = lock_inbox(&shared.inbox);
        if inbox.osc52_epoch != self.epoch {
            return false;
        }
        inbox.osc52_in_flight = false;
        let selections = std::mem::take(&mut inbox.osc52_pending_selections);
        let Some(text) = text else {
            return false;
        };
        let mut pushed = false;
        let mut iter = selections.into_iter().peekable();
        let mut remaining_text = Some(text);
        while let Some(selection) = iter.next() {
            if inbox.completed.len() >= MAX_CLIPBOARD_QUEUE {
                break;
            }
            let payload = if iter.peek().is_none() {
                remaining_text.take().unwrap_or_default()
            } else {
                remaining_text.clone().unwrap_or_default()
            };
            inbox.completed.push_back(ClipboardCompletion::Osc52Read {
                selection,
                text: payload,
            });
            pushed = true;
        }
        if pushed {
            shared.has_completions.store(true, Ordering::Release);
        }
        pushed
    }
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(shared) = self.shared.upgrade()
        {
            let mut inbox = lock_inbox(&shared.inbox);
            match self.kind {
                InFlightKind::Paste => {
                    if inbox.paste_epoch == self.epoch {
                        inbox.paste_in_flight = false;
                    }
                }
                InFlightKind::Osc52 => {
                    if inbox.osc52_epoch == self.epoch {
                        inbox.osc52_in_flight = false;
                        inbox.osc52_pending_selections.clear();
                    }
                }
            }
        }
    }
}

#[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
fn lock_inbox(mutex: &Mutex<ClipboardInbox>) -> MutexGuard<'_, ClipboardInbox> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Truncates `text` to at most [`MAX_CLIPBOARD_BYTES`] on a valid UTF-8 character boundary.
#[cfg(any(feature = "native-clipboard", test))]
pub(crate) fn clamp_clipboard_text(text: &str) -> &str {
    if text.len() <= MAX_CLIPBOARD_BYTES {
        return text;
    }
    let mut end = MAX_CLIPBOARD_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Copies `text` to the platform clipboard (when `native-clipboard` is enabled) and emits
/// `egui`'s authoritative `OutputCommand::CopyText`.
pub(crate) fn copy_text_everywhere(ui: &Ui, text: String) {
    #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
    {
        if let Ok(mut cb) = arboard::Clipboard::new()
            && cb.set_text(text.clone()).is_err()
        {
            // Best-effort native clipboard write; `ui.ctx().copy_text` below is authoritative.
        }
    }
    #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
    {
        web::write_text(&text);
    }
    ui.ctx().copy_text(text);
}

#[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
fn try_read_desktop_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

impl ClipboardBridge {
    /// Returns `true` when `native-clipboard` is compiled in and the host platform exposes a
    /// clipboard API (on `wasm32`, verifies `window.navigator.clipboard` is present).
    #[must_use]
    pub(crate) fn is_platform_clipboard_available() -> bool {
        #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
        {
            true
        }
        #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
        {
            web::is_clipboard_api_available()
        }
        #[cfg(not(feature = "native-clipboard"))]
        {
            false
        }
    }

    /// Returns `true` while an asynchronous clipboard read is in flight or has undrained output.
    #[must_use]
    pub(crate) fn has_pending_read(&self) -> bool {
        #[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
        {
            if self.shared.has_completions.load(Ordering::Acquire) {
                return true;
            }
            let inbox = lock_inbox(&self.shared.inbox);
            inbox.paste_in_flight || inbox.osc52_in_flight || !inbox.completed.is_empty()
        }
        #[cfg(not(any(all(feature = "native-clipboard", target_arch = "wasm32"), test)))]
        {
            false
        }
    }

    /// Cancels any in-flight or undrained asynchronous paste read (for example, when the
    /// browser already delivered an authoritative `egui::Event::Paste` for `Ctrl+Shift+V`).
    pub(crate) fn cancel_pending_paste(&self) {
        #[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
        {
            let mut inbox = lock_inbox(&self.shared.inbox);
            inbox.paste_epoch = inbox.paste_epoch.wrapping_add(1);
            inbox.paste_in_flight = false;
            inbox.completed.retain(|item| {
                !matches!(
                    item,
                    ClipboardCompletion::Paste(_) | ClipboardCompletion::PasteRejected
                )
            });
            self.shared
                .has_completions
                .store(!inbox.completed.is_empty(), Ordering::Release);
        }
    }

    /// Cancels any in-flight or undrained asynchronous `OSC 52` clipboard reads (for example,
    /// when the host revokes consent via `Osc52ReadPolicy::Deny` or `deny_osc52_read`).
    pub(crate) fn cancel_pending_osc52(&self) {
        #[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
        {
            let mut inbox = lock_inbox(&self.shared.inbox);
            inbox.osc52_epoch = inbox.osc52_epoch.wrapping_add(1);
            inbox.osc52_in_flight = false;
            inbox.osc52_pending_selections.clear();
            inbox
                .completed
                .retain(|item| !matches!(item, ClipboardCompletion::Osc52Read { .. }));
            self.shared
                .has_completions
                .store(!inbox.completed.is_empty(), Ordering::Release);
        }
    }

    /// Drains any completed asynchronous clipboard reads into `out`.
    ///
    /// Uses a single `Acquire` atomic check on the common idle path so frames without
    /// asynchronous clipboard activity never acquire the mutex.
    #[cfg(any(all(feature = "native-clipboard", target_arch = "wasm32"), test))]
    pub(crate) fn drain_completions(&self, out: &mut Vec<ClipboardCompletion>) {
        if !self.shared.has_completions.load(Ordering::Acquire) {
            return;
        }
        let mut inbox = lock_inbox(&self.shared.inbox);
        out.extend(inbox.completed.drain(..));
        self.shared.has_completions.store(false, Ordering::Release);
    }

    /// Triggers a paste operation from the platform clipboard.
    ///
    /// Returns `true` when text was synchronously read and encoded into `out_bytes`, or `false`
    /// when an asynchronous browser read or `ViewportCommand::RequestPaste` fallback was
    /// scheduled instead.
    pub(crate) fn trigger_paste(
        &self,
        ctx: &Context,
        terminal: &Terminal,
        out_bytes: &mut Vec<u8>,
    ) -> bool {
        #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
        {
            if let Some(text) = try_read_desktop_clipboard() {
                let clamped = clamp_clipboard_text(&text);
                if !clamped.is_empty() {
                    terminal.format_paste_into(clamped, out_bytes);
                }
                return true;
            }
            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
            false
        }

        #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
        {
            let _unused = (terminal, out_bytes);
            if !self.start_web_paste_read(ctx) {
                ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
            }
            false
        }

        #[cfg(not(feature = "native-clipboard"))]
        {
            let _unused = (terminal, out_bytes);
            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
            false
        }
    }

    /// Reads the platform clipboard to answer an `OSC 52` read request for `selection`.
    pub(crate) fn trigger_osc52_read(
        &self,
        ctx: &Context,
        selection: ClipboardSelection,
        terminal: &mut Terminal,
    ) -> Osc52ReadDispatch {
        #[cfg(all(feature = "native-clipboard", not(target_arch = "wasm32")))]
        {
            let _unused_ctx = ctx;
            if let Some(text) = try_read_desktop_clipboard() {
                terminal.provide_clipboard_read(selection, clamp_clipboard_text(&text));
                Osc52ReadDispatch::CompletedSync
            } else {
                Osc52ReadDispatch::Unavailable
            }
        }

        #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
        {
            let _unused_terminal = terminal;
            if self.start_web_osc52_read(ctx, selection) {
                Osc52ReadDispatch::StartedAsync
            } else {
                Osc52ReadDispatch::Unavailable
            }
        }

        #[cfg(not(feature = "native-clipboard"))]
        {
            let _unused = (ctx, selection, terminal);
            Osc52ReadDispatch::Unavailable
        }
    }

    #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
    fn start_web_paste_read(&self, ctx: &Context) -> bool {
        let Some(cb) = web::get_readable_clipboard() else {
            return false;
        };
        if lock_inbox(&self.shared.inbox).paste_in_flight {
            return true;
        }
        let Some(promise) = web::call_read_text(&cb) else {
            return false;
        };
        let epoch = {
            let mut inbox = lock_inbox(&self.shared.inbox);
            if inbox.paste_in_flight {
                return true;
            }
            inbox.paste_in_flight = true;
            inbox.paste_epoch
        };
        let guard = InFlightGuard::new(&self.shared, InFlightKind::Paste, epoch);
        let repaint_ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let completion = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(val) => match val.as_string() {
                    Some(text) => {
                        let clamped = clamp_clipboard_text(&text).to_owned();
                        ClipboardCompletion::Paste(clamped)
                    }
                    None => ClipboardCompletion::PasteRejected,
                },
                Err(_) => ClipboardCompletion::PasteRejected,
            };
            if guard.finish_paste(Some(completion)) {
                repaint_ctx.request_repaint();
            }
        });
        true
    }

    #[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
    fn start_web_osc52_read(&self, ctx: &Context, selection: ClipboardSelection) -> bool {
        let Some(cb) = web::get_readable_clipboard() else {
            return false;
        };
        {
            let mut inbox = lock_inbox(&self.shared.inbox);
            if inbox.osc52_in_flight {
                if !inbox.osc52_pending_selections.contains(&selection) {
                    inbox.osc52_pending_selections.push(selection);
                }
                return true;
            }
        }
        let Some(promise) = web::call_read_text(&cb) else {
            return false;
        };
        let epoch = {
            let mut inbox = lock_inbox(&self.shared.inbox);
            if !inbox.osc52_pending_selections.contains(&selection) {
                inbox.osc52_pending_selections.push(selection);
            }
            if inbox.osc52_in_flight {
                return true;
            }
            inbox.osc52_in_flight = true;
            inbox.osc52_epoch
        };
        let guard = InFlightGuard::new(&self.shared, InFlightKind::Osc52, epoch);
        let repaint_ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let text = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(val) => val
                    .as_string()
                    .map(|raw| clamp_clipboard_text(&raw).to_owned()),
                Err(_) => None,
            };
            if guard.finish_osc52(text) {
                repaint_ctx.request_repaint();
            }
        });
        true
    }

    #[cfg(test)]
    pub(crate) fn begin_test_paste(&self) -> Option<TestClipboardHandle> {
        let mut inbox = lock_inbox(&self.shared.inbox);
        if inbox.paste_in_flight {
            return None;
        }
        inbox.paste_in_flight = true;
        let epoch = inbox.paste_epoch;
        drop(inbox);
        Some(TestClipboardHandle {
            guard: InFlightGuard::new(&self.shared, InFlightKind::Paste, epoch),
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_test_osc52(
        &self,
        selection: ClipboardSelection,
    ) -> Option<TestClipboardHandle> {
        let mut inbox = lock_inbox(&self.shared.inbox);
        if !inbox.osc52_pending_selections.contains(&selection) {
            inbox.osc52_pending_selections.push(selection);
        }
        if inbox.osc52_in_flight {
            return None;
        }
        inbox.osc52_in_flight = true;
        let epoch = inbox.osc52_epoch;
        drop(inbox);
        Some(TestClipboardHandle {
            guard: InFlightGuard::new(&self.shared, InFlightKind::Osc52, epoch),
        })
    }
}

#[cfg(test)]
pub(crate) struct TestClipboardHandle {
    guard: InFlightGuard,
}

#[cfg(test)]
impl TestClipboardHandle {
    pub(crate) fn resolve_paste(self, text: impl Into<String>) -> bool {
        let raw = text.into();
        let clamped = clamp_clipboard_text(&raw).to_owned();
        self.guard
            .finish_paste(Some(ClipboardCompletion::Paste(clamped)))
    }

    pub(crate) fn reject_paste(self) -> bool {
        self.guard
            .finish_paste(Some(ClipboardCompletion::PasteRejected))
    }

    pub(crate) fn resolve_osc52(self, text: impl Into<String>) -> bool {
        let raw = text.into();
        let clamped = clamp_clipboard_text(&raw).to_owned();
        self.guard.finish_osc52(Some(clamped))
    }

    pub(crate) fn reject_osc52(self) -> bool {
        self.guard.finish_osc52(None)
    }
}

#[cfg(all(feature = "native-clipboard", target_arch = "wasm32"))]
mod web {
    use js_sys::{Function, Promise, Reflect};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::{JsFuture, spawn_local};

    fn get_clipboard() -> Option<web_sys::Clipboard> {
        let window = web_sys::window()?;
        if !window.is_secure_context() {
            return None;
        }
        let navigator = window.navigator();
        let raw = Reflect::get(&navigator, &JsValue::from_str("clipboard")).ok()?;
        if raw.is_undefined() || raw.is_null() {
            return None;
        }
        raw.dyn_into::<web_sys::Clipboard>().ok()
    }

    fn get_method(clipboard: &web_sys::Clipboard, name: &str) -> Option<Function> {
        Reflect::get(clipboard, &JsValue::from_str(name))
            .ok()?
            .dyn_into::<Function>()
            .ok()
    }

    pub(super) fn is_clipboard_api_available() -> bool {
        get_readable_clipboard().is_some()
    }

    pub(super) fn get_readable_clipboard() -> Option<web_sys::Clipboard> {
        let cb = get_clipboard()?;
        get_method(&cb, "readText").map(|_| cb)
    }

    pub(super) fn call_read_text(cb: &web_sys::Clipboard) -> Option<Promise> {
        let read_fn = get_method(cb, "readText")?;
        read_fn.call0(cb).ok()?.dyn_into::<Promise>().ok()
    }

    pub(super) fn write_text(text: &str) {
        let Some(cb) = get_clipboard() else {
            return;
        };
        let Some(write_fn) = get_method(&cb, "writeText") else {
            return;
        };
        let Ok(promise_val) = write_fn.call1(&cb, &JsValue::from_str(text)) else {
            return;
        };
        let Ok(promise) = promise_val.dyn_into::<Promise>() else {
            return;
        };
        spawn_local(async move {
            if JsFuture::from(promise).await.is_err() {
                // Best-effort Web Clipboard write; ignore browser permission rejections cleanly.
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_clipboard_availability_matches_feature_on_desktop() {
        assert_eq!(
            ClipboardBridge::is_platform_clipboard_available(),
            cfg!(feature = "native-clipboard")
        );
    }

    #[test]
    fn clamp_clipboard_text_preserves_utf8_char_boundaries() {
        assert_eq!(clamp_clipboard_text("hello"), "hello");
        let mut s = "a".repeat(MAX_CLIPBOARD_BYTES - 2);
        s.push('🦀');
        assert_eq!(s.len(), MAX_CLIPBOARD_BYTES + 2);
        let clamped = clamp_clipboard_text(&s);
        assert_eq!(clamped.len(), MAX_CLIPBOARD_BYTES - 2);
        assert!(clamped.is_ascii());
    }

    #[test]
    fn async_clipboard_dedup_drop_safety_and_drain() {
        let bridge = ClipboardBridge::default();
        assert!(!bridge.has_pending_read());

        let h1 = bridge.begin_test_paste().expect("first paste starts");
        assert!(bridge.has_pending_read());
        assert!(bridge.begin_test_paste().is_none());

        drop(h1);
        assert!(!bridge.has_pending_read());

        let h2 = bridge.begin_test_paste().expect("second paste starts");
        assert!(h2.resolve_paste("hello world"));
        assert!(bridge.has_pending_read());

        let mut drained = Vec::new();
        bridge.drain_completions(&mut drained);
        assert_eq!(
            drained,
            vec![ClipboardCompletion::Paste("hello world".to_owned())]
        );
        assert!(!bridge.has_pending_read());

        let h3 = bridge
            .begin_test_osc52(ClipboardSelection::Clipboard)
            .expect("osc52 starts");
        drop(bridge);
        assert!(!h3.resolve_osc52("late"));
    }

    #[test]
    fn async_clipboard_queue_cap_bounds_memory() {
        let bridge = ClipboardBridge::default();
        for i in 0..(MAX_CLIPBOARD_QUEUE + 5) {
            let h = bridge.begin_test_paste().expect("paste slot available");
            let pushed = h.resolve_paste(format!("msg-{i}"));
            assert_eq!(pushed, i < MAX_CLIPBOARD_QUEUE);
        }
        let mut drained = Vec::new();
        bridge.drain_completions(&mut drained);
        assert_eq!(drained.len(), MAX_CLIPBOARD_QUEUE);
        assert_eq!(
            drained.first(),
            Some(&ClipboardCompletion::Paste("msg-0".to_owned()))
        );
    }

    #[test]
    fn osc52_rejection_fails_closed_without_queuing_text() {
        let bridge = ClipboardBridge::default();
        let h = bridge
            .begin_test_osc52(ClipboardSelection::Clipboard)
            .expect("osc52 starts");
        assert!(!h.reject_osc52());
        assert!(!bridge.has_pending_read());
        let mut drained = Vec::new();
        bridge.drain_completions(&mut drained);
        assert!(drained.is_empty());
    }

    #[test]
    fn osc52_coalesces_distinct_selections_and_epoch_cancel_isolates_stale_handles() {
        let bridge = ClipboardBridge::default();

        // Two distinct selections while one read is in flight both receive replies on resolution.
        let h1 = bridge
            .begin_test_osc52(ClipboardSelection::Clipboard)
            .expect("first osc52 starts");
        assert!(
            bridge
                .begin_test_osc52(ClipboardSelection::Primary)
                .is_none()
        );
        assert!(
            bridge
                .begin_test_osc52(ClipboardSelection::Primary)
                .is_none()
        );
        assert!(h1.resolve_osc52("shared-clip"));

        let mut drained = Vec::new();
        bridge.drain_completions(&mut drained);
        assert_eq!(
            drained,
            vec![
                ClipboardCompletion::Osc52Read {
                    selection: ClipboardSelection::Clipboard,
                    text: "shared-clip".to_owned(),
                },
                ClipboardCompletion::Osc52Read {
                    selection: ClipboardSelection::Primary,
                    text: "shared-clip".to_owned(),
                },
            ]
        );

        // Cancelling an in-flight paste invalidates its handle AND lets a fresh epoch start
        // without the dropped stale handle clearing the new epoch's in-flight state.
        let stale_paste = bridge.begin_test_paste().expect("stale paste starts");
        bridge.cancel_pending_paste();
        assert!(!bridge.has_pending_read());

        let fresh_paste = bridge.begin_test_paste().expect("fresh paste starts");
        assert!(bridge.has_pending_read());
        drop(stale_paste);
        assert!(
            bridge.has_pending_read(),
            "dropping cancelled stale handle must not clear fresh epoch"
        );
        assert!(fresh_paste.resolve_paste("fresh"));

        drained.clear();
        bridge.drain_completions(&mut drained);
        assert_eq!(
            drained,
            vec![ClipboardCompletion::Paste("fresh".to_owned())]
        );

        // Cancelling an in-flight OSC 52 read prevents late resolution from queuing anything.
        let stale_osc = bridge
            .begin_test_osc52(ClipboardSelection::Clipboard)
            .expect("stale osc52 starts");
        bridge.cancel_pending_osc52();
        assert!(!stale_osc.resolve_osc52("leaked"));
        drained.clear();
        bridge.drain_completions(&mut drained);
        assert!(drained.is_empty());
    }
}
