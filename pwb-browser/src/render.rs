//! Skia 2D Graphical Renderer & Pixel-Exact Hit Testing (`pwb-browser::render`).

use crate::dom::Dom;
use crate::layout::{build_layout_tree, measure_text, resolve_unit, LayoutBox};
use ab_glyph::{Font, FontArc, Glyph, PxScale};
use pwb_core::spec::{Color, CursorStyle, OverflowStyle, Tag, UnitValue};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tiny_skia::{Color as SkiaColor, ColorU8, Paint, Path, PathBuilder, Pixmap, Rect, Transform};
use winit::window::CursorIcon;

/// A box is a scroll container when it asks for scrolling and its content
/// actually overflows its rect.
fn is_scroll_container(b: &LayoutBox) -> bool {
    matches!(b.overflow, OverflowStyle::Scroll | OverflowStyle::Auto)
        && b.content_height > b.rect.height() + 1.0
}

/// The scroll offset for a container, clamped to its current range so a stale
/// offset (e.g. after the content shrank) can never scroll past the end.
fn scroll_offset_for(b: &LayoutBox, scrolls: &HashMap<usize, f32>) -> f32 {
    let max = (b.content_height - b.rect.height()).max(0.0);
    scrolls
        .get(&b.node_id)
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, max)
}

/// Intersect a screen-space clip with a rect, returning `None` when empty.
fn intersect_opt(clip: Option<Rect>, rect: Rect) -> Option<Rect> {
    let base = clip.unwrap_or(rect);
    let left = base.left().max(rect.left());
    let top = base.top().max(rect.top());
    let right = base.right().min(rect.right());
    let bottom = base.bottom().min(rect.bottom());
    if right > left && bottom > top {
        Rect::from_xywh(left, top, right - left, bottom - top)
    } else {
        None
    }
}

/// Translation + clip state threaded through rendering/hit-testing so content
/// inside scroll containers is drawn and hit at its scrolled position, clipped
/// to the container.
#[derive(Clone, Copy)]
pub struct RenderState {
    pub tx: f32,
    pub ty: f32,
    pub clip: Option<Rect>,
}

impl RenderState {
    pub fn identity() -> Self {
        Self {
            tx: 0.0,
            ty: 0.0,
            clip: None,
        }
    }
}

/// The child state for a box: applies this box's scroll offset and clip to the
/// subtree when the box is a scroll container.
pub fn child_render_state(
    b: &LayoutBox,
    state: RenderState,
    scrolls: &HashMap<usize, f32>,
) -> RenderState {
    let mut child = state;
    if is_scroll_container(b) {
        child.ty -= scroll_offset_for(b, scrolls);
    }
    if matches!(
        b.overflow,
        OverflowStyle::Hidden | OverflowStyle::Scroll | OverflowStyle::Auto
    ) {
        let screen = Rect::from_xywh(
            b.rect.left() + state.tx,
            b.rect.top() + state.ty,
            b.rect.width(),
            b.rect.height(),
        )
        .unwrap();
        child.clip = intersect_opt(child.clip, screen);
    }
    child
}

static SYSTEM_FONT: OnceLock<Option<FontArc>> = OnceLock::new();

/// Parsed feather icon paths, cached per icon name so per-frame rendering only
/// re-strokes (scaled) rather than re-tokenizing the path data.
static ICON_PATH_CACHE: OnceLock<Mutex<HashMap<String, Path>>> = OnceLock::new();

fn cached_icon_path(name: &str) -> Option<Path> {
    let cache = ICON_PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut cache) = cache.lock() {
        if let Some(p) = cache.get(name) {
            return Some(p.clone());
        }
        if let Some(d) = crate::feather::feather_path(name) {
            let p = parse_icon_path(d);
            if p.len() > 0 {
                cache.insert(name.to_string(), p.clone());
                return Some(p);
            }
        }
    }
    None
}

/// Parse the normalized `M`/`L`/`C`/`Z` path data emitted by the feather
/// generator into a tiny_skia path (24x24 viewBox coordinates).
///
/// The generator resolves every other SVG command (arcs, quads, smooths,
/// H/V lines, shape elements) so only four command letters are possible here.
pub fn parse_icon_path(d: &str) -> Path {
    let mut pb = PathBuilder::new();
    let mut nums = d.split_whitespace().peekable();
    let mut cmd = 'M';
    while let Some(tok) = nums.next() {
        let c = tok.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            cmd = c;
            continue;
        }
        // A numeric token: consume the coordinates for the current command.
        // Every `M` starts a new subpath (icons contain multiple shapes).
        let x = tok.parse::<f32>().unwrap_or(0.0);
        let y = nums
            .next()
            .and_then(|t| t.parse::<f32>().ok())
            .unwrap_or(0.0);
        match cmd {
            'M' => pb.move_to(x, y),
            'L' => pb.line_to(x, y),
            'C' => {
                // Two more control points + the end point: 3 pairs total.
                let x1 = x;
                let y1 = y;
                let x2 = nums
                    .next()
                    .and_then(|t| t.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let y2 = nums
                    .next()
                    .and_then(|t| t.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let x3 = nums
                    .next()
                    .and_then(|t| t.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let y3 = nums
                    .next()
                    .and_then(|t| t.parse::<f32>().ok())
                    .unwrap_or(0.0);
                pb.cubic_to(x1, y1, x2, y2, x3, y3);
            }
            'Z' => pb.close(),
            _ => {}
        }
    }
    pb.finish().unwrap_or_else(|| {
        // The generator never emits empty data; this fallback only guards
        // against degenerate input.
        PathBuilder::from_rect(Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap())
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextSelection {
    pub start_node_id: Option<usize>,
    pub start_char_pos: Option<usize>,
    pub end_node_id: Option<usize>,
    pub end_char_pos: Option<usize>,
    pub is_selecting: bool,
    pub selected_text_buffer: String,
}

fn get_font() -> Option<&'static FontArc> {
    SYSTEM_FONT
        .get_or_init(|| {
            let font_paths = [
                "/System/Library/Fonts/Supplemental/Arial.ttf",
                "/System/Library/Fonts/Helvetica.ttc",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "C:\\Windows\\Fonts\\arial.ttf",
            ];
            for path in font_paths {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = FontArc::try_from_vec(bytes) {
                        return Some(font);
                    }
                }
            }
            None
        })
        .as_ref()
}

pub struct ScrollbarInfo {
    pub track_rect: Rect,
    pub thumb_rect: Rect,
}

/// Render the DOM into an existing pixmap, reusing its buffer across frames.
///
/// Allocating a fresh full-viewport `Pixmap` on every frame leaves many megabytes of
/// freed-but-cached allocations behind (a Retina window is 1800x1300 = ~9MB per frame),
/// so the caller keeps one pixmap alive and we only reallocate when the size changes.
pub fn render_dom_into_pixmap(
    pixmap: &mut Pixmap,
    dom: &Dom,
    width: u32,
    height: u32,
    scroll_y: f32,
    selection: &TextSelection,
    element_scrolls: &HashMap<usize, f32>,
    input_focus: Option<(usize, usize)>,
) -> (LayoutBox, Option<ScrollbarInfo>) {
    if pixmap.width() != width || pixmap.height() != height {
        if let Some(new_pixmap) = Pixmap::new(width, height) {
            *pixmap = new_pixmap;
        }
    }
    pixmap.fill(SkiaColor::from_rgba8(17, 17, 27, 255));

    let layout_root = build_layout_tree(dom, width, height, scroll_y);
    render_layout_box(
        &layout_root,
        pixmap,
        width,
        height,
        selection,
        element_scrolls,
        input_focus,
        RenderState::identity(),
    );

    // Calculate scrollbar info
    let content_height = layout_root.rect.height();
    let viewport_height = height as f32;

    let scrollbar_info = if content_height > viewport_height {
        let scrollbar_width = 12.0;
        let track_height = viewport_height;
        let thumb_height = (viewport_height / content_height * viewport_height).max(30.0);
        let max_scroll = content_height - viewport_height;
        let thumb_y = (scroll_y / max_scroll * (track_height - thumb_height))
            .clamp(0.0, track_height - thumb_height);

        // When the devtools are docked the page viewport ends at the sidebar's
        // left edge — the page scrollbar sits beside it, not underneath it.
        let dock_w =
            crate::layout::docked_devtools_width(dom, width as f32, height as f32).unwrap_or(0.0);
        let right_edge = (width as f32 - dock_w).max(scrollbar_width);

        let info = ScrollbarInfo {
            track_rect: Rect::from_xywh(
                right_edge - scrollbar_width,
                0.0,
                scrollbar_width,
                track_height,
            )
            .unwrap(),
            thumb_rect: Rect::from_xywh(
                right_edge - scrollbar_width,
                thumb_y,
                scrollbar_width,
                thumb_height,
            )
            .unwrap(),
        };

        // Render Scrollbar track
        let mut track_paint = Paint::default();
        track_paint.set_color(SkiaColor::from_rgba8(40, 40, 55, 255));
        pixmap.fill_rect(info.track_rect, &track_paint, Transform::identity(), None);

        // Render Scrollbar thumb
        let mut thumb_paint = Paint::default();
        thumb_paint.set_color(SkiaColor::from_rgba8(120, 120, 140, 255));
        pixmap.fill_rect(info.thumb_rect, &thumb_paint, Transform::identity(), None);

        Some(info)
    } else {
        None
    };

    (layout_root, scrollbar_info)
}

pub fn render_dom_to_pixmap(
    dom: &Dom,
    width: u32,
    height: u32,
    scroll_y: f32,
    selection: &TextSelection,
    _devtools_open: bool,
) -> (Pixmap, LayoutBox, Option<ScrollbarInfo>) {
    let mut pixmap = Pixmap::new(width, height).unwrap_or_else(|| Pixmap::new(800, 600).unwrap());
    let (layout_root, scrollbar_info) = render_dom_into_pixmap(
        &mut pixmap,
        dom,
        width,
        height,
        scroll_y,
        selection,
        &HashMap::new(),
        None,
    );
    (pixmap, layout_root, scrollbar_info)
}

/// Hit-test exact bounding rectangle from computed layout tree to return hovered cursor.
pub fn hit_test_cursor_from_layout(
    layout_root: &LayoutBox,
    mouse_x: f32,
    mouse_y: f32,
) -> CursorIcon {
    hit_test_cursor_from_layout_scrolled(layout_root, mouse_x, mouse_y, &HashMap::new())
}

/// Like [`hit_test_cursor_from_layout`], but honors element scroll offsets.
pub fn hit_test_cursor_from_layout_scrolled(
    layout_root: &LayoutBox,
    mouse_x: f32,
    mouse_y: f32,
    scrolls: &HashMap<usize, f32>,
) -> CursorIcon {
    if let Some(hovered_box) = hit_test_layout_box_scrolled(layout_root, mouse_x, mouse_y, scrolls)
    {
        if let Some(cursor_style) = hovered_box.style.cursor {
            match cursor_style {
                CursorStyle::Pointer => CursorIcon::Pointer,
                CursorStyle::Text => CursorIcon::Text,
                CursorStyle::Grab => CursorIcon::Grab,
                CursorStyle::Crosshair => CursorIcon::Crosshair,
                CursorStyle::NotAllowed => CursorIcon::NotAllowed,
                CursorStyle::Wait => CursorIcon::Wait,
                CursorStyle::Help => CursorIcon::Help,
                CursorStyle::ColResize => CursorIcon::ColResize,
                CursorStyle::RowResize => CursorIcon::RowResize,
                _ => CursorIcon::Default,
            }
        } else {
            CursorIcon::Default
        }
    } else {
        CursorIcon::Default
    }
}

/// Draw a devtools-style inspect highlight around the element under a point.
///
/// `strong` (inspect mode) fills the box with a translucent blue; otherwise a
/// subtle 2px outline marks the hovered element in the main section.
pub fn draw_hover_highlight(
    pixmap: &mut Pixmap,
    layout_root: &LayoutBox,
    x: f32,
    y: f32,
    strong: bool,
) {
    draw_hover_highlight_scrolled(pixmap, layout_root, x, y, strong, &HashMap::new())
}

/// Like [`draw_hover_highlight`], but honors element scroll offsets.
pub fn draw_hover_highlight_scrolled(
    pixmap: &mut Pixmap,
    layout_root: &LayoutBox,
    x: f32,
    y: f32,
    strong: bool,
    scrolls: &HashMap<usize, f32>,
) {
    let Some(box_node) = hit_test_layout_box_scrolled(layout_root, x, y, scrolls) else {
        return;
    };
    let r = box_node.rect;
    // Nothing useful to mark on the document shell or empty boxes.
    if matches!(box_node.tag, Tag::Document | Tag::Head | Tag::Title)
        || r.width() <= 0.0
        || r.height() <= 0.0
    {
        return;
    }
    // The hit-test already accounts for scroll offsets: the box rect is in
    // content coordinates, so translate by the accumulated scroll state.
    let (tx, ty) = box_screen_translation(box_node, layout_root, scrolls);
    draw_highlight_at(pixmap, r, tx, ty, strong);
}

/// The accumulated scroll translation (content → screen) for a box, computed
/// by walking from the root down to `b`. Used to draw highlights on boxes that
/// were located by id (e.g. a devtools-selected element) rather than by
/// hit-testing, where the scroll state is already folded in.
fn box_screen_translation(
    b: &LayoutBox,
    root: &LayoutBox,
    scrolls: &HashMap<usize, f32>,
) -> (f32, f32) {
    fn walk(
        node: &LayoutBox,
        target: usize,
        tx: f32,
        ty: f32,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<(f32, f32)> {
        if node.node_id == target {
            return Some((tx, ty));
        }
        let child = child_render_state(node, RenderState { tx, ty, clip: None }, scrolls);
        for c in &node.children {
            if let Some(hit) = walk(c, target, child.tx, child.ty, scrolls) {
                return Some(hit);
            }
        }
        None
    }
    walk(root, b.node_id, 0.0, 0.0, scrolls).unwrap_or((0.0, 0.0))
}

/// Draw the inspect highlight (optional translucent fill + 2px blue outline)
/// around a content-space rect shifted by `(tx, ty)` into screen space.
fn draw_highlight_at(pixmap: &mut Pixmap, r: Rect, tx: f32, ty: f32, strong: bool) {
    let left = r.left() + tx;
    let top = r.top() + ty;
    let rect = Rect::from_xywh(left, top, r.width(), r.height());
    let Some(rect) = rect else { return };

    if strong {
        let mut fill = Paint::default();
        fill.set_color(SkiaColor::from_rgba8(0, 120, 215, 55));
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(
            &path,
            &fill,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // 2px outline: four thin rects around the box.
    let lw = 2.0;
    let mut stroke = Paint::default();
    stroke.set_color(SkiaColor::from_rgba8(0, 120, 215, 235));
    for rect in [
        Rect::from_xywh(
            rect.left() - lw,
            rect.top() - lw,
            rect.width() + lw * 2.0,
            lw,
        ),
        Rect::from_xywh(rect.left() - lw, rect.bottom(), rect.width() + lw * 2.0, lw),
        Rect::from_xywh(rect.left() - lw, rect.top(), lw, rect.height()),
        Rect::from_xywh(rect.right(), rect.top(), lw, rect.height()),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuilder::from_rect(rect);
        pixmap.fill_path(
            &path,
            &stroke,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Draw the strong inspect highlight (fill + outline) around the element with
/// the given node id, honoring scroll offsets. Used by the pipeline to show the
/// devtools-selected element in the main section, mirroring pick mode's
/// hover preview.
pub fn draw_selected_highlight_scrolled(
    pixmap: &mut Pixmap,
    layout_root: &LayoutBox,
    node_id: usize,
    scrolls: &HashMap<usize, f32>,
) {
    let Some(box_node) = crate::layout::find_box_by_id(layout_root, node_id) else {
        return;
    };
    let r = box_node.rect;
    if matches!(box_node.tag, Tag::Document | Tag::Head | Tag::Title)
        || r.width() <= 0.0
        || r.height() <= 0.0
    {
        return;
    }
    let (tx, ty) = box_screen_translation(box_node, layout_root, scrolls);
    draw_highlight_at(pixmap, r, tx, ty, true);
}

pub fn hit_test_layout_box<'a>(box_node: &'a LayoutBox, mx: f32, my: f32) -> Option<&'a LayoutBox> {
    hit_test_layout_box_scrolled(box_node, mx, my, &HashMap::new())
}

/// Like [`hit_test_layout_box`], but honors element scroll offsets: content is
/// hit at its scrolled position and clipped to scroll containers.
pub fn hit_test_layout_box_scrolled<'a>(
    box_node: &'a LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<&'a LayoutBox> {
    fn walk<'a>(
        b: &'a LayoutBox,
        mx: f32,
        my: f32,
        state: RenderState,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<&'a LayoutBox> {
        if let Some(c) = state.clip {
            if mx < c.left() || mx > c.right() || my < c.top() || my > c.bottom() {
                return None;
            }
        }
        let child = child_render_state(b, state, scrolls);
        for child_box in b.children.iter().rev() {
            if let Some(hit) = walk(child_box, mx, my, child, scrolls) {
                return Some(hit);
            }
        }
        let r = b.rect;
        let rx = r.left() + state.tx;
        let ry = r.top() + state.ty;
        if mx >= rx && mx <= rx + r.width() && my >= ry && my <= ry + r.height() {
            Some(b)
        } else {
            None
        }
    }
    walk(box_node, mx, my, RenderState::identity(), scrolls)
}

/// Find the innermost scroll container under the cursor (for mouse-wheel
/// scrolling). Honors nested containers and scroll offsets.
pub fn find_scroll_container<'a>(
    root: &'a LayoutBox,
    mx: f32,
    my: f32,
    scrolls: &HashMap<usize, f32>,
) -> Option<&'a LayoutBox> {
    fn walk<'a>(
        b: &'a LayoutBox,
        mx: f32,
        my: f32,
        state: RenderState,
        scrolls: &HashMap<usize, f32>,
    ) -> Option<&'a LayoutBox> {
        if let Some(c) = state.clip {
            if mx < c.left() || mx > c.right() || my < c.top() || my > c.bottom() {
                return None;
            }
        }
        let child = child_render_state(b, state, scrolls);
        for child_box in b.children.iter().rev() {
            if let Some(hit) = walk(child_box, mx, my, child, scrolls) {
                return Some(hit);
            }
        }
        let r = b.rect;
        let rx = r.left() + state.tx;
        let ry = r.top() + state.ty;
        if is_scroll_container(b)
            && mx >= rx
            && mx <= rx + r.width()
            && my >= ry
            && my <= ry + r.height()
        {
            return Some(b);
        }
        None
    }
    walk(root, mx, my, RenderState::identity(), scrolls)
}
fn render_layout_box(
    box_node: &LayoutBox,
    pixmap: &mut Pixmap,
    viewport_width: u32,
    viewport_height: u32,
    selection: &TextSelection,
    scrolls: &HashMap<usize, f32>,
    input_focus: Option<(usize, usize)>,
    state: RenderState,
) {
    let vw = viewport_width as f32;
    let vh = viewport_height as f32;
    // Defaults mirror the layout engine: 3vh text, 2.5vh button font, 3.33vh
    // button padding (≈18/15/20px at a 600px-tall window, scaled up on taller
    // displays). `%` resolves against the box width.
    let resolve = |uv: Option<UnitValue>, default: UnitValue| {
        resolve_unit(uv.unwrap_or(default), box_node.rect.width(), vw, vh).unwrap_or(0.0)
    };
    // All lengths are exact device pixels — no viewport/DPR scaling.
    let sx = state.tx;
    let sy = state.ty;
    let clip = state.clip;

    // A screen-space helper that shifts a content rect by the accumulated
    // scroll translation and clips it to the current clip region.
    let shifted = |r: Rect| -> Option<Rect> {
        let moved = Rect::from_xywh(r.left() + sx, r.top() + sy, r.width(), r.height()).unwrap();
        intersect_opt(clip, moved)
    };

    match box_node.tag {
        Tag::Document | Tag::Head | Tag::Title => {}
        Tag::View => {
            if box_node.style.background_color != Color::TRANSPARENT {
                if let Some(r) = shifted(box_node.rect) {
                    let mut paint = Paint::default();
                    paint.set_color(to_skia_color(box_node.style.background_color));
                    let path = PathBuilder::from_rect(r);
                    pixmap.fill_path(
                        &path,
                        &paint,
                        tiny_skia::FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }

            // `border` (CSS) draws thin lines on the box rect edges — the
            // separators between devtools sections etc. Widths are exact pixels;
            // each side draws only when its width is non-zero. The box edges are
            // snapped to integer pixels so 1px lines render crisp instead of
            // antialiasing into a 2px blur (boxes can end on fractional edges).
            let bs = &box_node.style;
            if bs.border_top > 0.0
                || bs.border_right > 0.0
                || bs.border_bottom > 0.0
                || bs.border_left > 0.0
            {
                let (l, t, w, h) = (
                    (box_node.rect.left() + sx).round(),
                    (box_node.rect.top() + sy).round(),
                    box_node.rect.width().round(),
                    box_node.rect.height().round(),
                );
                let mut paint = Paint::default();
                paint.set_color(to_skia_color(bs.border_color));
                for r in [
                    Rect::from_xywh(l, t, w, bs.border_top),
                    Rect::from_xywh(l, t + h - bs.border_bottom, w, bs.border_bottom),
                    Rect::from_xywh(l, t, bs.border_left, h),
                    Rect::from_xywh(l + w - bs.border_right, t, bs.border_right, h),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(visible) = intersect_opt(clip, r) {
                        let path = PathBuilder::from_rect(visible);
                        pixmap.fill_path(
                            &path,
                            &paint,
                            tiny_skia::FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
        }
        Tag::Text => {
            if let Some(txt) = &box_node.text {
                let font_size = resolve(box_node.style.font_size, UnitValue::Vh(0.03));
                draw_text_ab_glyph(
                    pixmap,
                    txt,
                    box_node.rect.left(),
                    box_node.rect.top(),
                    font_size,
                    box_node.style.text_color,
                    box_node.node_id,
                    selection,
                    state,
                );
            }
        }
        Tag::Code => {
            // `<code>` draws its text with real JS syntax highlighting: the
            // text is tokenized and each token is drawn with its own color at
            // its own advance (strings orange, numbers green, keywords blue,
            // comments gray). The box's own `color` (if set) becomes the base
            // color for identifiers/operators — e.g. red error lines in the
            // devtools console stay red while their tokens are highlighted.
            if let Some(txt) = &box_node.text {
                let font_size = resolve(box_node.style.font_size, UnitValue::Vh(0.03));
                let base = if box_node.style.text_color != Color::WHITE {
                    box_node.style.text_color
                } else {
                    Color {
                        r: 212,
                        g: 212,
                        b: 212,
                        a: 255,
                    }
                };
                let mut x = box_node.rect.left();
                let y = box_node.rect.top();
                for (token, color) in js_tokenize(txt, base) {
                    if token.is_empty() {
                        continue;
                    }
                    x += draw_text_ab_glyph(
                        pixmap,
                        &token,
                        x,
                        y,
                        font_size,
                        color,
                        box_node.node_id,
                        selection,
                        state,
                    );
                }
            }
        }
        Tag::Button => {
            let btn_txt = box_node.text.as_deref().unwrap_or("Button");
            // Mirror the layout: buttons size from `font-size`/`padding`.
            let font_size = resolve(box_node.style.font_size, UnitValue::Vh(0.025));
            let _pad = resolve(box_node.style.padding, UnitValue::Vh(0.0333));

            let mut paint = Paint::default();
            let bg = if box_node.style.background_color != Color::TRANSPARENT {
                box_node.style.background_color
            } else {
                Color {
                    r: 166,
                    g: 227,
                    b: 161,
                    a: 255,
                }
            };
            paint.set_color(to_skia_color(bg));

            if let Some(r) = shifted(box_node.rect) {
                let path = PathBuilder::from_rect(r);
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }

            let label_color = if box_node.style.text_color != Color::WHITE {
                box_node.style.text_color
            } else {
                Color::BLACK
            };

            // Center text both horizontally and vertically within the button rect
            let text_w = measure_text(btn_txt, font_size);
            let text_left = box_node.rect.left() + (box_node.rect.width() - text_w) / 2.0;
            let text_top = box_node.rect.top() + (box_node.rect.height() - font_size) / 2.0;

            draw_text_ab_glyph(
                pixmap,
                btn_txt,
                text_left,
                text_top,
                font_size,
                label_color,
                box_node.node_id,
                selection,
                state,
            );
        }
        Tag::Icon => {
            // Feather icons render as strokes (round cap/join) in the element's
            // `color`, scaled from the 24x24 viewBox to the box size.
            let Some(name) = box_node.icon.as_deref() else {
                return;
            };
            let Some(path) = cached_icon_path(name) else {
                return;
            };
            let size = box_node.rect.width().min(box_node.rect.height());
            if size <= 0.0 {
                return;
            }
            let scale = size / 24.0;
            // Center the icon within its box
            let offset_x = (box_node.rect.width() - size) / 2.0;
            let offset_y = (box_node.rect.height() - size) / 2.0;
            let ts = Transform::from_scale(scale, scale).post_translate(
                box_node.rect.left() + sx + offset_x,
                box_node.rect.top() + sy + offset_y,
            );
            let stroke = tiny_skia::Stroke {
                width: (2.0 * scale).max(0.5),
                miter_limit: 4.0,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                dash: None,
            };
            let color = if box_node.style.text_color != Color::WHITE {
                box_node.style.text_color
            } else {
                Color {
                    r: 166,
                    g: 227,
                    b: 161,
                    a: 255,
                }
            };
            if let Some(stroked) = path.stroke(&stroke, 1.0) {
                if let Some(moved) = stroked.transform(ts) {
                    let mut paint = Paint::default();
                    paint.set_color(to_skia_color(color));
                    // Clip the icon to its box like every other element.
                    let box_rect = Rect::from_xywh(
                        box_node.rect.left() + sx,
                        box_node.rect.top() + sy,
                        box_node.rect.width(),
                        box_node.rect.height(),
                    )
                    .unwrap();
                    let clip_rect = intersect_opt(clip, box_rect);
                    // `moved` is already in screen/pixel space (the box rect +
                    // scroll translation were baked in by `ts`), so the fill
                    // transform is identity — otherwise the path is scaled and
                    // shifted a second time and the icon lands off-position.
                    if let Some(r) = clip_rect {
                        let mut mask =
                            tiny_skia::Mask::new(pixmap.width(), pixmap.height()).unwrap();
                        let cp = PathBuilder::from_rect(r);
                        mask.fill_path(
                            &cp,
                            tiny_skia::FillRule::Winding,
                            false,
                            Transform::identity(),
                        );
                        pixmap.fill_path(
                            &moved,
                            &paint,
                            tiny_skia::FillRule::Winding,
                            Transform::identity(),
                            Some(&mask),
                        );
                    } else {
                        pixmap.fill_path(
                            &moved,
                            &paint,
                            tiny_skia::FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
        }
        Tag::Input => {
            if let Some(r) = shifted(box_node.rect) {
                let border_color = SkiaColor::from_rgba8(120, 120, 130, 255);
                let bg = if box_node.style.background_color != Color::TRANSPARENT {
                    to_skia_color(box_node.style.background_color)
                } else {
                    SkiaColor::from_rgba8(250, 250, 252, 255)
                };
                let mut paint = Paint::default();
                paint.set_color(border_color);
                pixmap.fill_rect(r, &paint, Transform::identity(), None);
                // Inner fill inset by 1px = the border.
                let inner = Rect::from_xywh(
                    r.left() + 1.0,
                    r.top() + 1.0,
                    (r.width() - 2.0).max(1.0),
                    (r.height() - 2.0).max(1.0),
                )
                .unwrap();
                paint.set_color(bg);
                pixmap.fill_rect(inner, &paint, Transform::identity(), None);

                // Value (or gray placeholder) drawn inside, clipped to the box so
                // long values scroll visually instead of overflowing the field.
                let font_size = resolve(box_node.style.font_size, UnitValue::Vh(0.025));
                let pad = resolve(box_node.style.padding, UnitValue::Vh(0.012));
                let value = box_node.value.as_deref().unwrap_or("");
                let placeholder = box_node.placeholder.as_deref().unwrap_or("");
                let is_focused = input_focus
                    .map(|(id, _)| id == box_node.node_id)
                    .unwrap_or(false);

                let mut text_state = state;
                text_state.clip = intersect_opt(text_state.clip, inner);
                let (text, color) = if !value.is_empty() {
                    let bg_color = Color {
                        r: (bg.red() * 255.0) as u8,
                        g: (bg.green() * 255.0) as u8,
                        b: (bg.blue() * 255.0) as u8,
                        a: 255,
                    };
                    let c = if box_node.style.text_color != Color::WHITE {
                        box_node.style.text_color
                    } else {
                        input_auto_text_color(&bg_color)
                    };
                    (value, c)
                } else if !placeholder.is_empty() {
                    (
                        placeholder,
                        Color {
                            r: 130,
                            g: 130,
                            b: 140,
                            a: 255,
                        },
                    )
                } else {
                    ("", box_node.style.text_color)
                };
                let text_left = r.left() + pad + 1.0;
                let text_top = r.top() + pad;
                if !text.is_empty() {
                    draw_text_ab_glyph(
                        pixmap,
                        text,
                        text_left,
                        text_top,
                        font_size,
                        color,
                        box_node.node_id,
                        selection,
                        text_state,
                    );
                }
                // Caret: a 2px bar at the caret index (blinking is skipped to
                // avoid a per-frame timer waking the app).
                if is_focused {
                    let caret = input_focus.map(|(_, c)| c).unwrap_or(0);
                    let caret_x = text_left + caret_text_width(value, font_size, caret);
                    let caret_h = (font_size * 1.2).max(8.0);
                    let caret_top = r.top() + ((r.height() - caret_h) / 2.0).max(0.0);
                    let caret_rect = Rect::from_xywh(caret_x, caret_top, 2.0, caret_h).unwrap();
                    let mut paint = Paint::default();
                    let bg_color = Color {
                        r: (bg.red() * 255.0) as u8,
                        g: (bg.green() * 255.0) as u8,
                        b: (bg.blue() * 255.0) as u8,
                        a: 255,
                    };
                    paint.set_color(to_skia_color(input_auto_text_color(&bg_color)));
                    if let Some(visible) = intersect_opt(text_state.clip, caret_rect) {
                        pixmap.fill_rect(visible, &paint, Transform::identity(), None);
                    }
                }
            }
        }
        _ => {}
    }

    // Children inherit the scroll translate + clip of this box.
    let child_state = child_render_state(box_node, state, scrolls);
    for child in &box_node.children {
        render_layout_box(
            child,
            pixmap,
            viewport_width,
            viewport_height,
            selection,
            scrolls,
            input_focus,
            child_state,
        );
    }

    // The scrollbar draws last, on top of the (clipped) content.
    if is_scroll_container(box_node) {
        if let Some(r) = shifted(box_node.rect) {
            draw_scrollbar(
                pixmap,
                r,
                box_node.content_height,
                scroll_offset_for(box_node, scrolls),
            );
        }
    }
}

/// Width of the first `caret` chars of an input value at `font_size` (the x
/// offset of the caret), matching the renderer's per-glyph advance stepping.
fn caret_text_width(value: &str, font_size: f32, caret: usize) -> f32 {
    let prefix: String = value.chars().take(caret).collect();
    crate::layout::measure_text(&prefix, font_size)
}

/// Chrome-DevTools-dark-palette token colors for `<code>` highlighting.
const KEYWORD_COLOR: Color = Color {
    r: 86,
    g: 156,
    b: 214,
    a: 255,
}; // #569cd6
const STRING_COLOR: Color = Color {
    r: 206,
    g: 145,
    b: 120,
    a: 255,
}; // #ce9178
const NUMBER_COLOR: Color = Color {
    r: 181,
    g: 206,
    b: 168,
    a: 255,
}; // #b5cea8
const COMMENT_COLOR: Color = Color {
    r: 106,
    g: 153,
    b: 85,
    a: 255,
}; // #6a9955

/// Tokenize JS-ish text into `(token, color)` runs. Identifiers, operators and
/// punctuation use `base` (the element's own text color); keywords, strings,
/// numbers and comments get the fixed syntax palette.
fn js_tokenize(text: &str, base: Color) -> Vec<(String, Color)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            out.push((chars[start..i].iter().collect(), base));
            continue;
        }
        // Strings ('...', "...", `...`), honoring backslash escapes.
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((chars[start..i.min(n)].iter().collect(), STRING_COLOR));
            continue;
        }
        // Line comments.
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            out.push((chars[start..i].iter().collect(), COMMENT_COLOR));
            continue;
        }
        // Block comments.
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            out.push((chars[start..i].iter().collect(), COMMENT_COLOR));
            continue;
        }
        // Numbers (decimal, hex, floats).
        if c.is_ascii_digit() || (c == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if c == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
            } else {
                while i < n
                    && (chars[i].is_ascii_digit()
                        || chars[i] == '.'
                        || chars[i] == '_'
                        || chars[i] == 'e'
                        || chars[i] == 'E'
                        || ((chars[i] == '+' || chars[i] == '-')
                            && i > start
                            && (chars[i - 1] == 'e' || chars[i - 1] == 'E')))
                {
                    i += 1;
                }
            }
            out.push((chars[start..i].iter().collect(), NUMBER_COLOR));
            continue;
        }
        // Identifiers / keywords.
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if is_js_keyword(&word) {
                out.push((word, KEYWORD_COLOR));
            } else {
                out.push((word, base));
            }
            continue;
        }
        // Everything else: operators / punctuation, one char at a time.
        out.push((c.to_string(), base));
        i += 1;
    }
    out
}

fn is_js_keyword(word: &str) -> bool {
    matches!(
        word,
        "var"
            | "let"
            | "const"
            | "function"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "break"
            | "continue"
            | "new"
            | "class"
            | "extends"
            | "super"
            | "this"
            | "typeof"
            | "instanceof"
            | "in"
            | "of"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "delete"
            | "void"
            | "yield"
            | "async"
            | "await"
            | "import"
            | "export"
            | "default"
            | "static"
            | "get"
            | "set"
            | "null"
            | "undefined"
            | "true"
            | "false"
            | "NaN"
            | "Infinity"
    )
}

fn draw_text_ab_glyph(
    pixmap: &mut Pixmap,
    text: &str,
    start_x: f32,
    start_y: f32,
    size: f32,
    color: Color,
    node_id: usize,
    selection: &TextSelection,
    state: RenderState,
) -> f32 {
    let sx = state.tx;
    let sy = state.ty;
    // A point is visible when it lands inside the current clip region (screen
    // coordinates). `None` means no clipping.
    let visible = |px: f32, py: f32| -> bool {
        match state.clip {
            Some(c) => px >= c.left() && px <= c.right() && py >= c.top() && py <= c.bottom(),
            None => true,
        }
    };

    if let Some(font) = get_font() {
        let scale = PxScale::from(size);
        let mut x_offset = start_x;
        let baseline_y = start_y + size * 0.8;

        for (char_index, ch) in text.chars().enumerate() {
            let glyph_id = font.glyph_id(ch);
            let glyph: Glyph =
                glyph_id.with_scale_and_position(scale, ab_glyph::point(x_offset, baseline_y));

            // Same per-glyph stepping as the layout's `measure_text_width`, so a
            // text box's width always matches what is actually drawn.
            let advance = crate::layout::glyph_advance_px(font, glyph_id, size);

            let is_char_selected = is_point_selected(node_id, char_index, selection);

            if is_char_selected {
                if let Some(sel_rect) = intersect_opt(
                    state.clip,
                    Rect::from_xywh(x_offset + sx, start_y + sy, advance, size * 1.1).unwrap(),
                ) {
                    let mut sel_paint = Paint::default();
                    sel_paint.set_color(SkiaColor::from_rgba8(74, 118, 168, 200));
                    let sel_path = PathBuilder::from_rect(sel_rect);
                    pixmap.fill_path(
                        &sel_path,
                        &sel_paint,
                        tiny_skia::FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }

            let draw_color = if is_char_selected {
                Color::WHITE
            } else {
                color
            };

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    if coverage > 0.02 {
                        let xf = bounds.min.x as f32 + gx as f32 + sx;
                        let yf = bounds.min.y as f32 + gy as f32 + sy;
                        if visible(xf, yf) {
                            let px = xf as u32;
                            let py = yf as u32;
                            if px < pixmap.width() && py < pixmap.height() {
                                blend_pixel(pixmap, px, py, draw_color, coverage);
                            }
                        }
                    }
                });
            }
            x_offset += advance;
        }
        // Return the advance width actually consumed so callers that chain
        // draws (the syntax-highlighted `<code>` renderer) place the next
        // token right after this one instead of overlapping it.
        x_offset - start_x
    } else {
        let mut x_offset = start_x;
        let mut paint = Paint::default();
        paint.set_color(to_skia_color(color));

        for ch in text.chars() {
            if ch.is_whitespace() {
                x_offset += size * 0.4;
                continue;
            }

            let char_w = size * 0.55;
            let char_h = size * 0.8;

            if let Some(r) = intersect_opt(
                state.clip,
                Rect::from_xywh(x_offset + sx, start_y + sy, char_w * 0.75, char_h).unwrap(),
            ) {
                let path = PathBuilder::from_rect(r);
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }

            x_offset += char_w + 2.0;
        }
        // Same contract as the font path: the advance width consumed.
        x_offset - start_x
    }
}

/// Draw a vertical scrollbar (track + thumb) for a scroll container at its
/// screen rect. The thumb size/position reflect the visible fraction.
fn draw_scrollbar(pixmap: &mut Pixmap, rect: Rect, content_height: f32, scroll_offset: f32) {
    let inner_h = rect.height();
    if content_height <= inner_h + 1.0 {
        return;
    }
    let track_w = 8.0;
    let track_x = rect.right() - track_w;
    let track = Rect::from_xywh(track_x, rect.top(), track_w, inner_h).unwrap();

    let mut track_paint = Paint::default();
    track_paint.set_color(SkiaColor::from_rgba8(38, 38, 40, 255));
    let track_path = PathBuilder::from_rect(track);
    pixmap.fill_path(
        &track_path,
        &track_paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );

    let thumb_h = (inner_h * inner_h / content_height).clamp(16.0, inner_h);
    let max_scroll = (content_height - inner_h).max(0.0);
    let frac = if max_scroll > 0.0 {
        (scroll_offset / max_scroll).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_y = rect.top() + frac * (inner_h - thumb_h);
    let thumb = Rect::from_xywh(track_x, thumb_y, track_w, thumb_h).unwrap();

    let mut thumb_paint = Paint::default();
    thumb_paint.set_color(SkiaColor::from_rgba8(90, 90, 95, 255));
    let thumb_path = PathBuilder::from_rect(thumb);
    pixmap.fill_path(
        &thumb_path,
        &thumb_paint,
        tiny_skia::FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// Whether the character at `(node_id, char_index)` falls inside the selection range.
///
/// Handles reversed ranges (dragging left/up) by normalizing to a forward range.
/// The intermediate-node test assumes node ids are assigned in document order,
/// which the bundler guarantees within a document.
fn is_point_selected(node_id: usize, char_index: usize, selection: &TextSelection) -> bool {
    let (Some(start_node), Some(start_char)) = (selection.start_node_id, selection.start_char_pos)
    else {
        return false;
    };
    let (Some(end_node), Some(end_char)) = (selection.end_node_id, selection.end_char_pos) else {
        return false;
    };

    let (s_node, s_char, e_node, e_char) = if (start_node, start_char) <= (end_node, end_char) {
        (start_node, start_char, end_node, end_char)
    } else {
        (end_node, end_char, start_node, start_char)
    };

    if node_id == s_node && node_id == e_node {
        char_index >= s_char && char_index < e_char
    } else if node_id == s_node {
        char_index >= s_char
    } else if node_id == e_node {
        char_index < e_char
    } else {
        node_id > s_node && node_id < e_node
    }
}

fn blend_pixel(pixmap: &mut Pixmap, x: u32, y: u32, color: Color, coverage: f32) {
    let width = pixmap.width();
    let pixels = pixmap.pixels_mut();
    let idx = (y * width + x) as usize;
    if idx < pixels.len() {
        let existing = pixels[idx];
        let dst_r = existing.red() as f32;
        let dst_g = existing.green() as f32;
        let dst_b = existing.blue() as f32;
        let dst_a = existing.alpha() as f32 / 255.0;

        let font_alpha = (color.a as f32 / 255.0) * coverage;

        let out_r = (color.r as f32 * font_alpha + dst_r * (1.0 - font_alpha)).min(255.0) as u8;
        let out_g = (color.g as f32 * font_alpha + dst_g * (1.0 - font_alpha)).min(255.0) as u8;
        let out_b = (color.b as f32 * font_alpha + dst_b * (1.0 - font_alpha)).min(255.0) as u8;
        let out_a = ((font_alpha + dst_a * (1.0 - font_alpha)) * 255.0).min(255.0) as u8;

        let c = ColorU8::from_rgba(out_r, out_g, out_b, out_a);
        pixels[idx] = c.premultiply();
    }
}

fn to_skia_color(c: Color) -> SkiaColor {
    SkiaColor::from_rgba8(c.r, c.g, c.b, c.a)
}

/// Relative luminance (simplified sRGB). Returns 0.0 (black) – 1.0 (white).
fn srgb_luminance(c: &Color) -> f32 {
    let r = c.r as f32 / 255.0;
    let g = c.g as f32 / 255.0;
    let b = c.b as f32 / 255.0;
    // Rec. 709 coefficients.
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Pick black or white text to contrast against `bg`.
fn input_auto_text_color(bg: &Color) -> Color {
    if srgb_luminance(bg) > 0.4 {
        Color {
            r: 17,
            g: 17,
            b: 17,
            a: 255,
        } // dark text on light bg
    } else {
        Color {
            r: 240,
            g: 240,
            b: 245,
            a: 255,
        } // light text on dark bg
    }
}

/// Draw a small tooltip box near the mouse when the hovered element has a
/// `title` attribute.  Renders last (on top of everything).
pub fn draw_tooltip(
    pixmap: &mut Pixmap,
    title: &str,
    mx: f32,
    my: f32,
    viewport_w: u32,
    viewport_h: u32,
) {
    if title.is_empty() {
        return;
    }
    let font_size = (viewport_h as f32 * 0.03).clamp(16.0, 34.0);
    let text_w = crate::layout::measure_text(title, font_size);
    let pad = font_size * 0.5;
    let tip_w = (text_w + pad * 2.0).min(viewport_w as f32 - 20.0);
    let tip_h = font_size + pad * 2.0;
    // Position below and to the right of the cursor, clamped to the viewport.
    let mut tip_x = mx + 12.0;
    let mut tip_y = my + 20.0;
    if tip_x + tip_w > viewport_w as f32 - 4.0 {
        tip_x = (viewport_w as f32 - tip_w - 4.0).max(4.0);
    }
    if tip_y + tip_h > viewport_h as f32 - 4.0 {
        tip_y = (my - tip_h - 4.0).max(4.0);
    }
    let tip_rect = Rect::from_xywh(tip_x, tip_y, tip_w, tip_h).unwrap();
    // Background.
    {
        let mut paint = Paint::default();
        paint.set_color(SkiaColor::from_rgba8(40, 40, 40, 240));
        pixmap.fill_rect(tip_rect, &paint, Transform::identity(), None);
    }
    // Border (four 1px rects — tiny_skia Paint has no stroke mode).
    {
        let mut paint = Paint::default();
        paint.set_color(SkiaColor::from_rgba8(80, 80, 80, 255));
        let t = 1.0;
        // top
        if let Some(r) = Rect::from_xywh(tip_x, tip_y, tip_w, t) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
        // bottom
        if let Some(r) = Rect::from_xywh(tip_x, tip_y + tip_h - t, tip_w, t) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
        // left
        if let Some(r) = Rect::from_xywh(tip_x, tip_y, t, tip_h) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
        // right
        if let Some(r) = Rect::from_xywh(tip_x + tip_w - t, tip_y, t, tip_h) {
            pixmap.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
    // Text.
    draw_text_ab_glyph(
        pixmap,
        title,
        tip_x + pad,
        tip_y + pad,
        font_size,
        Color {
            r: 230,
            g: 230,
            b: 230,
            a: 255,
        },
        0,
        &TextSelection::default(),
        RenderState::identity(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_draw_hover_highlight_paints() {
        // A simple View with a known rect; hovering inside it must paint highlight
        // pixels, and a point outside must not.
        let root = crate::layout::LayoutBox {
            node_id: 1,
            tag: Tag::Document,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            style: Default::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![crate::layout::LayoutBox {
                node_id: 2,
                tag: Tag::View,
                element_id: None,
                rect: Rect::from_xywh(10.0, 10.0, 50.0, 50.0).unwrap(),
                style: Default::default(),
                text: None,
                value: None,
                placeholder: None,
                icon: None,
                children: vec![],
                overflow: pwb_core::spec::OverflowStyle::Visible,
                content_height: 50.0,
            }],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 100.0,
        };
        let mut pixmap = Pixmap::new(100, 100).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        draw_hover_highlight(&mut pixmap, &root, 30.0, 30.0, false);
        // The 2px outline band (x in 8..10) along the box's left edge should be blue.
        let c = pixmap.pixel(9, 20).unwrap();
        assert!(c.blue() > 150, "outline should be blue-ish, got {:?}", c);
        // Corner outside the box stays untouched (black).
        let c2 = pixmap.pixel(2, 2).unwrap();
        assert!(c2.red() < 10 && c2.green() < 10 && c2.blue() < 10);

        // Strong (inspect) mode fills the box interior.
        let mut pixmap2 = Pixmap::new(100, 100).unwrap();
        pixmap2.fill(SkiaColor::BLACK);
        draw_hover_highlight(&mut pixmap2, &root, 30.0, 30.0, true);
        let interior = pixmap2.pixel(30, 30).unwrap();
        assert!(
            interior.blue() > 30,
            "inspect fill should be blue-tinted, got {:?}",
            interior
        );
    }

    #[test]
    fn test_draw_selected_highlight_by_node_id() {
        // A View with a known rect; `draw_selected_highlight_scrolled` must
        // paint the outline + tint around that element given its node id.
        let root = crate::layout::LayoutBox {
            node_id: 1,
            tag: Tag::Document,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            style: Default::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![crate::layout::LayoutBox {
                node_id: 2,
                tag: Tag::View,
                element_id: None,
                rect: Rect::from_xywh(10.0, 10.0, 50.0, 50.0).unwrap(),
                style: Default::default(),
                text: None,
                value: None,
                placeholder: None,
                icon: None,
                children: vec![],
                overflow: pwb_core::spec::OverflowStyle::Visible,
                content_height: 50.0,
            }],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 100.0,
        };
        let mut pixmap = Pixmap::new(100, 100).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        draw_selected_highlight_scrolled(&mut pixmap, &root, 2, &HashMap::new());
        // 2px outline band along the box's left edge (x 8..10) is blue.
        let c = pixmap.pixel(9, 20).unwrap();
        assert!(c.blue() > 150, "outline should be blue-ish, got {:?}", c);
        // The strong fill tints the interior.
        let interior = pixmap.pixel(30, 30).unwrap();
        assert!(
            interior.blue() > 30,
            "selected fill should be blue-tinted, got {:?}",
            interior
        );
        // Unknown id: nothing painted.
        let mut pixmap2 = Pixmap::new(100, 100).unwrap();
        pixmap2.fill(SkiaColor::BLACK);
        draw_selected_highlight_scrolled(&mut pixmap2, &root, 999, &HashMap::new());
        let bg = pixmap2.pixel(30, 30).unwrap();
        assert!(bg.blue() < 10, "unknown id paints nothing, got {:?}", bg);
    }

    #[test]
    fn test_scroll_container_hit_test_and_finder() {
        use pwb_core::spec::OverflowStyle;
        // A 200x100 scroll container with 400px of content (four 100px rows).
        let child = |id: usize, y: f32| crate::layout::LayoutBox {
            node_id: id,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(0.0, y, 200.0, 100.0).unwrap(),
            style: Default::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: OverflowStyle::Visible,
            content_height: 100.0,
        };
        let container = crate::layout::LayoutBox {
            node_id: 10,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 200.0, 100.0).unwrap(),
            style: Default::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![
                child(11, 0.0),
                child(12, 100.0),
                child(13, 200.0),
                child(14, 300.0),
            ],
            overflow: OverflowStyle::Scroll,
            content_height: 400.0,
        };
        let root = crate::layout::LayoutBox {
            node_id: 1,
            tag: Tag::Document,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 400.0, 400.0).unwrap(),
            style: Default::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![container],
            overflow: OverflowStyle::Visible,
            content_height: 400.0,
        };

        // Unscrolled: only the first 100px of content is visible (clipped);
        // y=50 hits the first row, and content below the container is not.
        let empty = HashMap::new();
        assert_eq!(
            hit_test_layout_box_scrolled(&root, 50.0, 50.0, &empty).map(|b| b.node_id),
            Some(11)
        );
        let below = hit_test_layout_box_scrolled(&root, 50.0, 250.0, &empty).map(|b| b.node_id);
        assert!(
            below.is_none() || below == Some(1),
            "content below the clipped container must not be hit, got {:?}",
            below
        );

        // Scrolled 150px down: content shifts up, so y=50 now hits the third row
        // (natural y=200 → screen 50..150); the last row has scrolled past the
        // container and is clipped away.
        let mut scrolls = HashMap::new();
        scrolls.insert(10, 150.0);
        assert_eq!(
            hit_test_layout_box_scrolled(&root, 50.0, 50.0, &scrolls).map(|b| b.node_id),
            Some(13)
        );
        let past = hit_test_layout_box_scrolled(&root, 50.0, 250.0, &scrolls).map(|b| b.node_id);
        assert!(
            past.is_none() || past == Some(1),
            "scrolled-past content must be clipped, got {:?}",
            past
        );

        // The wheel-scroll finder returns the container when hovering it...
        assert_eq!(
            find_scroll_container(&root, 50.0, 50.0, &scrolls).map(|b| b.node_id),
            Some(10)
        );
        // ...but not outside it.
        assert!(find_scroll_container(&root, 250.0, 50.0, &scrolls).is_none());
    }

    #[test]
    fn test_scroll_container_renders_scrollbar_and_clips() {
        use crate::style::ComputedStyle;
        use pwb_core::spec::{Color, OverflowStyle};
        // A 100x50 scroll container holding one red 100px-tall row (overflows).
        let mut child_style = ComputedStyle::default();
        child_style.background_color = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let child = crate::layout::LayoutBox {
            node_id: 2,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            style: child_style,
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: OverflowStyle::Visible,
            content_height: 100.0,
        };
        let container = crate::layout::LayoutBox {
            node_id: 1,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(0.0, 0.0, 100.0, 50.0).unwrap(),
            style: ComputedStyle::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![child],
            overflow: OverflowStyle::Scroll,
            content_height: 100.0,
        };
        let mut pixmap = Pixmap::new(100, 60).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &container,
            &mut pixmap,
            100,
            60,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        // The red row fills the visible 0..50 region (clipped to the container).
        let inside = pixmap.pixel(50, 25).unwrap();
        assert!(
            inside.red() > 200 && inside.green() < 60,
            "content should paint, got {:?}",
            inside
        );
        // Content below the container is clipped away.
        let below = pixmap.pixel(50, 55).unwrap();
        assert!(
            below.red() < 10,
            "overflow must be clipped, got {:?}",
            below
        );
        // The scrollbar track + thumb render along the right edge.
        let track = pixmap.pixel(96, 10).unwrap();
        assert!(
            track.red() < 100 && track.blue() < 100 && track.green() < 100,
            "track should be gray, got {:?}",
            track
        );
    }

    #[test]
    fn test_is_point_selected() {
        let selection = TextSelection {
            start_node_id: Some(1),
            start_char_pos: Some(0),
            end_node_id: Some(1),
            end_char_pos: Some(5),
            is_selecting: false,
            selected_text_buffer: String::new(),
        };

        // Should select chars 0-4 in node 1
        assert!(is_point_selected(1, 2, &selection));
        assert!(is_point_selected(1, 0, &selection));
        assert!(is_point_selected(1, 4, &selection));
        // Should not select char 5 (at end boundary) or chars in other nodes
        assert!(!is_point_selected(1, 5, &selection));
        assert!(!is_point_selected(2, 0, &selection));

        // Reversed range (drag left/up) still selects the same characters.
        let reversed = TextSelection {
            start_node_id: Some(1),
            start_char_pos: Some(5),
            end_node_id: Some(1),
            end_char_pos: Some(0),
            is_selecting: false,
            selected_text_buffer: String::new(),
        };
        assert!(is_point_selected(1, 2, &reversed));
        assert!(!is_point_selected(1, 5, &reversed));
    }

    #[test]
    fn test_icon_renders_stroked_pixels() {
        use crate::style::ComputedStyle;
        use pwb_core::spec::Color as SpecColor;
        // A green `home` icon in a 48x48 box must paint stroke pixels in its
        // color and nothing outside the box.
        let mut style = ComputedStyle::default();
        style.text_color = SpecColor {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        let icon = crate::layout::LayoutBox {
            node_id: 7,
            tag: Tag::Icon,
            element_id: None,
            rect: Rect::from_xywh(10.0, 10.0, 48.0, 48.0).unwrap(),
            style,
            text: None,
            value: None,
            placeholder: None,
            icon: Some("home".to_string()),
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 48.0,
        };
        let mut pixmap = Pixmap::new(120, 120).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &icon,
            &mut pixmap,
            120,
            120,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        // The home icon's roofline spans the full box width near the top, and
        // must be painted exactly inside the box rect (the path transform must
        // not be applied twice — that would scale/shift the icon off-position).
        let green = (10..58)
            .flat_map(|x| (10..58).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let c = pixmap.pixel(x, y).unwrap();
                c.green() > 150 && c.red() < 100 && c.blue() < 100
            })
            .count();
        assert!(green > 100, "icon strokes painted, got {} green px", green);
        // The roofline is at the top of the box: a horizontal run of green near
        // y=14 spanning most of the box width.
        let roofline = (12..56)
            .filter(|&x| {
                let c = pixmap.pixel(x, 14).unwrap();
                c.green() > 150 && c.red() < 100
            })
            .count();
        assert!(
            roofline > 8,
            "roofline drawn at the box top, got {} green px",
            roofline
        );
        // Nothing leaks outside the box (e.g. the icon scaled past its rect).
        let outside = pixmap.pixel(0, 0).unwrap();
        assert!(
            outside.green() < 10,
            "outside the box stays black, got {:?}",
            outside
        );
        let past = pixmap.pixel(70, 30).unwrap();
        assert!(
            past.green() < 10,
            "no green past the box's right edge, got {:?}",
            past
        );
    }

    #[test]
    fn test_icon_centered_in_rectangular_box() {
        use crate::style::ComputedStyle;
        use pwb_core::spec::Color as SpecColor;
        // Test that an icon is centered in a rectangular box (taller than wide).
        // The icon should be horizontally centered.
        let mut style = ComputedStyle::default();
        style.text_color = SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        // 40px wide, 100px tall box - icon should be centered horizontally
        let icon = crate::layout::LayoutBox {
            node_id: 8,
            tag: Tag::Icon,
            element_id: None,
            rect: Rect::from_xywh(10.0, 10.0, 40.0, 100.0).unwrap(),
            style,
            text: None,
            value: None,
            placeholder: None,
            icon: Some("home".to_string()),
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 100.0,
        };
        let mut pixmap = Pixmap::new(60, 120).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &icon,
            &mut pixmap,
            60,
            120,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        // The icon size is min(40, 100) = 40px, so it should be 40x40
        // Scale = 40/24 = 1.667
        // Horizontally centered: icon left at x = 10 + (40-40)/2 = 10
        // Vertically centered: icon top at y = 10 + (100-40)/2 = 40
        // Home icon roofline is at y=12 in 24x24 viewBox -> 12 * 1.667 = 20px from icon top
        // So roofline at y = 40 + 20 = 60
        // Roof peak is at y=2 in viewBox -> 2 * 1.667 = 3.33px from icon top -> y = 43.33
        // Icon center x = 10 + 40/2 = 30

        // Check roofline is at y=60 (horizontally centered within the box)
        let roofline_center = (55..65)
            .filter(|&y| {
                // around y=60
                let c = pixmap.pixel(30, y).unwrap(); // x=30 is center of icon (10+40/2)
                c.red() > 150 && c.green() < 100
            })
            .count();
        assert!(
            roofline_center > 5,
            "roofline drawn at horizontally centered position, got {} red px",
            roofline_center
        );

        // Check no red pixels at the left of the box (x=10) - icon should not be at left edge
        let left_edge = (40..80)
            .filter(|&y| {
                let c = pixmap.pixel(10, y).unwrap();
                c.red() > 150
            })
            .count();
        assert!(
            left_edge == 0,
            "no icon at left edge of box, got {} red px",
            left_edge
        );

        // Check no red pixels at the right of the box (x=48) - icon should not be at right edge
        let right_edge = (40..80)
            .filter(|&y| {
                let c = pixmap.pixel(48, y).unwrap();
                c.red() > 150
            })
            .count();
        assert!(
            right_edge == 0,
            "no icon at right edge of box, got {} red px",
            right_edge
        );

        // Nothing leaks outside the box
        let outside = pixmap.pixel(0, 0).unwrap();
        assert!(
            outside.red() < 10,
            "outside the box stays black, got {:?}",
            outside
        );
        let past_bottom = pixmap.pixel(30, 115).unwrap();
        assert!(
            past_bottom.red() < 10,
            "no red past the box's bottom edge, got {:?}",
            past_bottom
        );
    }

    #[test]
    fn test_input_renders_border_value_and_caret() {
        use crate::style::ComputedStyle;
        use pwb_core::spec::Tag;
        let input = crate::layout::LayoutBox {
            node_id: 5,
            tag: Tag::Input,
            element_id: None,
            rect: Rect::from_xywh(10.0, 10.0, 200.0, 30.0).unwrap(),
            style: ComputedStyle::default(),
            text: None,
            value: Some("Hi".to_string()),
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 30.0,
        };
        let mut pixmap = Pixmap::new(300, 600).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &input,
            &mut pixmap,
            300,
            600,
            &TextSelection::default(),
            &HashMap::new(),
            Some((5, 1)), // focused, caret after 'H'
            RenderState::identity(),
        );

        // The 1px border at the box edge is gray, the interior is white.
        let border = pixmap.pixel(10, 20).unwrap();
        assert!(
            border.red() > 60 && border.red() < 200,
            "border painted, got {:?}",
            border
        );
        let interior = pixmap.pixel(100, 20).unwrap();
        assert!(
            interior.red() > 200 && interior.green() > 200,
            "interior bg, got {:?}",
            interior
        );

        // Value glyphs (dark pixels) appear inside the field across its height.
        let dark = (12..80)
            .flat_map(|x| (18..28).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let c = pixmap.pixel(x, y).unwrap();
                c.red() < 100 && c.green() < 100 && c.blue() < 100
            })
            .count();
        assert!(dark > 20, "value glyphs drawn, got {} dark px", dark);

        // The caret at index 1 sits right after the 'H' glyph (text starts at
        // left + padding + 1px border).
        let text_left = 10.0 + 7.2 + 1.0;
        let caret_xf = text_left + crate::layout::measure_text("H", 15.0);
        let caret_x = caret_xf as u32;
        // The 2px bar may straddle two columns; check both.
        let caret = pixmap.pixel(caret_x, 20).unwrap();
        let caret2 = pixmap.pixel(caret_x + 1, 20).unwrap();
        assert!(
            (caret.red() < 100 && caret.green() < 100)
                || (caret2.red() < 100 && caret2.green() < 100),
            "caret drawn, got {:?} / {:?}",
            caret,
            caret2
        );
    }

    #[test]
    fn test_input_caret_moves_to_index_not_pinned_at_zero() {
        use crate::dom::{Dom, DomNode};
        use pwb_core::spec::Tag as SpecTag;
        use std::collections::HashMap as HM;
        // A page with a single <input value="hello">.
        let dom = Dom {
            title: "t".to_string(),
            components: HM::new(),
            modules: HM::new(),
            root: DomNode {
                id: 1,
                tag: SpecTag::View,
                attributes: HM::new(),
                text: None,
                children: vec![DomNode {
                    id: 2,
                    tag: SpecTag::Input,
                    attributes: HM::from([("value".to_string(), "hello".to_string())]),
                    text: None,
                    children: vec![],
                }],
            },
        };
        let sel = &TextSelection::default();
        let empty_scrolls: HashMap<usize, f32> = HashMap::new();
        // Render twice, focusing the input with the caret at index 0 and at index 4
        // (after "hell"). With the old ~2000x over-wide text measurement the caret
        // offset landed past the field edge and was clipped, so the caret was only
        // ever painted at position 0. The index-4 caret must be drawn INSIDE the
        // field, to the right of the first character.
        let mut p0 = Pixmap::new(600, 100).unwrap();
        let (layout, _) = render_dom_into_pixmap(
            &mut p0,
            &dom,
            600,
            100,
            0.0,
            sel,
            &empty_scrolls,
            Some((2, 0)),
        );
        // Locate the input box so the caret region is checked within it.
        fn find_box<'a>(
            b: &'a crate::layout::LayoutBox,
            id: usize,
        ) -> Option<&'a crate::layout::LayoutBox> {
            if b.node_id == id {
                return Some(b);
            }
            b.children.iter().find_map(|c| find_box(c, id))
        }
        let input = find_box(&layout, 2).expect("input box");
        let box_left = input.rect.left().max(0.0) as u32 + 8;
        let box_right = std::cmp::min(input.rect.right() as u32, 600);
        let box_top = input.rect.top().max(0.0) as u32;
        let box_bottom = std::cmp::min(input.rect.bottom() as u32, 100);
        let mut p4 = Pixmap::new(600, 100).unwrap();
        render_dom_into_pixmap(
            &mut p4,
            &dom,
            600,
            100,
            0.0,
            sel,
            &empty_scrolls,
            Some((2, 4)),
        );
        let right_side_diffs = (box_top..box_bottom)
            .flat_map(|y| (box_left..box_right).map(move |x| (x, y)))
            .filter(|&(x, y)| p0.pixel(x, y) != p4.pixel(x, y))
            .count();
        assert!(
            right_side_diffs > 2,
            "a caret past position 0 must be painted inside the field, got {} differing px",
            right_side_diffs
        );
    }

    #[test]
    fn test_button_label_paints_inside_button_box() {
        use crate::dom::{Dom, DomNode};
        use pwb_core::spec::Tag as SpecTag;
        use std::collections::HashMap as HM;
        // A red-labeled <button>Hello</button>; with the old over-wide text
        // measurement the label was centered at a negative x and clipped, so
        // the button looked textless. The label must paint inside its box.
        let btn_attrs = HM::from([
            ("color".to_string(), "#ff0000".to_string()),
            ("font-size".to_string(), "20px".to_string()),
        ]);
        let dom = Dom {
            title: "t".to_string(),
            components: HM::new(),
            modules: HM::new(),
            root: DomNode {
                id: 1,
                tag: SpecTag::View,
                attributes: HM::new(),
                text: None,
                children: vec![DomNode {
                    id: 2,
                    tag: SpecTag::Button,
                    attributes: btn_attrs,
                    text: Some("Hello Button".to_string()),
                    children: vec![],
                }],
            },
        };
        let sel = &TextSelection::default();
        let empty_scrolls: HashMap<usize, f32> = HashMap::new();
        let mut pixmap = Pixmap::new(600, 200).unwrap();
        let (layout, _) =
            render_dom_into_pixmap(&mut pixmap, &dom, 600, 200, 0.0, sel, &empty_scrolls, None);
        fn find_box<'a>(
            b: &'a crate::layout::LayoutBox,
            id: usize,
        ) -> Option<&'a crate::layout::LayoutBox> {
            if b.node_id == id {
                return Some(b);
            }
            b.children.iter().find_map(|c| find_box(c, id))
        }
        let btn = find_box(&layout, 2).expect("button box");
        let (x0, y0, x1, y1) = (
            btn.rect.left().max(0.0) as u32,
            btn.rect.top().max(0.0) as u32,
            btn.rect.right() as u32,
            btn.rect.bottom() as u32,
        );
        let red = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let c = pixmap.pixel(x, y).unwrap();
                c.red() > 150 && c.green() < 120
            })
            .count();
        assert!(
            red > 20,
            "button label painted inside the button box, got {} red px",
            red
        );
    }

    #[test]
    fn test_border_draws_lines_on_view_edges() {
        use crate::style::ComputedStyle;
        use pwb_core::spec::Color as SpecColor;
        // A 100x60 view with a 2px red border (top+bottom+left+right) and a
        // white interior.
        let mut style = ComputedStyle::default();
        style.background_color = SpecColor::WHITE;
        style.border_top = 2.0;
        style.border_right = 2.0;
        style.border_bottom = 2.0;
        style.border_left = 2.0;
        style.border_color = SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let view = crate::layout::LayoutBox {
            node_id: 3,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(20.0, 20.0, 100.0, 60.0).unwrap(),
            style,
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 60.0,
        };
        let mut pixmap = Pixmap::new(200, 200).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &view,
            &mut pixmap,
            200,
            200,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        // Border lines: top edge row, bottom edge row, left/right columns.
        let is_red = |p: &Pixmap, x: u32, y: u32| {
            let c = p.pixel(x, y).unwrap();
            c.red() > 200 && c.green() < 60 && c.blue() < 60
        };
        assert!(is_red(&pixmap, 25, 20), "top border");
        assert!(is_red(&pixmap, 25, 79), "bottom border");
        assert!(is_red(&pixmap, 20, 40), "left border");
        assert!(is_red(&pixmap, 119, 40), "right border");
        // The interior (inset by the border) keeps the white background.
        let inside = pixmap.pixel(60, 40).unwrap();
        assert!(
            inside.red() > 200 && inside.green() > 200,
            "interior bg, got {:?}",
            inside
        );
        // Outside the box stays black.
        assert!(!is_red(&pixmap, 25, 12), "nothing above the box");

        // A side-only border (`border-bottom`) paints only that side.
        let mut style = ComputedStyle::default();
        style.background_color = SpecColor::WHITE;
        style.border_bottom = 2.0;
        style.border_color = SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let view = crate::layout::LayoutBox {
            node_id: 3,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(20.0, 20.0, 100.0, 60.0).unwrap(),
            style,
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 60.0,
        };
        let mut pixmap = Pixmap::new(200, 200).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &view,
            &mut pixmap,
            200,
            200,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );
        assert!(is_red(&pixmap, 25, 79), "bottom border still drawn");
        assert!(!is_red(&pixmap, 25, 20), "no top border");
    }

    #[test]
    fn test_js_tokenize_highlights_and_reassembles() {
        let base = Color {
            r: 200,
            g: 200,
            b: 200,
            a: 255,
        };
        let src = "var x = 42; // count\nlet s = 'hi' + \"yo\";\nfunction f() { return 1.5e3; }";
        let tokens = js_tokenize(src, base);
        // Reassembly: concatenating tokens reproduces the source exactly.
        let joined: String = tokens.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, src);

        let color_of = |needle: &str| {
            tokens
                .iter()
                .find(|(t, _)| t == needle)
                .map(|(_, c)| *c)
                .unwrap()
        };
        assert_eq!(color_of("var"), KEYWORD_COLOR);
        assert_eq!(color_of("42"), NUMBER_COLOR);
        assert_eq!(color_of("'hi'"), STRING_COLOR);
        assert_eq!(color_of("\"yo\""), STRING_COLOR);
        assert_eq!(color_of("function"), KEYWORD_COLOR);
        assert_eq!(color_of("// count"), COMMENT_COLOR);
        // Identifiers keep the base color.
        assert_eq!(color_of("x"), base);
        // true/false/null are keywords (Chrome colors them).
        let tokens2 = js_tokenize("true && null || undefined", base);
        let joined2: String = tokens2.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined2, "true && null || undefined");
        for (t, c) in tokens2.iter() {
            if t == "true" || t == "null" || t == "undefined" {
                assert_eq!(*c, KEYWORD_COLOR);
            }
        }
        // Unterminated strings don't panic and keep everything as the string.
        let tokens3 = js_tokenize("oops '", base);
        let joined3: String = tokens3.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined3, "oops '");
    }

    #[test]
    fn test_code_box_paints_highlighted_tokens() {
        // A `<code>` box with a keyword + string should paint blue keyword
        // pixels and orange string pixels (palette colors, not the box's base).
        let base = Color {
            r: 200,
            g: 200,
            b: 200,
            a: 255,
        };
        let mut box_node = LayoutBox {
            node_id: 7,
            tag: pwb_core::spec::Tag::Code,
            element_id: None,
            rect: tiny_skia::Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap(),
            style: crate::style::ComputedStyle::default(),
            text: Some("var k = 'q';".to_string()),
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 60.0,
        };
        box_node.style.text_color = base;
        // Explicit 24px so glyphs are full-size (the default 3vh would resolve
        // to ~2px at this tiny test viewport and antialias into the background).
        box_node.style.font_size = Some(pwb_core::spec::UnitValue::Px(24.0));
        let mut pixmap = Pixmap::new(300, 60).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &box_node,
            &mut pixmap,
            300,
            60,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        let mut found_keyword = false;
        let mut found_string = false;
        let mut found_base = false;
        for y in 0..60 {
            for x in 0..300 {
                let p = pixmap.pixel(x, y).unwrap();
                let c = Color {
                    r: p.red(),
                    g: p.green(),
                    b: p.blue(),
                    a: p.alpha(),
                };
                if c == KEYWORD_COLOR {
                    found_keyword = true;
                } else if c == STRING_COLOR {
                    found_string = true;
                } else if c == base {
                    found_base = true;
                }
            }
        }
        assert!(found_keyword, "keyword 'var' painted blue");
        assert!(found_string, "string 'q' painted orange");
        assert!(found_base, "identifier 'k' painted in the base color");
    }

    #[test]
    fn test_code_tokens_advance_by_real_width_not_font_size() {
        // Console `<code>` lines chain token draws using draw_text_ab_glyph's
        // returned advance. It used to return the font size regardless of the
        // token length, so every token was drawn on top of the previous one
        // (overlapping console text). The painted pixels must span roughly the
        // full measured text width.
        let base = Color {
            r: 200,
            g: 200,
            b: 200,
            a: 255,
        };
        let text = "aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddddd";
        let mut box_node = LayoutBox {
            node_id: 11,
            tag: pwb_core::spec::Tag::Code,
            element_id: None,
            rect: tiny_skia::Rect::from_xywh(0.0, 0.0, 700.0, 60.0).unwrap(),
            style: crate::style::ComputedStyle::default(),
            text: Some(text.to_string()),
            value: None,
            placeholder: None,
            icon: None,
            children: vec![],
            overflow: pwb_core::spec::OverflowStyle::Visible,
            content_height: 60.0,
        };
        box_node.style.text_color = base;
        box_node.style.font_size = Some(pwb_core::spec::UnitValue::Px(24.0));
        let mut pixmap = Pixmap::new(700, 60).unwrap();
        pixmap.fill(SkiaColor::BLACK);
        render_layout_box(
            &box_node,
            &mut pixmap,
            700,
            60,
            &TextSelection::default(),
            &HashMap::new(),
            None,
            RenderState::identity(),
        );

        let expected = crate::layout::measure_text(text, 24.0);
        let mut rightmost = 0u32;
        for y in 0..60 {
            for x in 0..700 {
                let p = pixmap.pixel(x, y).unwrap();
                if p.red() > 60 || p.green() > 60 || p.blue() > 60 {
                    rightmost = rightmost.max(x);
                }
            }
        }
        assert!(
            (rightmost as f32) >= expected * 0.8,
            "code tokens must advance by their real width: rightmost={} expected >= {:.0}",
            rightmost,
            expected * 0.8
        );
    }

    #[test]
    fn test_input_auto_text_color_contrast() {
        // Dark bg → light text.
        let dark = Color {
            r: 30,
            g: 30,
            b: 30,
            a: 255,
        };
        let c = super::input_auto_text_color(&dark);
        assert!(c.r > 200, "light text on dark bg, got {:?}", c);

        // Light bg → dark text.
        let light = Color {
            r: 240,
            g: 240,
            b: 240,
            a: 255,
        };
        let c = super::input_auto_text_color(&light);
        assert!(c.r < 50, "dark text on light bg, got {:?}", c);
    }

    #[test]
    fn test_srgb_luminance() {
        assert!(super::srgb_luminance(&Color::WHITE) > 0.9);
        assert!(super::srgb_luminance(&Color::BLACK) < 0.1);
        // Gray is mid-range.
        let gray = Color {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        };
        let lum = super::srgb_luminance(&gray);
        assert!(lum > 0.2 && lum < 0.6, "gray luminance {}", lum);
    }

    #[test]
    fn test_draw_tooltip_paints() {
        use tiny_skia::Pixmap;
        let mut pm = Pixmap::new(200, 200).unwrap();
        super::draw_tooltip(&mut pm, "Hello tip", 50.0, 50.0, 200, 200);
        // The tooltip is positioned below-right of the cursor (62, 70) area;
        // check that non-background pixels exist (the text + border).
        let mut painted = false;
        for y in 70..120u32 {
            for x in 60..160u32 {
                let px = pm.pixel(x, y).unwrap();
                if px.red() != 17 || px.green() != 17 || px.blue() != 27 {
                    painted = true;
                    break;
                }
            }
            if painted {
                break;
            }
        }
        assert!(painted, "tooltip painted pixels in the expected region");
    }
}
