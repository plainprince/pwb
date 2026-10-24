use super::*;
use crate::dom::Dom;
use crate::layout::LayoutBox;
use crate::render::color::{input_auto_text_color, srgb_luminance};
use crate::render::layout_box::render_layout_box;
use crate::render::text::{
    is_point_selected, js_tokenize, COMMENT_COLOR, KEYWORD_COLOR, NUMBER_COLOR, STRING_COLOR,
};
use pwb_core::spec::{Color, Tag};
use std::collections::HashMap;
use tiny_skia::{Color as SkiaColor, Pixmap, Rect};
use winit::window::CursorIcon;

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
        image_src: None,
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
            image_src: None,
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
        image_src: None,
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
            image_src: None,
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
        image_src: None,
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
        image_src: None,
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
        image_src: None,
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
    let child_style = ComputedStyle {
        background_color: Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        ..Default::default()
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
        image_src: None,
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
        image_src: None,
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
    let style = ComputedStyle {
        text_color: SpecColor {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        },
        ..Default::default()
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
        image_src: None,
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
    let style = ComputedStyle {
        text_color: SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        ..Default::default()
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
        image_src: None,
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
        image_src: None,
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
        (caret.red() < 100 && caret.green() < 100) || (caret2.red() < 100 && caret2.green() < 100),
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
    fn find_box(b: &crate::layout::LayoutBox, id: usize) -> Option<&crate::layout::LayoutBox> {
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
    fn find_box(b: &crate::layout::LayoutBox, id: usize) -> Option<&crate::layout::LayoutBox> {
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
    let style = ComputedStyle {
        background_color: SpecColor::WHITE,
        border_top: 2.0,
        border_right: 2.0,
        border_bottom: 2.0,
        border_left: 2.0,
        border_color: SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        ..Default::default()
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
        image_src: None,
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
    let style = ComputedStyle {
        background_color: SpecColor::WHITE,
        border_bottom: 2.0,
        border_color: SpecColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        },
        ..Default::default()
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
        image_src: None,
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
        image_src: None,
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
        image_src: None,
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
    let c = input_auto_text_color(&dark);
    assert!(c.r > 200, "light text on dark bg, got {:?}", c);

    // Light bg → dark text.
    let light = Color {
        r: 240,
        g: 240,
        b: 240,
        a: 255,
    };
    let c = input_auto_text_color(&light);
    assert!(c.r < 50, "dark text on light bg, got {:?}", c);
}

#[test]
fn test_srgb_luminance() {
    assert!(srgb_luminance(&Color::WHITE) > 0.9);
    assert!(srgb_luminance(&Color::BLACK) < 0.1);
    // Gray is mid-range.
    let gray = Color {
        r: 128,
        g: 128,
        b: 128,
        a: 255,
    };
    let lum = srgb_luminance(&gray);
    assert!(lum > 0.2 && lum < 0.6, "gray luminance {}", lum);
}

#[test]
fn test_draw_tooltip_paints() {
    use tiny_skia::Pixmap;
    let mut pm = Pixmap::new(200, 200).unwrap();
    draw_tooltip(&mut pm, "Hello tip", 50.0, 50.0, 200, 200);
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

fn rich_dom() -> Dom {
    use crate::dom::{Dom, DomNode};
    use std::collections::HashMap;
    let attrs = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    Dom {
        title: "rich".into(),
        root: DomNode {
            id: 1,
            tag: Tag::Document,
            attributes: HashMap::new(),
            text: None,
            children: vec![
                DomNode {
                    id: 2,
                    tag: Tag::View,
                    attributes: attrs(&[("background-color", "#ff0000")]),
                    text: None,
                    children: vec![DomNode {
                        id: 3,
                        tag: Tag::Text,
                        attributes: attrs(&[("color", "#ffffff")]),
                        text: Some("Hello PlainWeb".into()),
                        children: vec![],
                    }],
                },
                DomNode {
                    id: 4,
                    tag: Tag::Button,
                    attributes: HashMap::new(),
                    text: Some("Click".into()),
                    children: vec![],
                },
                DomNode {
                    id: 5,
                    tag: Tag::Input,
                    attributes: attrs(&[("value", "typed"), ("placeholder", "enter…")]),
                    text: None,
                    children: vec![],
                },
                DomNode {
                    id: 6,
                    tag: Tag::Code,
                    attributes: HashMap::new(),
                    text: Some("let x = 1;".into()),
                    children: vec![],
                },
                DomNode {
                    id: 7,
                    tag: Tag::Icon,
                    attributes: attrs(&[("icon", "star")]),
                    text: None,
                    children: vec![],
                },
                DomNode {
                    id: 8,
                    tag: Tag::View,
                    attributes: attrs(&[
                        ("position", "fixed"),
                        ("top", "0"),
                        ("right", "0"),
                        ("width", "120"),
                        ("height", "60"),
                        ("background-color", "#00ff00"),
                    ]),
                    text: None,
                    children: vec![],
                },
                DomNode {
                    id: 9,
                    tag: Tag::View,
                    attributes: attrs(&[
                        ("overflow", "scroll"),
                        ("height", "100"),
                        ("background-color", "#0000ff"),
                    ]),
                    text: None,
                    children: vec![DomNode {
                        id: 10,
                        tag: Tag::View,
                        attributes: attrs(&[("height", "400"), ("background-color", "#ffff00")]),
                        text: None,
                        children: vec![],
                    }],
                },
                // Tall in-flow block so the PAGE (not just the element)
                // overflows the viewport and produces a page scrollbar.
                DomNode {
                    id: 11,
                    tag: Tag::View,
                    attributes: attrs(&[("height", "1500")]),
                    text: None,
                    children: vec![],
                },
            ],
        },
        components: HashMap::new(),
        modules: HashMap::new(),
    }
}

#[test]
fn test_render_dom_full_featured_tree() {
    let dom = rich_dom();
    let (pixmap, layout_root, scrollbar) =
        render_dom_to_pixmap(&dom, 800, 600, 0.0, &TextSelection::default(), false);

    // Layout was built and the scroll container overflows → scrollbar.
    assert!(layout_root.rect.height() > 0.0);
    let info = scrollbar.expect("tall scroll content produces a scrollbar");
    assert!(info.thumb_rect.height() >= 30.0);
    assert!(info.track_rect.width() > 0.0);

    // The red hero view and the green fixed panel painted real pixels.
    let mut saw_red = false;
    let mut saw_green = false;
    for y in 0..600u32 {
        for x in 0..800u32 {
            let px = pixmap.pixel(x, y).unwrap();
            if px.red() > 200 && px.green() < 40 && px.blue() < 40 {
                saw_red = true;
            }
            if px.green() > 200 && px.red() < 40 && px.blue() < 40 {
                saw_green = true;
            }
        }
    }
    assert!(saw_red, "background-color: #ff0000 view paints red");
    assert!(saw_green, "fixed panel background paints green");

    // Scrolling the container moves its yellow content into view and
    // exercises the scrolled rendering paths.
    let scrolls = HashMap::from([(9, 150.0f32)]);
    let mut pixmap2 = Pixmap::new(800, 600).unwrap();
    let (_, scrollbar2) = render_dom_into_pixmap(
        &mut pixmap2,
        &dom,
        800,
        600,
        0.0,
        &TextSelection::default(),
        &scrolls,
        None,
    );
    assert!(scrollbar2.is_some());
}

#[test]
fn test_render_dom_no_scrollbar_for_short_content() {
    let dom = crate::dom::Dom {
        title: "short".into(),
        root: crate::dom::DomNode {
            id: 1,
            tag: Tag::Document,
            attributes: HashMap::new(),
            text: None,
            children: vec![crate::dom::DomNode {
                id: 2,
                tag: Tag::Text,
                attributes: HashMap::new(),
                text: Some("one line".into()),
                children: vec![],
            }],
        },
        components: HashMap::new(),
        modules: HashMap::new(),
    };
    let (_, _, scrollbar) =
        render_dom_to_pixmap(&dom, 800, 600, 0.0, &TextSelection::default(), false);
    assert!(scrollbar.is_none(), "no scrollbar when content fits");
}

#[test]
fn test_render_dom_into_pixmap_resizes_buffer() {
    let dom = rich_dom();
    let mut pixmap = Pixmap::new(100, 100).unwrap();
    render_dom_into_pixmap(
        &mut pixmap,
        &dom,
        100,
        100,
        0.0,
        &TextSelection::default(),
        &HashMap::new(),
        None,
    );
    assert_eq!((pixmap.width(), pixmap.height()), (100, 100));

    render_dom_into_pixmap(
        &mut pixmap,
        &dom,
        400,
        300,
        0.0,
        &TextSelection::default(),
        &HashMap::new(),
        None,
    );
    assert_eq!((pixmap.width(), pixmap.height()), (400, 300));
}

#[test]
fn test_render_with_selection_and_input_focus() {
    let dom = rich_dom();
    let selection = TextSelection {
        start_node_id: Some(3),
        start_char_pos: Some(0),
        end_node_id: Some(3),
        end_char_pos: Some(5),
        is_selecting: true,
        selected_text_buffer: "Hello".into(),
    };
    let mut pixmap = Pixmap::new(800, 600).unwrap();
    let (layout_root, _) = render_dom_into_pixmap(
        &mut pixmap,
        &dom,
        800,
        600,
        0.0,
        &selection,
        &HashMap::new(),
        Some((5, 2)),
    );
    // Selection + focus must not disturb layout, and the input box with
    // its focused caret still lays out where expected.
    let text_box = crate::layout::find_box_by_id(&layout_root, 3).expect("text box laid out");
    assert!(text_box.rect.width() > 0.0);
    let input = crate::layout::find_box_by_id(&layout_root, 5).expect("input box laid out");
    assert!(input.rect.height() > 0.0);
    // The glyph-selection band is only painted when a system font loads
    // (draw_text_ab_glyph's font path); the no-font fallback draws plain
    // char rects, so we only require the render to complete.
    assert_eq!((pixmap.width(), pixmap.height()), (800, 600));
}

#[test]
fn test_hit_test_cursor_from_layout_variants() {
    let dom = rich_dom();
    let layout_root = crate::layout::build_layout_tree(&dom, 800, 600, 0.0);

    // Button → pointer, text/input → text caret, plain view → default.
    let button = crate::layout::find_box_by_id(&layout_root, 4).unwrap();
    assert_eq!(
        hit_test_cursor_from_layout(
            &layout_root,
            button.rect.left() + 2.0,
            button.rect.top() + 2.0
        ),
        CursorIcon::Pointer
    );
    let input = crate::layout::find_box_by_id(&layout_root, 5).unwrap();
    assert_eq!(
        hit_test_cursor_from_layout(
            &layout_root,
            input.rect.left() + 2.0,
            input.rect.top() + 2.0
        ),
        CursorIcon::Text
    );
    // Outside everything → default.
    assert_eq!(
        hit_test_cursor_from_layout(&layout_root, -10.0, -10.0),
        CursorIcon::Default
    );
}

#[test]
fn test_image_paints_a_scaled_png_inside_its_box() {
    // A 2x2 solid red PNG, written to a temp path and referenced by `src`.
    let mut source = Pixmap::new(2, 2).unwrap();
    source.fill(SkiaColor::from_rgba8(255, 0, 0, 255));
    let png = source.encode_png().expect("encode png");
    let path = std::env::temp_dir().join(format!("pwb-img-{}.png", std::process::id()));
    std::fs::write(&path, png).expect("write png");

    let image = crate::layout::LayoutBox {
        node_id: 9,
        tag: Tag::Image,
        element_id: None,
        rect: Rect::from_xywh(10.0, 10.0, 40.0, 20.0).unwrap(),
        style: Default::default(),
        text: None,
        value: None,
        placeholder: None,
        icon: None,
        image_src: Some(path.to_string_lossy().into_owned()),
        children: vec![],
        overflow: pwb_core::spec::OverflowStyle::Visible,
        content_height: 20.0,
    };
    let mut pixmap = Pixmap::new(80, 60).unwrap();
    pixmap.fill(SkiaColor::BLACK);
    render_layout_box(
        &image,
        &mut pixmap,
        80,
        60,
        &TextSelection::default(),
        &HashMap::new(),
        None,
        RenderState::identity(),
    );

    // The whole box is covered by the scaled image…
    for (x, y) in [(12, 12), (30, 20), (48, 28)] {
        let c = pixmap.pixel(x, y).unwrap();
        assert!(
            c.red() > 200 && c.green() < 30 && c.blue() < 30,
            "pixel ({x},{y}) should be red, got {:?}",
            c
        );
    }
    // …and nothing paints outside the box rect.
    let outside = pixmap.pixel(5, 5).unwrap();
    assert!(
        outside.red() < 10 && outside.green() < 10 && outside.blue() < 10,
        "outside the image box must stay untouched, got {:?}",
        outside
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_image_with_missing_source_paints_a_placeholder() {
    let image = crate::layout::LayoutBox {
        node_id: 10,
        tag: Tag::Image,
        element_id: None,
        rect: Rect::from_xywh(0.0, 0.0, 20.0, 20.0).unwrap(),
        style: Default::default(),
        text: None,
        value: None,
        placeholder: None,
        icon: None,
        image_src: Some("/definitely/not/a/real/image.png".to_string()),
        children: vec![],
        overflow: pwb_core::spec::OverflowStyle::Visible,
        content_height: 20.0,
    };
    let mut pixmap = Pixmap::new(30, 30).unwrap();
    pixmap.fill(SkiaColor::BLACK);
    render_layout_box(
        &image,
        &mut pixmap,
        30,
        30,
        &TextSelection::default(),
        &HashMap::new(),
        None,
        RenderState::identity(),
    );
    let c = pixmap.pixel(10, 10).unwrap();
    assert!(
        c.red() > 25 && c.blue() > 35,
        "missing images paint a visible placeholder, got {:?}",
        c
    );
}

#[test]
fn test_link_paints_its_label_and_underline() {
    // Search results and the welcome page rely on native links being visible:
    // the label paints in the accent color and a 1px underline sits below it.
    let style = crate::style::ComputedStyle {
        padding: Some(pwb_core::spec::UnitValue::Px(0.0)),
        ..Default::default()
    };
    let link = crate::layout::LayoutBox {
        node_id: 11,
        tag: Tag::Link,
        element_id: None,
        rect: Rect::from_xywh(10.0, 10.0, 140.0, 30.0).unwrap(),
        style,
        text: Some("Visit Site →".to_string()),
        value: None,
        placeholder: None,
        icon: None,
        image_src: None,
        children: vec![],
        overflow: pwb_core::spec::OverflowStyle::Visible,
        content_height: 30.0,
    };
    let mut pixmap = Pixmap::new(200, 100).unwrap();
    pixmap.fill(SkiaColor::BLACK);
    render_layout_box(
        &link,
        &mut pixmap,
        200,
        600,
        &TextSelection::default(),
        &HashMap::new(),
        None,
        RenderState::identity(),
    );

    // Default 2.5vh font at a 600px-high viewport is 15px, so the underline is
    // at y = top + 16.
    let underline = pixmap.pixel(20, 26).unwrap();
    assert!(
        underline.blue() > 150 && underline.blue() > underline.red(),
        "link underline should be accent blue, got {:?}",
        underline
    );
    let outside = pixmap.pixel(20, 60).unwrap();
    assert!(
        outside.red() < 10 && outside.green() < 10 && outside.blue() < 10,
        "nothing paints outside the link box, got {:?}",
        outside
    );
}
