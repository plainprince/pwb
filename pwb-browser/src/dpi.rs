//! Device-pixel-ratio handling (HiDPI, Wayland/OS scaling).
//!
//! PlainWeb lays pages out in CSS pixels but rasterizes into the window's
//! *physical* pixel buffer, so text, borders, and images stay crisp at any
//! display scale. The bridge is [`scale`]: every authored `px` length is
//! multiplied by it while resolving (see [`crate::layout::resolve_unit`]), so a
//! 16px paragraph is laid out 16 CSS px wide but drawn with glyphs rasterized
//! at `16 * scale` device pixels.
//!
//! The scale lives in a thread-local because the whole browser (layout,
//! rendering, and the JS event loop) runs on one thread: tests that run in
//! parallel on other threads keep the default 1.0 instead of racing a global.
//!
//! `PWB_SCALE=<factor>` forces a scale, which is useful when a compositor
//! reports something the user disagrees with (e.g. Wayland without fractional
//! scaling support reporting 1.0 on a 1.5x display).

use std::cell::Cell;

/// Smallest accepted scale. Below 1 the layout would shrink below CSS px.
const MIN_SCALE: f32 = 1.0;
/// Upper bound: a 4x display already rasterizes four times as many pixels per
/// axis; higher values only cost memory and frame time.
const MAX_SCALE: f32 = 4.0;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
}

/// The current device-pixel ratio (1.0 on a standard display, 2.0 on Retina).
pub fn scale() -> f32 {
    SCALE.with(Cell::get)
}

/// Set the device-pixel ratio, clamped to a sane range. Non-finite values
/// (a broken backend) fall back to 1.0 rather than poisoning layout math.
pub fn set_scale(factor: f32) {
    let factor = if factor.is_finite() {
        factor.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    };
    SCALE.with(|scale| scale.set(factor));
}

/// A scale forced through `PWB_SCALE=<factor>`, if it parses as a positive
/// number. Callers apply this instead of the window's reported factor, so an
/// explicit override always wins.
pub fn env_scale() -> Option<f32> {
    let raw = std::env::var("PWB_SCALE").ok()?;
    let value: f32 = raw.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Scale a raw device-independent length (a theme size, a scroll step, a
/// hardcoded control size) into device pixels.
pub fn px(logical: f32) -> f32 {
    logical * scale()
}

/// Convert a physical size (what the window/surface report) into CSS pixels,
/// the units the DOM, theme metrics, and devtools truncation use.
pub fn logical_size(width: u32, height: u32) -> (u32, u32) {
    let factor = scale();
    (
        ((width as f32) / factor).round().max(1.0) as u32,
        ((height as f32) / factor).round().max(1.0) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_is_clamped_and_finite() {
        let original = scale();
        set_scale(2.0);
        assert_eq!(scale(), 2.0);
        assert_eq!(px(10.0), 20.0);
        set_scale(0.0);
        assert_eq!(scale(), MIN_SCALE);
        set_scale(100.0);
        assert_eq!(scale(), MAX_SCALE);
        set_scale(f32::NAN);
        assert_eq!(scale(), 1.0);
        set_scale(original);
    }
}
