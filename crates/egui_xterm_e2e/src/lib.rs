//! Opaque-box end-to-end test harness for `egui_xterm` (Tiers 1–5).

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Context, Event, FullOutput, ImeEvent, Key, Modifiers, MouseWheelSource, MouseWheelUnit,
    OutputCommand, PlatformOutput, PointerButton, Pos2, RawInput, Rect, Role, TouchPhase, Ui, Vec2,
    ViewportId,
};
use egui_xterm::{
    TerminalConfig, TerminalInput, TerminalIoError, TerminalResponse, TerminalRuntime, TerminalSize,
};
use epaint::{
    ClippedPrimitive, ClippedShape, Color32, Galley, ImageData, PaintStats, Primitive, Shape,
    TextureId, textures::TexturesDelta,
};

// ---------------------------------------------------------------------------
// Ungated M1 Input Test Doubles & Decorators
// ---------------------------------------------------------------------------

/// Scripted, deterministic [`TerminalInput`] test double for opaque-box E2E tests.
///
/// Supports chunked reads, outgoing byte capture, resize recording, and error/EOF injection.
pub struct ScriptedInput {
    chunks: VecDeque<Vec<u8>>,
    written: Vec<u8>,
    resizes: Vec<TerminalSize>,
    closed: bool,
    read_error: Option<TerminalIoError>,
    write_error: Option<TerminalIoError>,
    resize_error: Option<TerminalIoError>,
}

impl ScriptedInput {
    /// Creates an empty, open [`ScriptedInput`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            written: Vec::new(),
            resizes: Vec::new(),
            closed: false,
            read_error: None,
            write_error: None,
            resize_error: None,
        }
    }

    /// Creates a [`ScriptedInput`] preloaded with a single byte chunk.
    #[must_use]
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let mut s = Self::new();
        s.push_bytes(bytes);
        s
    }

    /// Creates a [`ScriptedInput`] preloaded with multiple discrete chunks.
    #[must_use]
    pub fn from_chunks(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut s = Self::new();
        for chunk in chunks {
            if !chunk.is_empty() {
                s.chunks.push_back(chunk);
            }
        }
        s
    }

    /// Enqueues a byte slice as an incoming chunk.
    pub fn push_bytes(&mut self, bytes: impl AsRef<[u8]>) {
        let slice = bytes.as_ref();
        if !slice.is_empty() {
            self.chunks.push_back(slice.to_vec());
        }
    }

    /// Enqueues UTF-8 text bytes as an incoming chunk.
    pub fn push_str(&mut self, s: impl AsRef<str>) {
        self.push_bytes(s.as_ref().as_bytes());
    }

    /// Enqueues `bytes` split into chunks of at most `max_chunk_size` bytes.
    ///
    /// Each `poll_read` call reads from at most one front chunk, so `max_chunk_size = 1`
    /// deterministically exercises 1-byte-per-`poll_read` trickle behavior.
    pub fn push_chunked(&mut self, bytes: impl AsRef<[u8]>, max_chunk_size: usize) {
        let size = max_chunk_size.max(1);
        for chunk in bytes.as_ref().chunks(size) {
            self.chunks.push_back(chunk.to_vec());
        }
    }

    /// Returns all outgoing bytes written via [`TerminalInput::write_bytes`].
    #[must_use]
    pub fn written_bytes(&self) -> &[u8] {
        &self.written
    }

    /// Returns all outgoing bytes decoded lossily as UTF-8.
    #[must_use]
    pub fn written_str(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }

    /// Takes and clears all recorded outgoing bytes.
    pub fn take_written(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.written)
    }

    /// Clears recorded outgoing bytes.
    pub fn clear_written(&mut self) {
        self.written.clear();
    }

    /// Returns `true` when no non-empty incoming chunks remain queued.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.chunks.iter().all(Vec::is_empty)
    }

    /// Returns all [`TerminalSize`] values passed to [`TerminalInput::resize`].
    #[must_use]
    pub fn recorded_resizes(&self) -> &[TerminalSize] {
        &self.resizes
    }

    /// Returns the most recent [`TerminalSize`] passed to [`TerminalInput::resize`].
    #[must_use]
    pub fn last_resize(&self) -> Option<TerminalSize> {
        self.resizes.last().copied()
    }

    /// Clears recorded resize events.
    pub fn clear_resizes(&mut self) {
        self.resizes.clear();
    }

    /// Marks the stream as closed once all queued incoming chunks are drained.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Injects an error to be returned by the next non-empty [`TerminalInput::poll_read`].
    pub fn fail_next_read(&mut self, err: TerminalIoError) {
        self.read_error = Some(err);
    }

    /// Injects an error to be returned by the next [`TerminalInput::write_bytes`].
    pub fn fail_next_write(&mut self, err: TerminalIoError) {
        self.write_error = Some(err);
    }

    /// Injects an error to be returned by the next [`TerminalInput::resize`].
    pub fn fail_next_resize(&mut self, err: TerminalIoError) {
        self.resize_error = Some(err);
    }
}

impl Default for ScriptedInput {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalInput for ScriptedInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        while self.chunks.front().is_some_and(Vec::is_empty) {
            self.chunks.pop_front();
        }
        if buf.is_empty() {
            return if self.is_closed() {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        }
        if let Some(err) = self.read_error.take() {
            return Err(err);
        }
        let Some(front) = self.chunks.front_mut() else {
            return if self.closed {
                Err(TerminalIoError::Closed)
            } else {
                Ok(0)
            };
        };
        let n = front.len().min(buf.len());
        buf[..n].copy_from_slice(&front[..n]);
        if n == front.len() {
            self.chunks.pop_front();
        } else {
            front.drain(..n);
        }
        Ok(n)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        if let Some(err) = self.write_error.take() {
            return Err(err);
        }
        if self.closed {
            return Err(TerminalIoError::Closed);
        }
        self.written.extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        self.resizes.push(size);
        if let Some(err) = self.resize_error.take() {
            return Err(err);
        }
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed && self.chunks.iter().all(Vec::is_empty)
    }
}

/// Non-`Send` / non-`Sync` [`TerminalInput`] adapter (`Rc<RefCell<ScriptedInput>>`).
///
/// Proves `!Send` trait compatibility across desktop and `wasm32` builds.
pub struct LocalRcInput {
    inner: Rc<RefCell<ScriptedInput>>,
}

impl LocalRcInput {
    /// Wraps a shared `Rc<RefCell<ScriptedInput>>`.
    #[must_use]
    pub fn new(inner: Rc<RefCell<ScriptedInput>>) -> Self {
        Self { inner }
    }

    /// Returns a reference to the underlying `Rc<RefCell<ScriptedInput>>`.
    #[must_use]
    pub fn inner(&self) -> &Rc<RefCell<ScriptedInput>> {
        &self.inner
    }
}

impl TerminalInput for LocalRcInput {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        self.inner.borrow_mut().poll_read(buf)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        self.inner.borrow_mut().write_bytes(bytes)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        self.inner.borrow_mut().resize(size)
    }

    fn is_closed(&self) -> bool {
        self.inner.borrow().is_closed()
    }
}

/// Spy decorator wrapping any `I: TerminalInput` to record `resize(TerminalSize)` calls.
pub struct CountingResizeInput<I: TerminalInput> {
    inner: I,
    resizes: Vec<TerminalSize>,
}

impl<I: TerminalInput> CountingResizeInput<I> {
    /// Wraps `inner` with resize call recording.
    #[must_use]
    pub fn new(inner: I) -> Self {
        Self {
            inner,
            resizes: Vec::new(),
        }
    }

    /// Returns an immutable reference to the wrapped [`TerminalInput`].
    #[must_use]
    pub fn inner(&self) -> &I {
        &self.inner
    }

    /// Returns a mutable reference to the wrapped [`TerminalInput`].
    pub fn inner_mut(&mut self) -> &mut I {
        &mut self.inner
    }

    /// Returns all [`TerminalSize`] notifications observed by this wrapper.
    #[must_use]
    pub fn recorded_resizes(&self) -> &[TerminalSize] {
        &self.resizes
    }

    /// Clears recorded resize notifications.
    pub fn clear_resizes(&mut self) {
        self.resizes.clear();
    }
}

impl<I: TerminalInput> TerminalInput for CountingResizeInput<I> {
    fn poll_read(&mut self, buf: &mut [u8]) -> Result<usize, TerminalIoError> {
        self.inner.poll_read(buf)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TerminalIoError> {
        self.inner.write_bytes(bytes)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalIoError> {
        self.resizes.push(size);
        self.inner.resize(size)
    }

    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

// ---------------------------------------------------------------------------
// Ungated M1 Inspection & Headless egui 0.37.0 / epaint 0.37.0 Mechanics
// ---------------------------------------------------------------------------

/// Tessellation and shape statistics extracted from `FullOutput` + `Context::tessellate`.
///
/// Implements [`fmt::Debug`] manually because [`epaint::PaintStats`] does not implement `Debug`.
#[derive(Clone)]
pub struct MeshStats {
    /// Total number of `ClippedPrimitive` entries emitted by tessellation.
    pub primitive_count: usize,
    /// Number of `Primitive::Mesh` entries.
    pub mesh_count: usize,
    /// Number of `Primitive::Callback` entries.
    pub callback_count: usize,
    /// Total vertices across all tessellated `Primitive::Mesh` entries.
    pub total_vertices: usize,
    /// Total triangle indices across all tessellated `Primitive::Mesh` entries.
    pub total_indices: usize,
    /// Full `epaint::PaintStats` computed from shapes and clipped primitives.
    pub paint_stats: PaintStats,
}

impl MeshStats {
    fn from_paint_stats_and_primitives(
        paint_stats: PaintStats,
        primitives: &[ClippedPrimitive],
    ) -> Self {
        let mut mesh_count = 0usize;
        let mut callback_count = 0usize;
        let mut total_vertices = 0usize;
        let mut total_indices = 0usize;

        for cp in primitives {
            match &cp.primitive {
                Primitive::Mesh(mesh) => {
                    mesh_count += 1;
                    total_vertices += mesh.vertices.len();
                    total_indices += mesh.indices.len();
                }
                Primitive::Callback(_) => {
                    callback_count += 1;
                }
            }
        }

        Self {
            primitive_count: primitives.len(),
            mesh_count,
            callback_count,
            total_vertices,
            total_indices,
            paint_stats: paint_stats.with_clipped_primitives(primitives),
        }
    }

    /// Computes [`MeshStats`] from captured shapes and tessellated clipped primitives.
    #[must_use]
    pub fn from_shapes_and_primitives(
        shapes: &[ClippedShape],
        primitives: &[ClippedPrimitive],
    ) -> Self {
        Self::from_paint_stats_and_primitives(PaintStats::from_shapes(shapes), primitives)
    }
}

impl fmt::Debug for MeshStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeshStats")
            .field("primitive_count", &self.primitive_count)
            .field("mesh_count", &self.mesh_count)
            .field("callback_count", &self.callback_count)
            .field("total_vertices", &self.total_vertices)
            .field("total_indices", &self.total_indices)
            .field("num_callbacks_in_shapes", &self.paint_stats.num_callbacks)
            .finish()
    }
}

/// Extracted text galley entry from `FullOutput::shapes`.
#[derive(Clone, Debug)]
pub struct RenderedTextEntry {
    /// Top-left logical origin of the text galley.
    pub pos: Pos2,
    /// Full text string inside `galley.job.text`.
    pub text: String,
    /// `galley.pixels_per_point` captured at layout time.
    pub pixels_per_point: f32,
    /// Underlying `Arc<Galley>` for section/color inspection.
    pub galley: Arc<Galley>,
}

/// Flattened, opaque-box inspection view over `Vec<ClippedShape>`.
///
/// Recursively traverses `Shape::Vec(Vec<Shape>)` and scopes `non_snapped_rect_count`
/// using `1e-3` `f32` epsilon tolerance against physical pixel boundaries.
#[derive(Clone, Debug, Default)]
pub struct ShapeInspection {
    /// All text galleys emitted inside the inspected scope.
    pub text_entries: Vec<RenderedTextEntry>,
    /// Total `Shape::Rect` count in scope.
    pub rect_count: usize,
    /// Total `Shape::LineSegment` count in scope.
    pub line_count: usize,
    /// Total `Shape::Path` count in scope.
    pub path_count: usize,
    /// Total `Shape::Mesh` count in scope.
    pub mesh_shape_count: usize,
    /// Non-transparent `Shape::Rect` `(rect, fill)` pairs.
    pub rect_fills: Vec<(Rect, Color32)>,
    /// Sum of `mesh.vertices.len()` across `Shape::Mesh` entries.
    pub mesh_vertices_total: usize,
    /// Texture identifiers referenced by `Shape::Mesh` entries in emission order.
    pub mesh_texture_ids: Vec<TextureId>,
    /// Clip rectangles associated with captured `ClippedShape` entries in scope.
    pub clip_rects: Vec<Rect>,
    /// Count of non-transparent/stroked `Shape::Rect` edges not aligned to physical pixels.
    pub non_snapped_rect_count: usize,
    text_fg_colors: Vec<Color32>,
    mesh_colors: Vec<Color32>,
    mesh_vertex_positions: Vec<Pos2>,
}

impl ShapeInspection {
    /// Inspects all shapes in `shapes` at `pixels_per_point`.
    #[must_use]
    pub fn from_shapes(shapes: &[ClippedShape], pixels_per_point: f32) -> Self {
        let mut out = Self::default();
        for clipped in shapes {
            out.clip_rects.push(clipped.clip_rect);
            visit_shape(&clipped.shape, None, pixels_per_point, &mut out);
        }
        out
    }

    /// Inspects shapes scoped to `terminal_rect` at `pixels_per_point`.
    #[must_use]
    pub fn from_terminal_shapes(
        shapes: &[ClippedShape],
        terminal_rect: Rect,
        pixels_per_point: f32,
    ) -> Self {
        let mut out = Self::default();
        for clipped in shapes {
            let eff_clip = clipped.clip_rect.intersect(terminal_rect);
            if !eff_clip.is_positive() {
                continue;
            }
            let before = (
                out.text_entries.len(),
                out.rect_count,
                out.line_count,
                out.path_count,
                out.mesh_shape_count,
            );
            visit_shape(&clipped.shape, Some(eff_clip), pixels_per_point, &mut out);
            let after = (
                out.text_entries.len(),
                out.rect_count,
                out.line_count,
                out.path_count,
                out.mesh_shape_count,
            );
            if after != before {
                out.clip_rects.push(clipped.clip_rect);
            }
        }
        out
    }

    /// Returns all rendered galley strings in emission order.
    #[must_use]
    pub fn rendered_lines(&self) -> Vec<String> {
        self.text_entries.iter().map(|e| e.text.clone()).collect()
    }

    /// Returns `true` if any rendered galley text contains `needle`.
    #[must_use]
    pub fn contains_text(&self, needle: &str) -> bool {
        self.text_entries.iter().any(|e| e.text.contains(needle))
    }

    /// Returns `true` if any rendered text section or override color equals `color`.
    #[must_use]
    pub fn has_text_fg_color(&self, color: Color32) -> bool {
        self.text_fg_colors.contains(&color)
    }

    /// Returns `true` if any `Shape::Rect` fill or `Shape::Mesh` vertex color equals `color`.
    #[must_use]
    pub fn has_rect_or_mesh_color(&self, color: Color32) -> bool {
        self.rect_fills.iter().any(|(_, c)| *c == color) || self.mesh_colors.contains(&color)
    }

    /// Returns all vertex colors captured from `Shape::Mesh` shapes in scope.
    #[must_use]
    pub fn mesh_colors(&self) -> &[Color32] {
        &self.mesh_colors
    }

    /// Returns all vertex positions captured from `Shape::Mesh` shapes in scope.
    #[must_use]
    pub fn mesh_vertex_positions(&self) -> &[Pos2] {
        &self.mesh_vertex_positions
    }
}

fn is_pixel_snapped(coord: f32, pixels_per_point: f32) -> bool {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return false;
    }
    let scaled = coord * pixels_per_point;
    (scaled - scaled.round()).abs() <= 1e-3
}

fn in_scope(bound: Option<Rect>, rect: Rect) -> bool {
    match bound {
        None => true,
        Some(b) => {
            if rect.is_negative() {
                return false;
            }
            if rect.width() > 0.0 && rect.height() > 0.0 {
                let inter = b.intersect(rect);
                inter.width() > 1e-4 && inter.height() > 1e-4
            } else {
                b.expand(1e-2).intersects(rect)
            }
        }
    }
}

fn visit_shape(
    shape: &Shape,
    bound: Option<Rect>,
    pixels_per_point: f32,
    out: &mut ShapeInspection,
) {
    match shape {
        Shape::Vec(children) => {
            for child in children {
                visit_shape(child, bound, pixels_per_point, out);
            }
        }
        Shape::Text(text_shape) => {
            let bounds = text_shape.visual_bounding_rect();
            let pos_rect = Rect::from_min_size(text_shape.pos, egui::Vec2::ZERO);
            let scoped = if !bounds.is_negative() && bounds.width() > 0.0 && bounds.height() > 0.0 {
                in_scope(bound, bounds)
            } else {
                in_scope(bound, bounds) || in_scope(bound, pos_rect)
            };
            if !scoped {
                return;
            }
            if let Some(over) = text_shape.override_text_color {
                let resolved = if over == Color32::PLACEHOLDER {
                    text_shape.fallback_color
                } else {
                    over
                };
                out.text_fg_colors.push(resolved);
            } else {
                for section in &text_shape.galley.job.sections {
                    let resolved = if section.format.color == Color32::PLACEHOLDER {
                        text_shape.fallback_color
                    } else {
                        section.format.color
                    };
                    out.text_fg_colors.push(resolved);
                }
            }
            out.text_entries.push(RenderedTextEntry {
                pos: text_shape.pos,
                text: text_shape.galley.job.text.clone(),
                pixels_per_point: text_shape.galley.pixels_per_point,
                galley: Arc::clone(&text_shape.galley),
            });
        }
        Shape::Rect(rect_shape) => {
            if !in_scope(bound, rect_shape.rect) {
                return;
            }
            out.rect_count += 1;
            if rect_shape.fill != Color32::TRANSPARENT {
                out.rect_fills.push((rect_shape.rect, rect_shape.fill));
            }
            let visible = rect_shape.fill != Color32::TRANSPARENT || !rect_shape.stroke.is_empty();
            if visible
                && !(is_pixel_snapped(rect_shape.rect.min.x, pixels_per_point)
                    && is_pixel_snapped(rect_shape.rect.min.y, pixels_per_point)
                    && is_pixel_snapped(rect_shape.rect.max.x, pixels_per_point)
                    && is_pixel_snapped(rect_shape.rect.max.y, pixels_per_point))
            {
                out.non_snapped_rect_count += 1;
            }
        }
        Shape::LineSegment { points, .. } => {
            if !in_scope(bound, Rect::from_two_pos(points[0], points[1])) {
                return;
            }
            out.line_count += 1;
        }
        Shape::Path(path_shape) => {
            if !in_scope(bound, path_shape.visual_bounding_rect()) {
                return;
            }
            out.path_count += 1;
        }
        Shape::Mesh(mesh) => {
            if !in_scope(bound, mesh.calc_bounds()) {
                return;
            }
            out.mesh_shape_count += 1;
            out.mesh_texture_ids.push(mesh.texture_id);
            out.mesh_vertices_total += mesh.vertices.len();
            for v in &mesh.vertices {
                out.mesh_colors.push(v.color);
                out.mesh_vertex_positions.push(v.pos);
            }
        }
        Shape::Noop
        | Shape::Circle(_)
        | Shape::Ellipse(_)
        | Shape::Band(_)
        | Shape::QuadraticBezier(_)
        | Shape::CubicBezier(_)
        | Shape::Callback(_) => {}
    }
}

fn copied_texts_from(platform_output: &PlatformOutput) -> Vec<String> {
    platform_output
        .commands
        .iter()
        .filter_map(|cmd| match cmd {
            OutputCommand::CopyText(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn settled_selections_from(platform_output: &PlatformOutput) -> Vec<String> {
    platform_output
        .commands
        .iter()
        .filter_map(|cmd| match cmd {
            OutputCommand::TextSelectionSettled(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn opened_urls_from(platform_output: &PlatformOutput) -> Vec<String> {
    platform_output
        .commands
        .iter()
        .filter_map(|cmd| match cmd {
            OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        })
        .collect()
}

fn accesskit_roles_from(platform_output: &PlatformOutput) -> Vec<Role> {
    platform_output
        .accesskit_update
        .as_ref()
        .map(|update| update.nodes.iter().map(|(_, node)| node.role()).collect())
        .unwrap_or_default()
}

/// Summary of a single texture upload/update in `egui::FullOutput::textures_delta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureSetEntry {
    /// Target `epaint::TextureId`.
    pub texture_id: TextureId,
    /// Texture dimensions `[width, height]` in texels.
    pub size: [usize; 2],
    /// Deterministic 64-bit FNV-1a hash of the RGBA pixel bytes.
    pub pixel_hash: u64,
}

/// Summary of `egui::FullOutput::textures_delta` captured before `drop_without_applying_deltas()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextureDeltaSummary {
    /// Uploaded or updated texture entries sorted deterministically by texture id.
    pub set_entries: Vec<TextureSetEntry>,
    /// Number of texture identifiers queued in `textures_delta.free`.
    pub freed_count: usize,
}

impl TextureDeltaSummary {
    /// Extracts a deterministic summary from `delta`.
    #[must_use]
    pub fn from_textures_delta(delta: &TexturesDelta) -> Self {
        let mut set_entries = Vec::new();
        for (&texture_id, image_deltas) in &delta.set {
            for image_delta in image_deltas {
                let ImageData::Color(color_image) = &image_delta.image;
                set_entries.push(TextureSetEntry {
                    texture_id,
                    size: color_image.size,
                    pixel_hash: fnv1a_pixels(&color_image.pixels),
                });
            }
        }
        set_entries.sort_by_key(|entry| {
            let id_key = match entry.texture_id {
                TextureId::Managed(id) => (0u8, id),
                TextureId::User(id) => (1u8, id),
            };
            (id_key, entry.size, entry.pixel_hash)
        });
        Self {
            set_entries,
            freed_count: delta.free.len(),
        }
    }

    /// Returns `true` when no textures were uploaded or freed in this frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set_entries.is_empty() && self.freed_count == 0
    }
}

fn fnv1a_pixels(pixels: &[Color32]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for px in pixels {
        for byte in px.to_array() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3_u64);
        }
    }
    hash
}

/// Metadata extracted from a shared `egui::FullOutput` after calling
/// `full_output.drop_without_applying_deltas()`.
pub struct FullOutputMeta {
    /// Captured platform output (clipboard/URL commands, IME, cursor, AccessKit).
    pub platform_output: PlatformOutput,
    /// Captured clipped shapes from the frame.
    pub shapes: Vec<ClippedShape>,
    /// Summary of texture uploads and frees captured before dropping deltas.
    pub texture_deltas: TextureDeltaSummary,
    /// Effective `pixels_per_point` for this frame.
    pub pixels_per_point: f32,
    /// Wall-clock time spent inside `ctx.run_ui`.
    pub elapsed: Duration,
    /// Whether `egui` scheduled another repaint on this frame.
    pub repaint_requested: bool,
}

impl FullOutputMeta {
    /// Extracts metadata from `full_output` and drops texture deltas safely.
    #[must_use]
    pub fn from_full_output(ctx: &Context, full_output: FullOutput, elapsed: Duration) -> Self {
        let repaint_requested = full_output
            .viewport_output
            .values()
            .any(|v| v.repaint_delay < Duration::MAX)
            || ctx.has_requested_repaint();
        let platform_output = full_output.platform_output.clone();
        let shapes = full_output.shapes.clone();
        let texture_deltas = TextureDeltaSummary::from_textures_delta(&full_output.textures_delta);
        let pixels_per_point = full_output.pixels_per_point;
        full_output.drop_without_applying_deltas();
        Self {
            platform_output,
            shapes,
            texture_deltas,
            pixels_per_point,
            elapsed,
            repaint_requested,
        }
    }

    /// Inspects all shapes emitted in this frame.
    #[must_use]
    pub fn inspect_shapes(&self) -> ShapeInspection {
        ShapeInspection::from_shapes(&self.shapes, self.pixels_per_point)
    }

    /// Returns all `OutputCommand::CopyText` payloads emitted in this frame.
    #[must_use]
    pub fn copied_texts(&self) -> Vec<String> {
        copied_texts_from(&self.platform_output)
    }

    /// Returns all `OutputCommand::TextSelectionSettled` payloads emitted in this frame.
    #[must_use]
    pub fn settled_selections(&self) -> Vec<String> {
        settled_selections_from(&self.platform_output)
    }

    /// Returns all `OutputCommand::OpenUrl` URLs emitted in this frame.
    #[must_use]
    pub fn opened_urls(&self) -> Vec<String> {
        opened_urls_from(&self.platform_output)
    }

    /// Returns all AccessKit node [`Role`]s emitted in `platform_output.accesskit_update`.
    #[must_use]
    pub fn accesskit_roles(&self) -> Vec<Role> {
        accesskit_roles_from(&self.platform_output)
    }
}

/// Runs one deterministic headless `egui::Context::run_ui` frame and safely drops texture deltas.
pub fn run_headless_ui(
    ctx: &Context,
    screen_rect: Rect,
    native_pixels_per_point: f32,
    sim_time: f64,
    window_focused: bool,
    events: Vec<Event>,
    ui_fn: impl FnMut(&mut Ui),
) -> FullOutputMeta {
    let mut raw_input = RawInput {
        screen_rect: Some(screen_rect),
        time: Some(sim_time),
        focused: window_focused,
        events,
        ..Default::default()
    };
    raw_input
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(native_pixels_per_point);

    let start = Instant::now();
    let full_output = ctx.run_ui(raw_input, ui_fn);
    let elapsed = start.elapsed();
    FullOutputMeta::from_full_output(ctx, full_output, elapsed)
}

/// Tessellates `shapes` at `pixels_per_point` and computes [`MeshStats`].
#[must_use]
pub fn tessellate_and_stats(
    ctx: &Context,
    shapes: Vec<ClippedShape>,
    pixels_per_point: f32,
) -> (Vec<ClippedPrimitive>, MeshStats) {
    let paint_stats = PaintStats::from_shapes(&shapes);
    let primitives = ctx.tessellate(shapes, pixels_per_point);
    let stats = MeshStats::from_paint_stats_and_primitives(paint_stats, &primitives);
    (primitives, stats)
}

// ---------------------------------------------------------------------------
// Headless `egui` Harnesses
// ---------------------------------------------------------------------------

/// Output captured from a single headless `egui::Context::run_ui` terminal frame.
pub struct FrameOutcome {
    /// Response returned by `TerminalRuntime::show`.
    pub terminal_response: TerminalResponse,
    /// Captured platform output.
    pub platform_output: PlatformOutput,
    /// Captured clipped shapes.
    pub shapes: Vec<ClippedShape>,
    /// Summary of texture uploads and frees captured before dropping deltas.
    pub texture_deltas: TextureDeltaSummary,
    /// Effective `pixels_per_point` for this frame.
    pub pixels_per_point: f32,
    /// Wall-clock time spent inside `ctx.run_ui`.
    pub elapsed: Duration,
    /// Whether `egui` scheduled another repaint on this frame.
    pub repaint_requested: bool,
}

impl FrameOutcome {
    fn from_meta(terminal_response: TerminalResponse, meta: FullOutputMeta) -> Self {
        Self {
            terminal_response,
            platform_output: meta.platform_output,
            shapes: meta.shapes,
            texture_deltas: meta.texture_deltas,
            pixels_per_point: meta.pixels_per_point,
            elapsed: meta.elapsed,
            repaint_requested: meta.repaint_requested,
        }
    }

    /// Inspects all shapes emitted in this frame.
    #[must_use]
    pub fn inspect_shapes(&self) -> ShapeInspection {
        ShapeInspection::from_shapes(&self.shapes, self.pixels_per_point)
    }

    /// Returns all `OutputCommand::CopyText` payloads emitted in this frame.
    #[must_use]
    pub fn copied_texts(&self) -> Vec<String> {
        copied_texts_from(&self.platform_output)
    }

    /// Returns all `OutputCommand::TextSelectionSettled` payloads emitted in this frame.
    #[must_use]
    pub fn settled_selections(&self) -> Vec<String> {
        settled_selections_from(&self.platform_output)
    }

    /// Returns all `OutputCommand::OpenUrl` URLs emitted in this frame.
    #[must_use]
    pub fn opened_urls(&self) -> Vec<String> {
        opened_urls_from(&self.platform_output)
    }

    /// Returns all AccessKit node [`Role`]s emitted in `platform_output.accesskit_update`.
    #[must_use]
    pub fn accesskit_roles(&self) -> Vec<Role> {
        accesskit_roles_from(&self.platform_output)
    }
}

/// Headless `egui 0.37` driver wrapping `Context`, `TerminalRuntime<I>`, and `TerminalConfig`.
pub struct HeadlessTermHarness<I: TerminalInput = ScriptedInput> {
    /// Headless `egui::Context` with AccessKit enabled.
    pub ctx: Context,
    /// Terminal runtime under test.
    pub runtime: TerminalRuntime<I>,
    /// Mutable terminal configuration applied each frame.
    pub config: TerminalConfig,
    screen_rect: Rect,
    native_pixels_per_point: f32,
    sim_time: f64,
    window_focused: bool,
    pending_events: Vec<Event>,
    last_outcome: Option<FrameOutcome>,
}

impl HeadlessTermHarness<ScriptedInput> {
    /// Creates a harness backed by an empty [`ScriptedInput`].
    #[must_use]
    pub fn scripted() -> Self {
        Self::new(ScriptedInput::new())
    }

    /// Creates a harness backed by an empty [`ScriptedInput`] and custom `config`.
    #[must_use]
    pub fn scripted_with_config(config: TerminalConfig) -> Self {
        Self::with_config(ScriptedInput::new(), config)
    }
}

impl<I: TerminalInput> HeadlessTermHarness<I> {
    /// Creates a headless harness with default `TerminalConfig` and `800x600` viewport.
    #[must_use]
    pub fn new(input: I) -> Self {
        Self::with_config(input, TerminalConfig::default())
    }

    /// Creates a headless harness with custom `config` and `800x600` viewport.
    #[must_use]
    pub fn with_config(input: I, config: TerminalConfig) -> Self {
        let ctx = Context::default();
        ctx.enable_accesskit();
        Self {
            ctx,
            runtime: TerminalRuntime::with_config(input, config.clone()),
            config,
            screen_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
            native_pixels_per_point: 1.0,
            sim_time: 0.0,
            window_focused: true,
            pending_events: Vec::new(),
            last_outcome: None,
        }
    }

    /// Sets the headless viewport size in logical points at origin `(0, 0)`.
    pub fn set_viewport_size(&mut self, width: f32, height: f32) {
        self.screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height));
    }

    /// Sets an explicit screen rect in logical points.
    pub fn set_screen_rect(&mut self, rect: Rect) {
        self.screen_rect = rect;
    }

    /// Sets `native_pixels_per_point` applied to `ViewportId::ROOT` on subsequent frames.
    pub fn set_pixels_per_point(&mut self, ppp: f32) {
        self.native_pixels_per_point = ppp;
    }

    /// Updates `self.config.zoom_factor`.
    pub fn set_zoom_factor(&mut self, zoom: f32) {
        self.config.zoom_factor = zoom;
    }

    /// Sets `RawInput::focused`.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    /// Advances the deterministic simulated clock by `delta_secs`.
    pub fn advance_time(&mut self, delta_secs: f64) {
        self.sim_time += delta_secs;
    }

    /// Returns the current simulated clock in seconds.
    #[must_use]
    pub fn sim_time(&self) -> f64 {
        self.sim_time
    }

    /// Executes one headless frame rendering the terminal inside `CentralPanel` with `Frame::NONE`.
    pub fn run_frame(&mut self) -> FrameOutcome {
        self.run_frame_in_ui(|ui, runtime, config| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| runtime.show_with_config(ui, config))
                .inner
        })
    }

    /// Executes one headless frame using a caller-supplied UI layout closure.
    pub fn run_frame_in_ui(
        &mut self,
        mut layout_fn: impl FnMut(
            &mut Ui,
            &mut TerminalRuntime<I>,
            &mut TerminalConfig,
        ) -> TerminalResponse,
    ) -> FrameOutcome {
        let events = std::mem::take(&mut self.pending_events);
        let mut captured_resp = None;
        let meta = run_headless_ui(
            &self.ctx,
            self.screen_rect,
            self.native_pixels_per_point,
            self.sim_time,
            self.window_focused,
            events,
            |ui| {
                captured_resp = Some(layout_fn(ui, &mut self.runtime, &mut self.config));
            },
        );
        #[expect(
            clippy::expect_used,
            reason = "test harness helper: egui::Context::run_ui invokes the closure synchronously"
        )]
        let resp = captured_resp.expect("layout_fn must invoke terminal runtime");
        let outcome = FrameOutcome::from_meta(resp, meta);
        self.last_outcome = Some(FrameOutcome {
            terminal_response: outcome.terminal_response.clone(),
            platform_output: outcome.platform_output.clone(),
            shapes: outcome.shapes.clone(),
            texture_deltas: outcome.texture_deltas.clone(),
            pixels_per_point: outcome.pixels_per_point,
            elapsed: outcome.elapsed,
            repaint_requested: outcome.repaint_requested,
        });
        outcome
    }

    /// Runs `count` consecutive frames, advancing `sim_time` by `dt_secs` before each frame.
    pub fn run_frames(&mut self, count: usize, dt_secs: f64) -> Vec<FrameOutcome> {
        let mut outcomes = Vec::with_capacity(count);
        for _ in 0..count {
            self.advance_time(dt_secs);
            outcomes.push(self.run_frame());
        }
        outcomes
    }

    /// Runs up to `max_frames`, advancing `dt_secs` per frame, until `predicate` returns `true`.
    pub fn run_until(
        &mut self,
        max_frames: usize,
        dt_secs: f64,
        mut predicate: impl FnMut(&Self, &FrameOutcome) -> bool,
    ) -> bool {
        for _ in 0..max_frames {
            self.advance_time(dt_secs);
            let outcome = self.run_frame();
            if predicate(self, &outcome) {
                return true;
            }
        }
        false
    }

    /// Computes [`MeshStats`] for the most recently captured frame.
    #[must_use]
    pub fn render_to_mesh_stats(&self) -> MeshStats {
        let Some(last) = &self.last_outcome else {
            return MeshStats::from_shapes_and_primitives(&[], &[]);
        };
        let (_, stats) =
            tessellate_and_stats(&self.ctx, last.shapes.clone(), last.pixels_per_point);
        stats
    }

    /// Tessellates the most recently captured frame into `Vec<ClippedPrimitive>`.
    #[must_use]
    pub fn tessellate_last_frame(&self) -> Vec<ClippedPrimitive> {
        let Some(last) = &self.last_outcome else {
            return Vec::new();
        };
        self.ctx
            .tessellate(last.shapes.clone(), last.pixels_per_point)
    }

    /// Queues an `egui::Event` for the next frame.
    pub fn queue_event(&mut self, event: Event) {
        self.pending_events.push(event);
    }

    /// Queues a key press or release event.
    pub fn inject_key(&mut self, key: Key, pressed: bool, modifiers: Modifiers) {
        self.queue_event(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }

    /// Queues a key press followed by a key release.
    pub fn tap_key(&mut self, key: Key, modifiers: Modifiers) {
        self.inject_key(key, true, modifiers);
        self.inject_key(key, false, modifiers);
    }

    /// Queues `Event::Text`.
    pub fn inject_text(&mut self, text: impl Into<String>) {
        self.queue_event(Event::Text(text.into()));
    }

    /// Queues `Event::PointerMoved`.
    pub fn inject_pointer_move(&mut self, pos: Pos2) {
        self.queue_event(Event::PointerMoved(pos));
    }

    /// Queues `Event::PointerButton`.
    pub fn inject_pointer_button(
        &mut self,
        pos: Pos2,
        button: PointerButton,
        pressed: bool,
        modifiers: Modifiers,
    ) {
        self.queue_event(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        });
    }

    /// Ensures a warm-up frame has run, then injects a pointer move + press + release click.
    pub fn inject_click(&mut self, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
        if self.last_outcome.is_none() {
            let _ = self.run_frame();
        }
        self.inject_pointer_move(pos);
        self.inject_pointer_button(pos, button, true, modifiers);
        self.inject_pointer_button(pos, button, false, modifiers);
        let _ = self.run_frame();
    }

    /// Injects a smooth pointer drag from `from` to `to` across `steps` frames.
    pub fn inject_drag_steps(
        &mut self,
        from: Pos2,
        to: Pos2,
        button: PointerButton,
        modifiers: Modifiers,
        steps: usize,
    ) {
        if self.last_outcome.is_none() {
            let _ = self.run_frame();
        }
        let steps = steps.max(1);
        self.inject_pointer_move(from);
        self.inject_pointer_button(from, button, true, modifiers);
        let _ = self.run_frame();
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let pos = from.lerp(to, t);
            self.inject_pointer_move(pos);
            let _ = self.run_frame();
        }
        self.inject_pointer_button(to, button, false, modifiers);
        let _ = self.run_frame();
    }

    /// Queues `Event::MouseWheel`.
    pub fn inject_scroll(&mut self, unit: MouseWheelUnit, delta: Vec2, modifiers: Modifiers) {
        self.queue_event(Event::MouseWheel {
            unit,
            delta,
            phase: TouchPhase::Move,
            source: MouseWheelSource::Wheel,
            modifiers,
        });
    }

    /// Queues `Event::Ime(ImeEvent::Preedit { .. })`.
    pub fn inject_ime_preedit(
        &mut self,
        text: impl Into<String>,
        active_range_chars: Option<std::ops::Range<usize>>,
    ) {
        self.queue_event(Event::Ime(ImeEvent::Preedit {
            text: text.into(),
            active_range_chars,
        }));
    }

    /// Queues `Event::Ime(ImeEvent::Commit(..))`.
    pub fn inject_ime_commit(&mut self, text: impl Into<String>) {
        self.queue_event(Event::Ime(ImeEvent::Commit(text.into())));
    }

    /// Queues `Event::Copy`.
    pub fn inject_copy(&mut self) {
        self.queue_event(Event::Copy);
    }

    /// Queues `Event::Paste`.
    pub fn inject_paste(&mut self, text: impl Into<String>) {
        self.queue_event(Event::Paste(text.into()));
    }

    /// Ensures a warm-up frame has run, clicks the terminal center, and asserts keyboard focus.
    pub fn focus_terminal(&mut self) {
        if self.last_outcome.is_none() {
            let _ = self.run_frame();
        }
        let center = self.last_outcome.as_ref().map_or_else(
            || self.screen_rect.center(),
            |o| o.terminal_response.response.rect.center(),
        );
        self.inject_click(center, PointerButton::Primary, Modifiers::NONE);
        assert!(
            self.ctx.egui_wants_keyboard_input(),
            "terminal widget did not acquire egui keyboard focus"
        );
    }

    /// Returns the logical center point of cell `(col, row)` from the last frame.
    #[must_use]
    pub fn cell_center(&self, col: u16, row: u16) -> Option<Pos2> {
        let outcome = self.last_outcome.as_ref()?;
        let resp = &outcome.terminal_response;
        if col >= resp.cols || row >= resp.rows {
            return None;
        }
        let origin = resp.response.rect.min;
        Some(Pos2::new(
            origin.x + (f32::from(col) + 0.5) * resp.cell_size.x,
            origin.y + (f32::from(row) + 0.5) * resp.cell_size.y,
        ))
    }

    /// Returns a reference to the most recent [`FrameOutcome`], if any.
    #[must_use]
    pub fn last_outcome(&self) -> Option<&FrameOutcome> {
        self.last_outcome.as_ref()
    }
}

/// Multi-pane / split-screen headless harness for testing independent `TerminalRuntime` instances.
pub struct MultiPaneHarness<A: TerminalInput, B: TerminalInput> {
    /// Shared headless `egui::Context`.
    pub ctx: Context,
    /// Left terminal pane runtime.
    pub left: TerminalRuntime<A>,
    /// Left terminal pane configuration.
    pub left_config: TerminalConfig,
    /// Right terminal pane runtime.
    pub right: TerminalRuntime<B>,
    /// Right terminal pane configuration.
    pub right_config: TerminalConfig,
    /// Fraction of viewport width allocated to the left pane (`0.0..=1.0`).
    pub left_width_fraction: f32,
    screen_rect: Rect,
    native_pixels_per_point: f32,
    sim_time: f64,
    pending_events: Vec<Event>,
}

impl<A: TerminalInput, B: TerminalInput> MultiPaneHarness<A, B> {
    /// Creates a 50/50 split harness at `800x600` with AccessKit enabled.
    ///
    /// Note: `new` intentionally defers rendering the first frame so callers can adjust
    /// viewport size or split fraction before the initial `TerminalInput::resize` runs.
    /// Callers dispatching pointer clicks should run a warm-up [`Self::run_split_frame`]
    /// first so `egui` widget hit-testing rects are populated.
    #[must_use]
    pub fn new(left: A, right: B) -> Self {
        let ctx = Context::default();
        ctx.enable_accesskit();
        let left_config = TerminalConfig::default();
        let right_config = TerminalConfig::default();
        Self {
            ctx,
            left: TerminalRuntime::with_config(left, left_config.clone()),
            left_config,
            right: TerminalRuntime::with_config(right, right_config.clone()),
            right_config,
            left_width_fraction: 0.5,
            screen_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
            native_pixels_per_point: 1.0,
            sim_time: 0.0,
            pending_events: Vec::new(),
        }
    }

    /// Sets the headless viewport size in logical points.
    pub fn set_viewport_size(&mut self, width: f32, height: f32) {
        self.screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height));
    }

    /// Sets the left pane width fraction (`0.0..=1.0`).
    pub fn set_split_fraction(&mut self, fraction: f32) {
        self.left_width_fraction = fraction.clamp(0.0, 1.0);
    }

    /// Sets `native_pixels_per_point`.
    pub fn set_pixels_per_point(&mut self, ppp: f32) {
        self.native_pixels_per_point = ppp;
    }

    /// Advances the simulated clock by `delta_secs`.
    pub fn advance_time(&mut self, delta_secs: f64) {
        self.sim_time += delta_secs;
    }

    /// Queues an `egui::Event` for the next split frame.
    pub fn queue_event(&mut self, event: Event) {
        self.pending_events.push(event);
    }

    /// Executes one split frame rendering both `left` and `right` side-by-side.
    pub fn run_split_frame(&mut self) -> (TerminalResponse, TerminalResponse, FullOutputMeta) {
        let events = std::mem::take(&mut self.pending_events);
        let left_width = self.screen_rect.width() * self.left_width_fraction;
        let left_rect = Rect::from_min_size(
            self.screen_rect.min,
            Vec2::new(left_width, self.screen_rect.height()),
        );
        let right_rect = Rect::from_min_max(
            Pos2::new(self.screen_rect.min.x + left_width, self.screen_rect.min.y),
            self.screen_rect.max,
        );

        let mut left_resp = None;
        let mut right_resp = None;
        let meta = run_headless_ui(
            &self.ctx,
            self.screen_rect,
            self.native_pixels_per_point,
            self.sim_time,
            true,
            events,
            |ui| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                    left_resp = Some(self.left.show_with_config(ui, &mut self.left_config));
                });
                ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                    right_resp = Some(self.right.show_with_config(ui, &mut self.right_config));
                });
            },
        );

        #[expect(
            clippy::expect_used,
            reason = "test harness helper: egui::Context::run_ui invokes both pane closures synchronously"
        )]
        let left = left_resp.expect("left pane rendered");
        #[expect(
            clippy::expect_used,
            reason = "test harness helper: egui::Context::run_ui invokes both pane closures synchronously"
        )]
        let right = right_resp.expect("right pane rendered");
        (left, right, meta)
    }
}

// ---------------------------------------------------------------------------
// Desktop PTY Session (`#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]`)
// ---------------------------------------------------------------------------

/// Desktop-only PTY E2E session wrapper with bounded timeouts and cross-platform shell commands.
#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
pub struct PtyTestSession {
    /// Underlying headless terminal harness driven by `PtyBackend`.
    pub harness: HeadlessTermHarness<egui_xterm_pty::PtyBackend>,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "pty"))]
impl PtyTestSession {
    /// Spawns a PTY session using the provided [`egui_xterm_pty::PtyConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if the underlying PTY or child process fails to spawn.
    pub fn spawn(config: egui_xterm_pty::PtyConfig) -> Result<Self, TerminalIoError> {
        let backend = egui_xterm_pty::PtyBackend::spawn(config)?;
        Ok(Self {
            harness: HeadlessTermHarness::new(backend),
        })
    }

    /// Spawns a cross-platform command echoing `message`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if the PTY command fails to spawn.
    pub fn spawn_echo_command(message: &str) -> Result<Self, TerminalIoError> {
        #[cfg(windows)]
        let (shell, args) = (
            "cmd".to_owned(),
            vec!["/C".to_owned(), format!("echo {message}")],
        );
        #[cfg(not(windows))]
        let (shell, args) = (
            "sh".to_owned(),
            vec!["-c".to_owned(), format!("echo {message}")],
        );

        Self::spawn(egui_xterm_pty::PtyConfig {
            shell: Some(shell),
            args,
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(80, 24),
        })
    }

    /// Spawns an interactive OS shell at `(cols, rows)`.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if the interactive shell fails to spawn.
    pub fn spawn_interactive_shell(cols: u16, rows: u16) -> Result<Self, TerminalIoError> {
        Self::spawn(egui_xterm_pty::PtyConfig {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size: TerminalSize::new(cols, rows),
        })
    }

    /// Polls `run_frame()` with bounded sleep and deterministic `sim_time` advance until
    /// `needle` appears in visible terminal text or `timeout` elapses.
    ///
    /// # Errors
    ///
    /// Propagates any [`TerminalIoError`] encountered while polling the session.
    pub fn run_until_contains(
        &mut self,
        needle: &str,
        timeout: Duration,
    ) -> Result<bool, TerminalIoError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.harness.advance_time(1.0 / 60.0);
            let _ = self.harness.run_frame();
            if self
                .harness
                .runtime
                .terminal()
                .visible_lines()
                .iter()
                .any(|line| line.contains(needle))
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Sends a command line followed by `\r` to the child PTY.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalIoError`] if writing to the PTY fails.
    pub fn send_line(&mut self, line: &str) -> Result<(), TerminalIoError> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\r');
        self.harness.runtime.input_mut().write_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Stroke, WidgetInfo, pos2, vec2};

    #[test]
    fn scripted_input_chunked_and_trickle_reads() {
        let mut input = ScriptedInput::from_chunks(vec![vec![b'a', b'b'], Vec::new(), vec![b'c']]);
        let mut buf = [0u8; 8];
        assert_eq!(input.poll_read(&mut buf), Ok(2));
        assert_eq!(&buf[..2], b"ab");
        assert_eq!(input.poll_read(&mut buf), Ok(1));
        assert_eq!(&buf[..1], b"c");
        assert_eq!(input.poll_read(&mut buf), Ok(0));

        input.push_chunked(b"XYZ", 1);
        for expected in *b"XYZ" {
            assert_eq!(input.poll_read(&mut buf), Ok(1));
            assert_eq!(buf[0], expected);
        }

        input.push_bytes(b"hello");
        let mut small = [0u8; 2];
        assert_eq!(input.poll_read(&mut small), Ok(2));
        assert_eq!(&small, b"he");
        assert_eq!(input.poll_read(&mut buf), Ok(3));
        assert_eq!(&buf[..3], b"llo");

        assert_eq!(input.write_bytes(b"out"), Ok(()));
        assert_eq!(input.written_str(), "out");
        assert_eq!(input.take_written(), b"out");
        assert!(input.written_bytes().is_empty());

        let sz = TerminalSize::new(100, 30);
        assert_eq!(input.resize(sz), Ok(()));
        assert_eq!(input.last_resize(), Some(sz));
        assert_eq!(input.recorded_resizes(), &[sz]);

        input.fail_next_read(TerminalIoError::message("read boom"));
        assert_eq!(
            input.poll_read(&mut buf),
            Err(TerminalIoError::message("read boom"))
        );
        input.fail_next_write(TerminalIoError::message("write boom"));
        assert_eq!(
            input.write_bytes(b"x"),
            Err(TerminalIoError::message("write boom"))
        );
        input.fail_next_resize(TerminalIoError::message("resize boom"));
        assert_eq!(
            input.resize(sz),
            Err(TerminalIoError::message("resize boom"))
        );

        input.push_str("tail");
        input.close();
        assert!(!input.is_closed());
        assert_eq!(input.poll_read(&mut buf), Ok(4));
        assert_eq!(&buf[..4], b"tail");
        assert!(input.is_closed());
        assert_eq!(input.poll_read(&mut buf), Err(TerminalIoError::Closed));
    }

    #[test]
    fn local_rc_input_is_not_send_and_forwards() {
        let shared = Rc::new(RefCell::new(ScriptedInput::from_bytes(b"rc_data")));
        let mut local = LocalRcInput::new(Rc::clone(&shared));

        let dyn_ref: &mut dyn TerminalInput = &mut local;
        let mut buf = [0u8; 16];
        assert_eq!(dyn_ref.poll_read(&mut buf), Ok(7));
        assert_eq!(&buf[..7], b"rc_data");
        assert_eq!(dyn_ref.write_bytes(b"reply"), Ok(()));
        let sz = TerminalSize::new(90, 28);
        assert_eq!(dyn_ref.resize(sz), Ok(()));
        assert!(!dyn_ref.is_closed());

        assert_eq!(shared.borrow().written_bytes(), b"reply");
        assert_eq!(shared.borrow().last_resize(), Some(sz));
    }

    #[test]
    fn counting_resize_input_records_sizes() {
        let inner = ScriptedInput::new();
        let mut spy = CountingResizeInput::new(inner);
        let s1 = TerminalSize::new(80, 24);
        let s2 = TerminalSize::with_pixels(120, 40, 960, 640);
        assert_eq!(spy.resize(s1), Ok(()));
        assert_eq!(spy.resize(s2), Ok(()));
        assert_eq!(spy.recorded_resizes(), &[s1, s2]);
        assert_eq!(spy.inner().recorded_resizes(), &[s1, s2]);
        spy.clear_resizes();
        assert!(spy.recorded_resizes().is_empty());
    }

    #[test]
    fn headless_egui_037_frame_accesskit_dpi_and_tessellate() {
        let ctx = Context::default();
        ctx.enable_accesskit();

        let screen_rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        // Warm-up frame so layout/AccessKit node registration settles.
        let _ = run_headless_ui(&ctx, screen_rect, 2.0, 0.0, true, Vec::new(), |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let resp = ui.allocate_response(vec2(80.0, 20.0), egui::Sense::click());
                    resp.widget_info(|| WidgetInfo::new(Role::Terminal));
                });
        });

        let meta = run_headless_ui(&ctx, screen_rect, 2.0, 0.1, true, Vec::new(), |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let resp = ui.allocate_response(vec2(80.0, 20.0), egui::Sense::click());
                    resp.widget_info(|| WidgetInfo::new(Role::Terminal));
                    let painter = ui.painter();
                    painter.rect_filled(
                        Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 20.0)),
                        0.0,
                        Color32::from_rgb(10, 20, 30),
                    );
                    painter.add(Shape::Vec(vec![Shape::hline(
                        0.0..=80.0,
                        10.0,
                        Stroke::new(1.0, Color32::WHITE),
                    )]));
                    ui.label("HELLO_E2E");
                    ui.ctx().copy_text("clip".to_owned());
                });
        });

        assert!((meta.pixels_per_point - 2.0).abs() < 1e-6);
        assert_eq!(meta.copied_texts(), vec!["clip".to_owned()]);
        assert!(meta.accesskit_roles().contains(&Role::Terminal));

        let inspection = meta.inspect_shapes();
        assert!(inspection.line_count >= 1);
        assert!(inspection.contains_text("HELLO_E2E"));
        assert!(
            inspection
                .text_entries
                .iter()
                .any(|e| e.text.contains("HELLO_E2E") && (e.pixels_per_point - 2.0).abs() < 1e-6)
        );
        assert_eq!(inspection.non_snapped_rect_count, 0);
        assert!(inspection.has_rect_or_mesh_color(Color32::from_rgb(10, 20, 30)));

        let (_prims, stats) = tessellate_and_stats(&ctx, meta.shapes, meta.pixels_per_point);
        assert!(stats.primitive_count > 0);
        assert!(stats.total_vertices > 0);
        assert!(stats.total_indices > 0);
        assert!(format!("{stats:?}").contains("MeshStats"));
    }

    #[test]
    fn shape_inspection_resolves_placeholder_override_text_color() {
        let ctx = Context::default();
        let fallback = Color32::from_rgb(12, 180, 95);
        let meta = run_headless_ui(
            &ctx,
            Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0)),
            1.0,
            0.0,
            true,
            Vec::new(),
            |ui| {
                let galley = ui.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(
                        "PLACEHOLDER_OVERRIDE".to_owned(),
                        egui::FontId::monospace(14.0),
                        Color32::PLACEHOLDER,
                    )
                });
                let mut text_shape = epaint::TextShape::new(pos2(4.0, 4.0), galley, fallback);
                text_shape.override_text_color = Some(Color32::PLACEHOLDER);
                ui.painter().add(Shape::Text(text_shape));
            },
        );

        let inspection = meta.inspect_shapes();
        assert!(inspection.contains_text("PLACEHOLDER_OVERRIDE"));
        assert!(inspection.has_text_fg_color(fallback));
        assert!(!inspection.has_text_fg_color(Color32::PLACEHOLDER));
    }

    #[test]
    fn texture_delta_summary_and_mesh_clip_inspection() {
        let ctx = Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0));
        let term_rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 80.0));
        let tex_red = Color32::from_rgb(200, 30, 40);
        let tex_blue = Color32::from_rgb(20, 90, 220);

        let mut handle = Some(ctx.load_texture(
            "unit_test_tex",
            egui::ColorImage::filled([2, 3], tex_red),
            egui::TextureOptions::NEAREST,
        ));
        let expected_id = handle.as_ref().expect("texture handle created").id();

        let meta1 = run_headless_ui(&ctx, screen_rect, 1.0, 0.0, true, Vec::new(), |ui| {
            let tex_id = handle.as_ref().expect("texture handle live").id();
            let mut textured_mesh = epaint::Mesh::with_texture(tex_id);
            textured_mesh.add_rect_with_uv(
                Rect::from_min_size(pos2(20.0, 20.0), vec2(20.0, 20.0)),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            let mut untextured_mesh = epaint::Mesh::default();
            untextured_mesh.add_colored_rect(
                Rect::from_min_size(pos2(250.0, 120.0), vec2(30.0, 20.0)),
                Color32::GREEN,
            );

            ui.painter_at(term_rect)
                .add(Shape::Mesh(Arc::new(textured_mesh)));
            ui.painter().add(Shape::Mesh(Arc::new(untextured_mesh)));
        });

        let entry1 = meta1
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.texture_id == expected_id)
            .copied()
            .expect("uploaded texture must appear in TextureDeltaSummary");
        assert_eq!(entry1.size, [2, 3]);
        assert_eq!(meta1.texture_deltas.freed_count, 0);
        assert!(!meta1.texture_deltas.is_empty());

        let all_insp = meta1.inspect_shapes();
        assert_eq!(all_insp.mesh_shape_count, 2);
        assert_eq!(
            all_insp.mesh_texture_ids,
            vec![expected_id, TextureId::default()]
        );
        assert!(all_insp.clip_rects.contains(&term_rect));

        let scoped_insp =
            ShapeInspection::from_terminal_shapes(&meta1.shapes, term_rect, meta1.pixels_per_point);
        assert_eq!(scoped_insp.mesh_shape_count, 1);
        assert_eq!(scoped_insp.mesh_texture_ids, vec![expected_id]);
        assert_eq!(scoped_insp.clip_rects, vec![term_rect]);

        // Updating the same TextureHandle produces a different FNV-1a pixel hash.
        if let Some(h) = handle.as_mut() {
            h.set(
                egui::ColorImage::filled([2, 3], tex_blue),
                egui::TextureOptions::NEAREST,
            );
        }
        let meta2 = run_headless_ui(&ctx, screen_rect, 1.0, 0.1, true, Vec::new(), |_ui| {});
        let entry2 = meta2
            .texture_deltas
            .set_entries
            .iter()
            .find(|e| e.texture_id == expected_id)
            .copied()
            .expect("updated texture must appear in TextureDeltaSummary");
        assert_eq!(entry2.size, [2, 3]);
        assert_ne!(entry1.pixel_hash, entry2.pixel_hash);

        // Dropping the TextureHandle records a freed texture delta on the next frame.
        drop(handle);
        let meta3 = run_headless_ui(&ctx, screen_rect, 1.0, 0.2, true, Vec::new(), |_ui| {});
        assert!(meta3.texture_deltas.freed_count >= 1);
    }

    #[test]
    fn from_terminal_shapes_multi_pane_50_50_has_zero_bleed() {
        let mut harness = MultiPaneHarness::new(
            ScriptedInput::from_bytes(b"LEFT_PANE"),
            ScriptedInput::from_bytes(b"RIGHT_PANE"),
        );
        let left_bg = [10, 20, 30];
        let right_bg = [200, 100, 50];
        harness.left_config.theme.scheme.background = left_bg;
        harness.right_config.theme.scheme.background = right_bg;

        let (left_resp, right_resp, meta) = harness.run_split_frame();
        let left_rect = left_resp.response.rect;
        let right_rect = right_resp.response.rect;
        assert!(
            (left_rect.max.x - right_rect.min.x).abs() < 1e-3,
            "expected shared 50/50 split boundary: left={left_rect:?}, right={right_rect:?}"
        );

        let left_insp =
            ShapeInspection::from_terminal_shapes(&meta.shapes, left_rect, meta.pixels_per_point);
        let right_insp =
            ShapeInspection::from_terminal_shapes(&meta.shapes, right_rect, meta.pixels_per_point);

        let left_bg_c32 = Color32::from_rgb(left_bg[0], left_bg[1], left_bg[2]);
        let right_bg_c32 = Color32::from_rgb(right_bg[0], right_bg[1], right_bg[2]);
        assert!(left_insp.has_rect_or_mesh_color(left_bg_c32));
        assert!(
            !left_insp.has_rect_or_mesh_color(right_bg_c32),
            "right pane background leaked into left pane inspection: clip_rects={:?}",
            left_insp.clip_rects
        );
        assert!(right_insp.has_rect_or_mesh_color(right_bg_c32));
        assert!(
            !right_insp.has_rect_or_mesh_color(left_bg_c32),
            "left pane background leaked into right pane inspection: clip_rects={:?}",
            right_insp.clip_rects
        );

        assert!(!left_insp.clip_rects.is_empty());
        assert!(!right_insp.clip_rects.is_empty());
        assert!(left_insp.clip_rects.iter().all(|c| {
            let inter = c.intersect(right_rect);
            !inter.is_positive()
        }));
        assert!(right_insp.clip_rects.iter().all(|c| {
            let inter = c.intersect(left_rect);
            !inter.is_positive()
        }));
        assert!(
            left_insp
                .mesh_vertex_positions()
                .iter()
                .all(|p| p.x <= left_rect.max.x + 1e-3)
        );
        assert!(
            right_insp
                .mesh_vertex_positions()
                .iter()
                .all(|p| p.x >= right_rect.min.x - 1e-3)
        );
    }

    #[test]
    fn from_terminal_shapes_excludes_disjoint_clip_rect() {
        let ctx = Context::default();
        let screen_rect = Rect::from_min_size(Pos2::ZERO, vec2(500.0, 500.0));
        let term_rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(100.0, 80.0));
        let disjoint_clip = Rect::from_min_size(pos2(400.0, 400.0), vec2(50.0, 50.0));
        let edge_touching_clip = Rect::from_min_size(pos2(110.0, 10.0), vec2(50.0, 80.0));

        let meta = run_headless_ui(&ctx, screen_rect, 1.0, 0.0, true, Vec::new(), |ui| {
            let mut mesh = epaint::Mesh::default();
            mesh.add_colored_rect(term_rect, Color32::from_rgb(220, 40, 40));
            ui.painter_at(disjoint_clip)
                .add(Shape::Mesh(Arc::new(mesh)));

            let mut edge_mesh = epaint::Mesh::default();
            edge_mesh.add_colored_rect(term_rect, Color32::from_rgb(40, 220, 40));
            ui.painter_at(edge_touching_clip)
                .add(Shape::Mesh(Arc::new(edge_mesh)));
        });

        let insp =
            ShapeInspection::from_terminal_shapes(&meta.shapes, term_rect, meta.pixels_per_point);
        assert_eq!(insp.mesh_shape_count, 0);
        assert!(insp.clip_rects.is_empty());
        assert!(!insp.has_rect_or_mesh_color(Color32::from_rgb(220, 40, 40)));
        assert!(!insp.has_rect_or_mesh_color(Color32::from_rgb(40, 220, 40)));
    }
}
