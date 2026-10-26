use super::positioning::{layout_node, resolve_oof_rect, translate_box};
use crate::dom::{Dom, DomNode};
use crate::layout::box_types::LayoutCtx;
use crate::layout::{build_layout_tree, LayoutBox};
use crate::style::ComputedStyle;
use pwb_core::spec::{OverflowStyle, Tag, UnitValue};
use std::collections::HashMap;
use tiny_skia::Rect;

fn attrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node(
    id: usize,
    tag: Tag,
    attrs: HashMap<String, String>,
    text: Option<&str>,
    children: Vec<DomNode>,
) -> DomNode {
    DomNode {
        id,
        tag,
        attributes: attrs,
        text: text.map(|s| s.to_string()),
        children,
    }
}

fn find(b: &LayoutBox, id: usize) -> Option<&LayoutBox> {
    if b.node_id == id {
        return Some(b);
    }
    b.children.iter().find_map(|c| find(c, id))
}

fn dom_with(root: DomNode) -> Dom {
    Dom {
        title: "t".to_string(),
        root,
        components: HashMap::new(),
        modules: HashMap::new(),
    }
}

#[test]
fn flex_row_children_start_in_content_area_not_top() {
    // A flex-row with default view padding must place its children (icons)
    // in the content area — below the top padding — not glued to the row's
    // top edge ("icons always at the top").
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![node(
            2,
            Tag::View,
            HashMap::from([("flex-direction".to_string(), "row".to_string())]),
            None,
            vec![
                node(
                    3,
                    Tag::Icon,
                    HashMap::from([("icon".to_string(), "home".to_string())]),
                    None,
                    vec![],
                ),
                node(
                    4,
                    Tag::Icon,
                    HashMap::from([("icon".to_string(), "star".to_string())]),
                    None,
                    vec![],
                ),
            ],
        )],
    );
    let layout = build_layout_tree(&dom_with(root), 900, 600, 0.0);
    let row = find(&layout, 2).expect("row in tree");
    let home = find(&layout, 3).expect("home icon in tree");
    let default_pad = 600.0 * 0.0333; // 3.33vh ≈ 20px
                                      // The icon's absolute y is the row's flow position plus the row's top
                                      // padding — it must sit inside the row below the top padding, not glued
                                      // to the top edge of the screen (it used to be left in local coordinates).
    assert!(
        (home.rect.top() - (row.rect.top() + default_pad)).abs() < 2.0,
        "row child sits in the row's content area, got top={} row.top={} padding={:.1}",
        home.rect.top(),
        row.rect.top(),
        default_pad
    );
    assert!(
        home.rect.top() > row.rect.top(),
        "icon is below the row's top edge, not at the screen top"
    );
    assert!(
        home.rect.bottom() <= row.rect.bottom() + 2.0,
        "icon stays within the row bounds (row bottom={}, icon bottom={})",
        row.rect.bottom(),
        home.rect.bottom()
    );
}

#[test]
fn button_and_input_are_inline_sized_not_full_width() {
    // `<button>label</button>` and `<input value="...">` must keep their
    // old-HTML inline sizing (shrink to content) instead of stretching to
    // the container's full width.
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![node(
            2,
            Tag::View,
            HashMap::new(),
            None,
            vec![
                node(
                    3,
                    Tag::Button,
                    HashMap::new(),
                    Some("Explore Network"),
                    vec![],
                ),
                node(
                    4,
                    Tag::Input,
                    HashMap::from([("value".to_string(), "hello".to_string())]),
                    None,
                    vec![],
                ),
            ],
        )],
    );
    let layout = build_layout_tree(&dom_with(root), 900, 600, 0.0);
    let btn = find(&layout, 3).expect("button in tree");
    let input = find(&layout, 4).expect("input in tree");
    // Content width for the column is ~740px; neither control may take it.
    assert!(
        btn.rect.width() < 300.0,
        "button shrinks to its label, got width {}",
        btn.rect.width()
    );
    assert!(
        input.rect.width() < 300.0,
        "input stays a compact inline field even with a value, got width {}",
        input.rect.width()
    );
}

#[test]
fn fixed_sidebar_is_containing_block_for_its_children() {
    // A `position: fixed` sidebar with an explicit width must lay its
    // subtree out against its own resolved width (not the page's flow
    // width), and its absolutely-positioned bottom-pinned panel must
    // resolve `width: 100%` / `bottom: 0` against the sidebar — this is
    // what keeps the devtools panels confined to the sidebar.
    let sidebar = node(
        2,
        Tag::View,
        HashMap::from([
            ("position".to_string(), "fixed".to_string()),
            ("top".to_string(), "0".to_string()),
            ("right".to_string(), "0".to_string()),
            ("width".to_string(), "25vw".to_string()),
            ("height".to_string(), "100vh".to_string()),
            ("padding".to_string(), "0".to_string()),
        ]),
        None,
        vec![
            node(
                3,
                Tag::View,
                HashMap::from([("height".to_string(), "3vh".to_string())]),
                None,
                vec![],
            ),
            node(
                4,
                Tag::View,
                HashMap::from([
                    ("position".to_string(), "absolute".to_string()),
                    ("left".to_string(), "0".to_string()),
                    ("bottom".to_string(), "0".to_string()),
                    ("width".to_string(), "100%".to_string()),
                    ("height".to_string(), "28vh".to_string()),
                ]),
                None,
                vec![],
            ),
        ],
    );
    let root = node(1, Tag::Document, HashMap::new(), None, vec![sidebar]);
    let layout = build_layout_tree(&dom_with(root), 1800, 1300, 0.0);
    let sidebar_box = find(&layout, 2).expect("sidebar in tree");
    let header = find(&layout, 3).expect("header in tree");
    let panel = find(&layout, 4).expect("panel in tree");

    // 25vw of 1800 = 450, anchored at the right edge.
    assert!(
        (sidebar_box.rect.width() - 450.0).abs() < 1.0,
        "sidebar is 25vw = 450 wide, got {}",
        sidebar_box.rect.width()
    );
    // In-flow children are confined to the sidebar's own width.
    assert!(
        header.rect.width() <= sidebar_box.rect.width() + 1.0,
        "header fits inside the sidebar (header {} vs sidebar {})",
        header.rect.width(),
        sidebar_box.rect.width()
    );
    // The absolute panel resolves `width: 100%` and `bottom: 0` against
    // the sidebar, not the viewport.
    assert!(
        (panel.rect.width() - sidebar_box.rect.width()).abs() < 1.0,
        "panel width is 100% of the sidebar, got {}",
        panel.rect.width()
    );
    assert!(
        (panel.rect.bottom() - sidebar_box.rect.bottom()).abs() < 1.0,
        "panel is pinned to the sidebar's bottom edge (panel bottom {} vs sidebar bottom {})",
        panel.rect.bottom(),
        sidebar_box.rect.bottom()
    );
    assert!(
        panel.rect.left() >= sidebar_box.rect.left() - 1.0
            && panel.rect.right() <= sidebar_box.rect.right() + 1.0,
        "panel stays horizontally inside the sidebar"
    );
}

#[test]
fn display_none_boxes_collapse_and_are_skipped_in_rows() {
    // A display:none element is removed from layout entirely: neither the
    // box nor its children appear, and a flex row skips it.
    let hidden = node(
        2,
        Tag::View,
        HashMap::from([("display".to_string(), "none".to_string())]),
        None,
        vec![node(3, Tag::Text, HashMap::new(), Some("gone"), vec![])],
    );
    let visible = node(4, Tag::Text, HashMap::new(), Some("here"), vec![]);
    let row = node(
        5,
        Tag::View,
        HashMap::from([("flex-direction".to_string(), "row".to_string())]),
        None,
        vec![hidden, visible],
    );
    let root = node(1, Tag::Document, HashMap::new(), None, vec![row]);
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);

    assert!(find(&layout, 2).is_none(), "display:none box is dropped");
    assert!(find(&layout, 3).is_none(), "its children are dropped too");
    // The visible sibling still lays out.
    let vis = find(&layout, 4).expect("visible box exists");
    assert!(vis.rect.width() > 0.0);
}

#[test]
fn relative_offsets_shift_the_box_from_its_flow_position() {
    let make = |relative: bool| {
        let mut attrs = HashMap::new();
        if relative {
            attrs.insert("position".to_string(), "relative".to_string());
            attrs.insert("top".to_string(), "30".to_string());
            attrs.insert("left".to_string(), "12".to_string());
        }
        let rel = node(2, Tag::View, attrs, None, vec![]);
        let root = node(1, Tag::Document, HashMap::new(), None, vec![rel]);
        build_layout_tree(&dom_with(root), 800, 600, 0.0)
    };
    let flow = find(&make(false), 2).expect("flow box").rect;
    let shifted = find(&make(true), 2).expect("relative box").rect;
    // The relative box is translated by exactly the resolved offsets.
    assert!(
        (shifted.top() - (flow.top() + 30.0)).abs() < 1.0,
        "relative top: 30 (flow {}, shifted {})",
        flow.top(),
        shifted.top()
    );
    assert!(
        (shifted.left() - (flow.left() + 12.0)).abs() < 1.0,
        "relative left: 12 (flow {}, shifted {})",
        flow.left(),
        shifted.left()
    );
}

#[test]
fn sticky_top_resolves_against_containing_block_in_flex_column() {
    // In a flex column a sticky child is positioned out-of-flow (it does
    // not advance the flow cursor), and `top` resolves against the
    // containing block — the same anchor sticky uses when it clamps
    // during scroll in the flow path.
    let sticky = node(
        2,
        Tag::View,
        HashMap::from([
            ("position".to_string(), "sticky".to_string()),
            ("top".to_string(), "10".to_string()),
            ("height".to_string(), "40".to_string()),
        ]),
        None,
        vec![],
    );
    let header = node(
        3,
        Tag::View,
        HashMap::from([("height".to_string(), "100".to_string())]),
        None,
        vec![],
    );
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![header, sticky.clone()],
    );

    for scroll in [0.0, 200.0] {
        let layout = build_layout_tree(&dom_with(root.clone()), 800, 600, scroll);
        let s = find(&layout, 2).expect("sticky box");
        assert!(
            (s.rect.top() - 10.0).abs() < 1.0,
            "scroll {}: sticky sits at top: 10, got {}",
            scroll,
            s.rect.top()
        );
    }
    // The sticky box didn't consume flow space: the document is just
    // document padding + the header.
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    assert!(
        layout.rect.height() < 150.0,
        "sticky is out-of-flow (doc height {})",
        layout.rect.height()
    );
}

#[test]
fn flex_row_justify_content_variants() {
    let mk_row = |justify: &str, extra: HashMap<String, String>| {
        let mut attrs = HashMap::from([
            ("flex-direction".to_string(), "row".to_string()),
            ("justify-content".to_string(), justify.to_string()),
            ("width".to_string(), "400".to_string()),
            ("height".to_string(), "50".to_string()),
            ("padding".to_string(), "0".to_string()),
        ]);
        attrs.extend(extra);
        node(
            2,
            Tag::View,
            attrs,
            None,
            vec![
                node(
                    3,
                    Tag::View,
                    HashMap::from([
                        ("width".to_string(), "50".to_string()),
                        ("height".to_string(), "20".to_string()),
                    ]),
                    None,
                    vec![],
                ),
                node(
                    4,
                    Tag::View,
                    HashMap::from([
                        ("width".to_string(), "50".to_string()),
                        ("height".to_string(), "20".to_string()),
                    ]),
                    None,
                    vec![],
                ),
            ],
        )
    };

    // space-between: first child at left edge, second at right edge
    // (positions are relative to the row box, which starts at the
    // document's flow padding).
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![mk_row("space-between", HashMap::new())],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let row_box = find(&layout, 2).expect("row");
    let a = find(&layout, 3).unwrap();
    let b = find(&layout, 4).unwrap();
    assert!(
        (a.rect.left() - row_box.rect.left()).abs() < 2.0,
        "first child at row start (row {}, child {})",
        row_box.rect.left(),
        a.rect.left()
    );
    assert!(
        (b.rect.right() - row_box.rect.right()).abs() < 2.0,
        "second child pushed to the row end (row {}, child {})",
        row_box.rect.right(),
        b.rect.right()
    );

    // center: both children shifted by leftover/2.
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![mk_row("center", HashMap::new())],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let row_box = find(&layout, 2).expect("row");
    let a = find(&layout, 3).unwrap();
    assert!(
        a.rect.left() > row_box.rect.left() + 100.0,
        "centered children shift right by leftover/2 (row {}, child {})",
        row_box.rect.left(),
        a.rect.left()
    );

    // end: both children shifted by the full leftover.
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![mk_row("end", HashMap::new())],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let row_box = find(&layout, 2).expect("row");
    let b = find(&layout, 4).unwrap();
    assert!(
        (b.rect.right() - row_box.rect.right()).abs() < 2.0,
        "end-aligned children sit at the row end (row {}, child {})",
        row_box.rect.right(),
        b.rect.right()
    );
}

#[test]
fn flex_row_positions_absolute_child_out_of_flow() {
    // An absolute child of a flex row is out-of-flow: it doesn't consume
    // row space, and resolves against the containing block.
    let row = node(
        2,
        Tag::View,
        HashMap::from([
            ("flex-direction".to_string(), "row".to_string()),
            ("width".to_string(), "400".to_string()),
            ("height".to_string(), "100".to_string()),
            ("padding".to_string(), "0".to_string()),
        ]),
        None,
        vec![
            node(
                3,
                Tag::View,
                HashMap::from([
                    ("position".to_string(), "absolute".to_string()),
                    ("top".to_string(), "10".to_string()),
                    ("left".to_string(), "200".to_string()),
                    ("width".to_string(), "50".to_string()),
                    ("height".to_string(), "30".to_string()),
                ]),
                None,
                vec![],
            ),
            node(
                4,
                Tag::View,
                HashMap::from([
                    ("width".to_string(), "50".to_string()),
                    ("height".to_string(), "20".to_string()),
                ]),
                None,
                vec![],
            ),
        ],
    );
    let root = node(1, Tag::Document, HashMap::new(), None, vec![row]);
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);

    let abs = find(&layout, 3).expect("absolute child laid out");
    assert!(
        (abs.rect.left() - 200.0).abs() < 2.0,
        "absolute child at left: 200, got {}",
        abs.rect.left()
    );
    assert!(
        (abs.rect.top() - 10.0).abs() < 2.0,
        "absolute child at top: 10, got {}",
        abs.rect.top()
    );
    // The in-flow sibling is not displaced by the absolute child: it sits
    // at the row's start (the row itself starts at document padding).
    let row_box = find(&layout, 2).expect("row");
    let flow = find(&layout, 4).expect("in-flow child");
    assert!(
        (flow.rect.left() - row_box.rect.left()).abs() < 2.0,
        "in-flow child stays at the row start (row {}, child {})",
        row_box.rect.left(),
        flow.rect.left()
    );
}

#[test]
fn absolute_top_left_resolves_against_containing_block() {
    let abs = node(
        2,
        Tag::View,
        HashMap::from([
            ("position".to_string(), "absolute".to_string()),
            ("top".to_string(), "40".to_string()),
            ("left".to_string(), "60".to_string()),
            ("width".to_string(), "80".to_string()),
            ("height".to_string(), "25".to_string()),
        ]),
        None,
        vec![],
    );
    let root = node(1, Tag::Document, HashMap::new(), None, vec![abs]);
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let b = find(&layout, 2).expect("absolute box");
    assert!(
        (b.rect.left() - 60.0).abs() < 2.0,
        "left: 60, got {}",
        b.rect.left()
    );
    assert!(
        (b.rect.top() - 40.0).abs() < 2.0,
        "top: 40, got {}",
        b.rect.top()
    );
    assert!(
        (b.rect.width() - 80.0).abs() < 2.0,
        "width: 80, got {}",
        b.rect.width()
    );
}

// ---------------------------------------------------------------------------
// Direct layout_node / resolve_oof_rect / translate_box unit tests.
//
// The out-of-flow and display:none arms inside `layout_node` are defensive:
// `build_box` filters those children before recursing, so they are only
// reachable by calling `layout_node` directly — which these tests do, pinning
// the behaviour if the call sites ever change.
// ---------------------------------------------------------------------------

#[test]
fn layout_node_display_none_collapses_to_a_zero_rect() {
    let ctx = LayoutCtx {
        viewport_w: 800.0,
        viewport_h: 600.0,
    };
    let hidden = node(
        2,
        Tag::View,
        attrs(&[("display", "none"), ("id", "ghost")]),
        None,
        vec![node(3, Tag::Text, HashMap::new(), Some("gone"), vec![])],
    );
    let mut y = 50.0;
    let b = layout_node(
        &hidden,
        &ctx,
        10.0,
        &mut y,
        400.0,
        ctx.viewport_rect(),
        false,
        None,
    );
    assert_eq!(b.rect.width(), 0.0);
    assert_eq!(b.rect.height(), 0.0);
    assert_eq!(b.rect.left(), 10.0);
    assert_eq!(b.rect.top(), 50.0);
    assert!(b.children.is_empty(), "children are dropped with the box");
    assert_eq!(
        b.element_id.as_deref(),
        Some("ghost"),
        "id attribute carried"
    );
    assert_eq!(y, 50.0, "display:none never advances the flow cursor");
}

#[test]
fn layout_node_fixed_anchors_to_the_viewport_and_translates_its_subtree() {
    let ctx = LayoutCtx {
        viewport_w: 800.0,
        viewport_h: 600.0,
    };
    let panel = node(
        2,
        Tag::View,
        attrs(&[
            ("position", "fixed"),
            ("top", "0"),
            ("right", "0"),
            ("width", "200"),
            ("height", "100"),
            ("padding", "0"),
        ]),
        None,
        vec![node(
            3,
            Tag::View,
            attrs(&[("width", "50"), ("height", "20"), ("padding", "0")]),
            None,
            vec![],
        )],
    );
    let mut y = 33.0;
    let b = layout_node(
        &panel,
        &ctx,
        0.0,
        &mut y,
        800.0,
        ctx.viewport_rect(),
        false,
        None,
    );
    // `right: 0` with width 200 pins the panel at 800 - 200 = 600.
    assert!(
        (b.rect.left() - 600.0).abs() < 1.0,
        "left = {}",
        b.rect.left()
    );
    assert!(b.rect.top().abs() < 1.0, "top = {}", b.rect.top());
    assert!((b.rect.width() - 200.0).abs() < 1.0);
    assert!(
        (b.rect.height() - 100.0).abs() < 1.0,
        "explicit height wins"
    );
    assert_eq!(y, 33.0, "fixed boxes never advance the flow cursor");
    // The child was laid out in local coordinates, then translated with the panel.
    assert!(
        b.children[0].rect.left() >= 599.0,
        "child translated into the panel: {}",
        b.children[0].rect.left()
    );
}

#[test]
fn layout_node_absolute_resolves_bottom_anchor_against_the_containing_block() {
    let ctx = LayoutCtx {
        viewport_w: 800.0,
        viewport_h: 600.0,
    };
    let containing = Rect::from_xywh(50.0, 40.0, 300.0, 200.0).unwrap();
    let panel = node(
        2,
        Tag::View,
        attrs(&[
            ("position", "absolute"),
            ("left", "0"),
            ("bottom", "0"),
            ("width", "100%"),
            ("height", "50"),
            ("padding", "0"),
        ]),
        None,
        vec![],
    );
    let mut y = 500.0;
    let b = layout_node(&panel, &ctx, 0.0, &mut y, 800.0, containing, false, None);
    // `width: 100%` resolves against the 300px containing block.
    assert!(
        (b.rect.width() - 300.0).abs() < 1.0,
        "width = {}",
        b.rect.width()
    );
    assert!(
        (b.rect.left() - 50.0).abs() < 1.0,
        "left = {}",
        b.rect.left()
    );
    // `bottom: 0` with height 50 → top = 240 - 50 = 190.
    assert!((b.rect.top() - 190.0).abs() < 1.0, "top = {}", b.rect.top());
    assert_eq!(y, 500.0, "absolute boxes never advance the flow cursor");
}

#[test]
fn resolve_oof_rect_keeps_the_flow_position_and_floors_the_size() {
    let containing = Rect::from_xywh(50.0, 40.0, 300.0, 200.0).unwrap();
    // No offsets → flow position; no sizes → natural size, floored at 1px.
    let style = ComputedStyle::default();
    let rect = resolve_oof_rect(&style, &containing, 0.0, 0.0, 75.0, 90.0, 800.0, 600.0);
    assert!(
        (rect.left() - 75.0).abs() < 1.0,
        "flow_x kept: {}",
        rect.left()
    );
    assert!(
        (rect.top() - 90.0).abs() < 1.0,
        "flow_y kept: {}",
        rect.top()
    );
    assert!(
        rect.width() >= 1.0 && rect.height() >= 1.0,
        "size floors at 1px"
    );

    // Explicit sizes override the natural size and clamp to min/max-width.
    let style = ComputedStyle {
        width: Some(UnitValue::Px(100.0)),
        height: Some(UnitValue::Px(40.0)),
        max_width: Some(UnitValue::Px(80.0)),
        ..ComputedStyle::default()
    };
    let rect = resolve_oof_rect(&style, &containing, 10.0, 10.0, 0.0, 0.0, 800.0, 600.0);
    assert!(
        (rect.width() - 80.0).abs() < 1.0,
        "max-width clamps: {}",
        rect.width()
    );
    assert!((rect.height() - 40.0).abs() < 1.0);

    // `right`-anchoring clamps so the box can never escape the left edge.
    let style = ComputedStyle {
        right: Some(10_000.0),
        ..ComputedStyle::default()
    };
    let rect = resolve_oof_rect(&style, &containing, 40.0, 40.0, 0.0, 0.0, 800.0, 600.0);
    assert!(
        (rect.left() - containing.left()).abs() < 1.0,
        "clamped to the containing block: {}",
        rect.left()
    );
}

#[test]
fn translate_box_moves_the_whole_subtree_and_ignores_zero_offsets() {
    fn box_at(id: usize, x: f32, top: f32, children: Vec<LayoutBox>) -> LayoutBox {
        LayoutBox {
            node_id: id,
            tag: Tag::View,
            element_id: None,
            rect: Rect::from_xywh(x, top, 20.0, 10.0).unwrap(),
            style: ComputedStyle::default(),
            text: None,
            value: None,
            placeholder: None,
            icon: None,
            image_src: None,
            children,
            overflow: OverflowStyle::Visible,
            content_height: 0.0,
        }
    }

    let mut root = box_at(1, 0.0, 0.0, vec![box_at(2, 5.0, 5.0, vec![])]);
    translate_box(&mut root, 10.0, 20.0);
    assert_eq!(root.rect.left(), 10.0);
    assert_eq!(root.rect.top(), 20.0);
    assert_eq!(
        root.children[0].rect.left(),
        15.0,
        "child moved with the parent"
    );
    assert_eq!(root.children[0].rect.top(), 25.0);

    // Zero offsets return early without touching anything.
    translate_box(&mut root, 0.0, 0.0);
    assert_eq!(root.rect.left(), 10.0);
    assert_eq!(root.children[0].rect.left(), 15.0);
}

#[test]
fn flex_row_without_explicit_height_sizes_to_its_children() {
    // No `height` on the row: it grows to padding + tallest child instead of
    // collapsing or stretching.
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![node(
            2,
            Tag::View,
            attrs(&[("flex-direction", "row"), ("padding", "0")]),
            None,
            vec![
                node(
                    3,
                    Tag::View,
                    attrs(&[("width", "60"), ("height", "24")]),
                    None,
                    vec![],
                ),
                node(
                    4,
                    Tag::View,
                    attrs(&[("width", "40"), ("height", "18")]),
                    None,
                    vec![],
                ),
            ],
        )],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let row = find(&layout, 2).expect("row");
    let a = find(&layout, 3).expect("first child");
    let b = find(&layout, 4).expect("second child");
    // The row is at least as tall as its tallest child and does not overlap
    // what comes after it in the flow.
    assert!(
        row.rect.height() >= a.rect.height() - 1.0,
        "row height {} covers child height {}",
        row.rect.height(),
        a.rect.height()
    );
    assert!(
        b.rect.left() >= a.rect.right() - 1.0,
        "children sit side by side (a.right {}, b.left {})",
        a.rect.right(),
        b.rect.left()
    );
}

#[test]
fn justify_content_edges_with_no_leftover_or_a_single_child() {
    // space-between with one child, and center/end when children exactly fill
    // the row: all three take the no-shift path out of the distribution.
    let mk_row = |justify: &str, children: Vec<DomNode>| {
        node(
            2,
            Tag::View,
            attrs(&[
                ("flex-direction", "row"),
                ("justify-content", justify),
                ("width", "200"),
                ("height", "40"),
                ("padding", "0"),
                ("gap", "0"),
            ]),
            None,
            children,
        )
    };
    let fixed = |id: usize, w: u32| {
        let width = w.to_string();
        node(
            id,
            Tag::View,
            attrs(&[("width", width.as_str()), ("height", "20")]),
            None,
            vec![],
        )
    };

    // space-between, single child: nothing to distribute between.
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![mk_row("space-between", vec![fixed(3, 50)])],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let row = find(&layout, 2).unwrap();
    let only = find(&layout, 3).unwrap();
    assert!(
        (only.rect.left() - row.rect.left()).abs() < 2.0,
        "single child stays at the row start"
    );

    // center/end with exactly-filling children: zero leftover, no shift.
    for justify in ["center", "end"] {
        let root = node(
            1,
            Tag::Document,
            HashMap::new(),
            None,
            vec![mk_row(justify, vec![fixed(3, 100), fixed(4, 100)])],
        );
        let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
        let row = find(&layout, 2).unwrap();
        let a = find(&layout, 3).unwrap();
        assert!(
            (a.rect.left() - row.rect.left()).abs() < 2.0,
            "{justify} with no leftover leaves the child at the row start"
        );
    }
}

#[test]
fn button_honors_explicit_width_and_height() {
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![node(
            2,
            Tag::View,
            HashMap::new(),
            None,
            vec![
                node(
                    3,
                    Tag::Button,
                    attrs(&[("width", "120"), ("height", "48")]),
                    Some("OK"),
                    vec![],
                ),
                node(4, Tag::Button, HashMap::new(), Some("OK"), vec![]),
            ],
        )],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);
    let explicit = find(&layout, 3).expect("explicit button");
    assert!(
        (explicit.rect.width() - 120.0).abs() < 1.0,
        "explicit width wins: {}",
        explicit.rect.width()
    );
    assert!(
        (explicit.rect.height() - 48.0).abs() < 1.0,
        "explicit height wins: {}",
        explicit.rect.height()
    );
    // The default button still sizes to its label, not the explicit box.
    let natural = find(&layout, 4).expect("natural button");
    assert!(
        natural.rect.width() < 100.0,
        "default button shrinks to its label: {}",
        natural.rect.width()
    );
}

#[test]
fn image_sizing_prefers_attributes_then_css_then_defaults() {
    let root = node(
        1,
        Tag::Document,
        HashMap::new(),
        None,
        vec![node(
            2,
            Tag::View,
            HashMap::new(),
            None,
            vec![
                // Pixel attributes win outright.
                node(
                    3,
                    Tag::Image,
                    attrs(&[("width", "120"), ("height", "60")]),
                    None,
                    vec![],
                ),
                // CSS lengths apply when no pixel attribute is present.
                node(
                    4,
                    Tag::Image,
                    attrs(&[("style", "width: 90px; height: 30px")]),
                    None,
                    vec![],
                ),
                // No sizing at all → the 320x180 default.
                node(5, Tag::Image, HashMap::new(), None, vec![]),
            ],
        )],
    );
    let layout = build_layout_tree(&dom_with(root), 800, 600, 0.0);

    let attr_sized = find(&layout, 3).expect("attr-sized image");
    assert!(
        (attr_sized.rect.width() - 120.0).abs() < 1.0,
        "width {}",
        attr_sized.rect.width()
    );
    assert!(
        (attr_sized.rect.height() - 60.0).abs() < 1.0,
        "height {}",
        attr_sized.rect.height()
    );

    let css_sized = find(&layout, 4).expect("css-sized image");
    assert!(
        (css_sized.rect.width() - 90.0).abs() < 1.0,
        "width {}",
        css_sized.rect.width()
    );
    assert!(
        (css_sized.rect.height() - 30.0).abs() < 1.0,
        "height {}",
        css_sized.rect.height()
    );

    let default = find(&layout, 5).expect("default image");
    assert!(
        default.rect.width() <= 320.0 + 1.0,
        "default width {}",
        default.rect.width()
    );
    assert!(
        default.rect.width() > 100.0,
        "default width {}",
        default.rect.width()
    );
    assert!(
        (default.rect.height() - 180.0).abs() < 1.0,
        "default height {}",
        default.rect.height()
    );
    // Images push the flow along by height + spacing.
    assert!(css_sized.rect.top() >= attr_sized.rect.bottom() - 1.0);
}

#[test]
fn scratch_tab_dump() {
    let dev_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("webpages/devtools");
    let bundle = crate::bundler::bundle_from_dir(&dev_dir).expect("devtools bundle");
    let dom = Dom::from_bundle(&bundle);
    let root = build_layout_tree(&dom, 1800, 1300, 0.0);
    fn dump(b: &crate::layout::LayoutBox, depth: usize) {
        let r = b.rect;
        println!(
            "{}{} id={} x={:.1} y={:.1} w={:.1} h={:.1}",
            "  ".repeat(depth),
            b.tag.name(),
            b.node_id,
            r.left(),
            r.top(),
            r.width(),
            r.height()
        );
        for c in &b.children {
            dump(c, depth + 1);
        }
    }
    dump(&root, 0);
}
