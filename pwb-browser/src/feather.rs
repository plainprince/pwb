//! Feather icon path data (`pwb-browser::feather`). GENERATED — do not edit.
//!
//! Every icon is a normalized absolute path using only `M`/`L`/`C`/`Z`
//! commands in a 24x24 viewBox, resolved from the original feather SVG
//! markup (arcs, quads, smooths, H/V lines and shape elements are all
//! converted here). Icons render stroked (round cap/join), like feather.

mod icons_a_f;
mod icons_g_m;
mod icons_n_s;
mod icons_t_z;

#[cfg(test)]
mod tests;

/// Look up a feather icon's path data by name (e.g. `"home"`).
pub fn feather_path(name: &str) -> Option<&'static str> {
    icons_a_f::lookup(name)
        .or_else(|| icons_g_m::lookup(name))
        .or_else(|| icons_n_s::lookup(name))
        .or_else(|| icons_t_z::lookup(name))
}
