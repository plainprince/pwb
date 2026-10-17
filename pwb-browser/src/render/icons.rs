//! Feather icon path caching.

use crate::feather;
use ab_glyph::FontArc;
use std::sync::{Mutex, OnceLock};
use tiny_skia::{Path, PathBuilder, Rect};

static ICON_PATH_CACHE: OnceLock<Mutex<std::collections::HashMap<String, Path>>> = OnceLock::new();

pub(crate) fn cached_icon_path(name: &str) -> Option<Path> {
    let cache = ICON_PATH_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut cache) = cache.lock() {
        if let Some(p) = cache.get(name) {
            return Some(p.clone());
        }
        if let Some(d) = feather::feather_path(name) {
            let p = parse_icon_path(d);
            if !p.is_empty() {
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
pub(crate) fn parse_icon_path(d: &str) -> Path {
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

pub(crate) fn get_font() -> Option<&'static FontArc> {
    crate::layout::get_font()
}
