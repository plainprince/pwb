//! Real browsing, end to end, without opening a window.
//!
//! Each test follows the path a click actually takes in `pwb-browser`:
//!
//!   URL/URI → resolver (builtin site compiler or artifact host)
//!          → `Bundle` → `Dom` → layout tree → hit test
//!          → link href → resolver → next page
//!
//! The pages are the real ones shipped in `webpages/` (welcome, demo) and the
//! real `search.plain` renderer from `pwb-node`, so the tests cover the PML/PSS
//! compiler, component imports, layout, hit-testing, native link navigation,
//! and headless painting instead of hand-built DOM fixtures.
//!
//! What is not covered: the winit window, GPU/surface presentation, OS input
//! events, and clipboard interaction. Those need a display server; everything
//! below runs in CI.

use pwb_browser::builtin_sites;
use pwb_browser::dom::{Dom, DomNode};
use pwb_browser::layout::{build_layout_tree, deepest_box_scrolled, find_box_by_id};
use pwb_browser::render::{render_dom_to_pixmap, TextSelection};
use pwb_browser::resolver::resolve_url_or_file;
use pwb_core::crypto::ContentId;
use pwb_core::spec::Tag;
use pwb_core::wire::Bundle;
use pwb_node::domain::DomainRegistry;
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::search::SearchEngine;
use std::collections::HashMap;

const VIEWPORT_W: u32 = 900;
const VIEWPORT_H: u32 = 700;

/// The background fill used by the renderer when a page paints nothing.
const RENDER_BG: (u8, u8, u8) = (17, 17, 27); // #11111b

fn open(
    target: &str,
    search: &SearchEngine,
    host: &ArtifactHost,
    registry: &DomainRegistry,
    identity: &IdentityManager,
) -> (Vec<u8>, Bundle, Dom) {
    let bytes = resolve_url_or_file(target, search, host, registry, identity, 0)
        .unwrap_or_else(|e| panic!("resolving {target:?} failed: {e:?}"));
    let bundle = Bundle::deserialize(&bytes)
        .unwrap_or_else(|e| panic!("{target:?} did not decode as a bundle: {e:?}"));
    let dom = Dom::from_bundle(&bundle);
    (bytes, bundle, dom)
}

fn find_first_link(node: &DomNode) -> Option<&DomNode> {
    if node.tag == Tag::Link
        && node
            .attributes
            .get("href")
            .map(|href| !href.trim().is_empty())
            .unwrap_or(false)
    {
        return Some(node);
    }
    node.children.iter().find_map(find_first_link)
}

fn find_link_with_href_prefix<'a>(node: &'a DomNode, prefix: &str) -> Option<&'a DomNode> {
    if node.tag == Tag::Link {
        if let Some(href) = node.attributes.get("href") {
            if href.starts_with(prefix) {
                return Some(node);
            }
        }
    }
    node.children
        .iter()
        .find_map(|child| find_link_with_href_prefix(child, prefix))
}

fn find_by_attribute<'a>(node: &'a DomNode, attr: &str, value: &str) -> Option<&'a DomNode> {
    if node.attributes.get(attr).map(String::as_str) == Some(value) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_by_attribute(child, attr, value))
}

/// Whether `node_id` is `root_id` or lives inside its DOM subtree.
fn in_subtree(root: &DomNode, root_id: usize, node_id: usize) -> bool {
    fn contains(node: &DomNode, target: usize) -> bool {
        node.id == target || node.children.iter().any(|child| contains(child, target))
    }
    fn find(node: &DomNode, id: usize) -> Option<&DomNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, id))
    }
    find(root, root_id)
        .map(|subtree| contains(subtree, node_id))
        .unwrap_or(false)
}

/// Hit-test the center of a node's layout box, returning the deepest node hit.
fn hit_test_center(layout: &pwb_browser::layout::LayoutBox, node_id: usize) -> Option<usize> {
    let layout_box = find_box_by_id(layout, node_id)?;
    let x = layout_box.rect.left() + layout_box.rect.width().max(1.0) / 2.0;
    let y = layout_box.rect.top() + layout_box.rect.height().max(1.0) / 2.0;
    let (hit, _scroll_tx) = deepest_box_scrolled(layout, x, y, &HashMap::new())?;
    Some(hit.node_id)
}

/// Render the page headlessly and count pixels that are not the renderer's
/// bare background — i.e. evidence that the page (background, cards, text)
/// actually painted.
fn painted_pixels(dom: &Dom) -> usize {
    let (pixmap, _layout, _scrollbar) = render_dom_to_pixmap(
        dom,
        VIEWPORT_W,
        VIEWPORT_H,
        0.0,
        &TextSelection::default(),
        false,
    );
    pixmap
        .pixels()
        .iter()
        .filter(|pixel| (pixel.red(), pixel.green(), pixel.blue()) != RENDER_BG)
        .count()
}

fn find_box_with_id<'a>(
    layout: &'a pwb_browser::layout::LayoutBox,
    element_id: &str,
) -> Option<&'a pwb_browser::layout::LayoutBox> {
    if layout.element_id.as_deref() == Some(element_id) {
        return Some(layout);
    }
    layout
        .children
        .iter()
        .find_map(|child| find_box_with_id(child, element_id))
}

/// Opening the browser to `welcome.plain` paints the real page, and clicking
/// its native `<link href="demo.plain">` lands on the demo page — the exact
/// click path, but with a headless hit test instead of a mouse event.
#[test]
fn test_welcome_page_paints_and_its_native_link_opens_the_demo() {
    let search = SearchEngine::new();
    let host = ArtifactHost::new();
    let registry = DomainRegistry::new();
    let identity = IdentityManager::new();

    let (_welcome_bytes, welcome_bundle, welcome_dom) =
        open("welcome.plain", &search, &host, &registry, &identity);
    assert_eq!(welcome_bundle.title, "Welcome to PlainWeb");

    // The page is not a blank frame: its card background, heading, and text
    // all reach the pixmap.
    let painted = painted_pixels(&welcome_dom);
    assert!(
        painted > 5_000,
        "welcome.plain should paint a substantial page, got {painted} pixels"
    );

    let welcome_layout = build_layout_tree(&welcome_dom, VIEWPORT_W, VIEWPORT_H, 0.0);
    let link = find_first_link(&welcome_dom.root).expect("welcome page has a native link");
    assert_eq!(
        link.attributes.get("href").map(String::as_str),
        Some("demo.plain")
    );
    let link_box = find_box_by_id(&welcome_layout, link.id).expect("link has a layout box");
    assert!(
        link_box.rect.width() > 0.0 && link_box.rect.height() > 0.0,
        "a clickable link must occupy real pixels, got {:?}",
        link_box.rect
    );

    let hit = hit_test_center(&welcome_layout, link.id).expect("link center is hittable");
    assert!(
        in_subtree(&welcome_dom.root, link.id, hit),
        "click at the link center should hit the link (or its text), hit node {hit}"
    );

    // Following the href re-enters the resolver exactly like a real click.
    let href = link.attributes.get("href").unwrap();
    let (_demo_bytes, demo_bundle, demo_dom) = open(href, &search, &host, &registry, &identity);
    assert_eq!(demo_bundle.title, "Plain Web Demo");

    let demo_layout = build_layout_tree(&demo_dom, VIEWPORT_W, VIEWPORT_H, 0.0);
    // Widgets the demo page is built from are laid out with a real size.
    for id in ["main-btn", "demo-input"] {
        let node = find_by_attribute(&demo_dom.root, "id", id)
            .unwrap_or_else(|| panic!("demo page is missing #{id}"));
        let layout_box = find_box_with_id(&demo_layout, id)
            .or_else(|| find_box_by_id(&demo_layout, node.id))
            .unwrap_or_else(|| panic!("#{id} has no layout box"));
        assert!(
            layout_box.rect.width() > 0.0 && layout_box.rect.height() > 0.0,
            "#{id} must occupy space, got {:?}",
            layout_box.rect
        );
    }
    assert!(
        painted_pixels(&demo_dom) > 5_000,
        "demo.plain should paint its hero, icons, input, and button"
    );
}

/// A `search.plain` result's "Visit Site" link is native: following it
/// resolves `content.pwb/<cid>` from the artifact host and renders the cached
/// page, with no script payload and no network round-trip.
#[test]
fn test_search_result_native_link_opens_the_cached_artifact() {
    let search = SearchEngine::new();
    let host = ArtifactHost::new();
    let registry = DomainRegistry::new();
    let identity = IdentityManager::new();

    // What the federated search layer does when a hit is prefetched: store the
    // bundle by content id and index its terms locally.
    let demo_bytes = builtin_sites::compile_builtin_page_bytes("demo")
        .expect("demo.plain compiles from its real sources");
    let cid = ContentId::compute(&demo_bytes);
    host.store(demo_bytes.clone()).expect("host stores bytes");
    let demo_bundle = Bundle::deserialize(&demo_bytes).expect("demo decodes");
    search.index_bundle(cid, &demo_bundle);
    search.record_title(cid, demo_bundle.title.clone());

    let (result_bytes, result_bundle, result_dom) = open(
        "search.plain?q=plainweb",
        &search,
        &host,
        &registry,
        &identity,
    );
    assert!(
        result_bundle.title.contains("Results for"),
        "expected a results page, got {:?}",
        result_bundle.title
    );
    assert!(
        pwb_core::wire::Bundle::is_compressed(&result_bytes),
        "system pages ship in the PWBZ container too"
    );

    // The card exposes the hit's title and a native content link.
    let title_text = format!("#1. {}", demo_bundle.title);
    fn text_nodes(node: &DomNode) -> Vec<&str> {
        let mut out = Vec::new();
        if let Some(text) = &node.text {
            out.push(text.as_str());
        }
        for child in &node.children {
            out.extend(text_nodes(child));
        }
        out
    }
    assert!(
        text_nodes(&result_dom.root).contains(&title_text.as_str()),
        "results page must show the indexed hit, saw {:?}",
        text_nodes(&result_dom.root)
    );

    let link = find_link_with_href_prefix(&result_dom.root, "content.pwb/")
        .expect("result card has a native content.pwb link");
    let href = link.attributes.get("href").cloned().unwrap();
    assert_eq!(
        href,
        format!("content.pwb/{}", cid.to_hex()),
        "link must address the stored artifact by its content id"
    );

    // Hit test the link and follow it through the resolver.
    let layout = build_layout_tree(&result_dom, VIEWPORT_W, VIEWPORT_H, 0.0);
    let hit = hit_test_center(&layout, link.id).expect("result link is hittable");
    assert!(
        in_subtree(&result_dom.root, link.id, hit),
        "click at the result link center should hit the link, hit node {hit}"
    );

    let (opened_bytes, opened_bundle, opened_dom) =
        open(&href, &search, &host, &registry, &identity);
    assert_eq!(
        opened_bytes, demo_bytes,
        "the cached bundle is served as-is"
    );
    assert_eq!(opened_bundle.title, demo_bundle.title);
    assert!(
        painted_pixels(&opened_dom) > 5_000,
        "the page opened from a search result must render"
    );
}
