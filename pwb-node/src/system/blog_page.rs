//! `blog.plain`: the single global feed of signed entries.
//!
//! Every entry is authored by an identity, signed over its title and body, and
//! flooded across the network (see [`crate::blog`]). The page shows the feed
//! newest first, who is reading, and a composer that hands a title and body to
//! [`BlogService::post`](crate::blog::BlogService::post), which signs and
//! floods them. The pipeline subscribes before rendering, so an open feed is
//! always a subscription.

use std::net::SocketAddr;

use pwb_core::crypto::PublicKey;
use pwb_core::system::SystemDomain;
use pwb_core::wire::Bundle;

use super::network_page::compile;
use super::registry_page::{escape, short_hex};
use super::SystemDomainHandler;

/// Style sheet for the feed. Entries are cards so a long body stays readable
/// between posts.
const BLOG_STYLES: &str = r#"
.blog-container {
    background-color: #1e1e2e;
    padding: 25px;
    gap: 15px;
}
.blog-title {
    color: #89b4fa;
    font-size: 26px;
}
.blog-info {
    color: #a6adc8;
    font-size: 15px;
}
.blog-heading {
    color: #a6e3a1;
    font-size: 19px;
}
.blog-back {
    color: #89b4fa;
    font-size: 14px;
    padding: 0;
}
.blog-warn {
    color: #f9e2af;
    font-size: 14px;
}
.blog-error {
    color: #f38ba8;
    font-size: 14px;
}
.blog-feed {
    gap: 10px;
}
.blog-entry {
    background-color: #313244;
    padding: 14px;
    border-radius: 8px;
    gap: 6px;
}
.blog-entry-title {
    color: #cdd6f4;
    font-size: 19px;
}
.blog-who {
    color: #89b4fa;
    font-size: 12px;
}
.blog-who-out {
    color: #a6e3a1;
    font-size: 12px;
}
.blog-text {
    color: #cdd6f4;
    font-size: 15px;
}
.blog-hint {
    color: #6c7086;
    font-size: 14px;
}
.blog-composer {
    background-color: #313244;
    padding: 12px;
    border-radius: 8px;
    gap: 10px;
    min-width: 280px;
}
#blog-title-input {
    padding: 10px;
    border-radius: 6px;
    background-color: #1e1e2e;
    color: #cdd6f4;
    font-size: 17px;
}
#blog-text-input {
    padding: 10px;
    border-radius: 6px;
    background-color: #1e1e2e;
    color: #cdd6f4;
    font-size: 15px;
}
#blog-post {
    background-color: #a6e3a1;
    color: #11111b;
    padding: 10px 20px;
    border-radius: 6px;
    font-size: 16px;
    cursor: pointer;
}
"#;

impl SystemDomainHandler<'_> {
    /// Render the native `blog.plain` bundle: the live feed, its readers, and
    /// the composer.
    pub fn render_system_blog_page(&self, peers: &[(PublicKey, SocketAddr)]) -> Bundle {
        let title = format!("{} — The Global Feed", SystemDomain::Blog.as_str());
        let feed = &self.blog;
        let entries = feed.entries.len();
        let authors = feed.author_count();
        let readers = feed.reader_count();

        let mut markup = format!(
            r##"<view class="blog-container">
        <text class="blog-title">blog.plain — The Global Feed</text>
        <text class="blog-info">This node identity: {}</text>
        <text class="blog-info">Entries are signed by their author and flood the network: every reader keeps the feed, and anyone can relay it.</text>"##,
            feed.local.to_hex()
        );

        if !feed.subscribed {
            markup.push_str(
                r##"<text class="blog-warn">Subscribing to the feed — announcing to peers and asking for what you missed.</text>"##,
            );
        }

        markup.push_str(&format!(
            r##"<text class="blog-info">{} · {} · {}</text>"##,
            count(entries, "entry", "entries"),
            count(authors, "author", "authors"),
            count(readers, "reader", "readers"),
        ));

        // Readers are announced one hop, like a room roster: this is "peers I
        // have heard reading", not a global count.
        let mut roster: Vec<String> = feed
            .subscribers
            .iter()
            .map(|key| short_hex(&key.0, 8))
            .collect();
        roster.sort();
        let roster = if roster.is_empty() {
            "nobody else yet".to_string()
        } else {
            roster.join(", ")
        };
        markup.push_str(&format!(
            r##"<text class="blog-info">Reading too: {roster}</text>"##
        ));

        if peers.is_empty() {
            markup.push_str(
                r##"<text class="blog-warn">No peers connected: entries stay on this node until you pair with a node on nodes.plain.</text>"##,
            );
        }

        markup.push_str(r##"<link class="blog-back" href="help.plain">← help.plain</link>"##);

        markup.push_str(r##"<view class="blog-feed">"##);
        if feed.entries.is_empty() {
            markup.push_str(
                r##"<text class="blog-hint">No entries yet. Write the first one — it goes out signed by you and reaches the network through every peer.</text>"##,
            );
        } else {
            // Newest first: the feed reads like a timeline, not a transcript.
            for entry in feed.entries.iter().rev() {
                let (who_class, who) = if entry.sender == feed.local {
                    ("blog-who-out", "me".to_string())
                } else {
                    ("blog-who", short_hex(&entry.sender.0, 8))
                };
                markup.push_str(r##"<view class="blog-entry">"##);
                for line in wrap(&entry.title, 56) {
                    markup.push_str(&format!(
                        r##"<text class="blog-entry-title">{}</text>"##,
                        escape(&line)
                    ));
                }
                markup.push_str(&format!(
                    r##"<text class="{who_class}">{who} · {}</text>"##,
                    stamp(entry.sent_at)
                ));
                if !entry.text.trim().is_empty() {
                    for line in wrap(&entry.text, 88) {
                        markup.push_str(&format!(
                            r##"<text class="blog-text">{}</text>"##,
                            escape(&line)
                        ));
                    }
                }
                markup.push_str(r##"</view>"##);
            }
        }
        markup.push_str(r##"</view>"##);

        if let Some(error) = &feed.error {
            markup.push_str(&format!(
                r##"<text class="blog-error">{}</text>"##,
                escape(error)
            ));
        }

        markup.push_str(
            r##"<view class="blog-composer">
            <input id="blog-title-input" placeholder="Headline..." />
            <input id="blog-text-input" placeholder="Write an entry..." />
            <button id="blog-post">Post</button>
        </view>"##,
        );
        markup.push_str(r##"</view></plain>"##);

        let pml_markup = format!(
            r##"<plain>
        <head><title>blog.plain</title></head>
        {}"##,
            markup
        );

        // Post hands the headline and body to the pipeline's pending-blog
        // action; the pipeline signs the entry, floods it, and re-renders this
        // page from the updated feed.
        let js_script = r##"console.log("Loaded blog.plain feed interface.");
var __blogPost = "__pwb_blog_post(__pwb_getValue('#blog-title-input'), __pwb_getValue('#blog-text-input')); __pwb_setValue('#blog-title-input', ''); __pwb_setValue('#blog-text-input', '');";
var __blogBtn = document.selector("#blog-post");
if (__blogBtn) { __blogBtn.onClick(__blogPost); }
var __blogText = document.selector("#blog-text-input");
if (__blogText) { __blogText.addEventHandler("enter", __blogPost); }
var __blogTitle = document.selector("#blog-title-input");
if (__blogTitle) { __blogTitle.addEventHandler("enter", __blogPost); }
"##;

        compile(title, &pml_markup, BLOG_STYLES, Some(js_script))
    }
}

/// `n thing(s)` with the right plural.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Split `text` into lines of at most `width` characters, breaking on spaces
/// and hard-splitting any single word longer than a line.
///
/// The renderer draws text on one line and does not wrap it, so a blog body
/// has to arrive pre-wrapped; blank lines between paragraphs are preserved.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut paragraph_seen = false;
    for paragraph in text.split('\n') {
        if !paragraph_seen {
            paragraph_seen = true;
        } else if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let word_len = word.chars().count();
            if line.is_empty() {
                if word_len <= width {
                    line = word.to_string();
                } else {
                    line = hard_split(word, width, &mut lines);
                }
            } else if line.chars().count() + 1 + word_len <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(std::mem::take(&mut line));
                line = if word_len <= width {
                    word.to_string()
                } else {
                    hard_split(word, width, &mut lines)
                };
            }
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Push full `width`-character chunks of `word` and return its remainder (the
/// part that starts the next line).
fn hard_split(word: &str, width: usize, lines: &mut Vec<String>) -> String {
    let mut current = String::new();
    for ch in word.chars() {
        if current.chars().count() == width {
            lines.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    current
}

/// `YYYY-MM-DD HH:MM` (UTC) for a unix timestamp. Blog entries are read days
/// later, so unlike a chat line they carry the date, not just the clock.
fn stamp(unix: u64) -> String {
    let secs_of_day = unix % 86_400;
    let (year, month, day) = civil_from_days((unix / 86_400) as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60
    )
}

/// Civil UTC date from days since 1970-01-01 (Howard Hinnant's algorithm),
/// avoiding a date-library dependency for one label.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blog::BlogSnapshot;
    use crate::domain::DomainRegistry;
    use crate::host::ArtifactHost;
    use crate::identity::IdentityManager;
    use crate::search::SearchEngine;
    use pwb_core::crypto::Keypair;
    use pwb_core::net::{blog_entry_sign_message, BlogEntryWire};

    fn handler<'a>(identity: &'a IdentityManager, blog: BlogSnapshot) -> SystemDomainHandler<'a> {
        let search = SearchEngine::new();
        let host = ArtifactHost::new();
        let registry = DomainRegistry::new();
        let search = Box::leak(Box::new(search));
        let host = Box::leak(Box::new(host));
        let registry = Box::leak(Box::new(registry));
        SystemDomainHandler::new(search, host, registry, identity, 0).with_blog(blog)
    }

    /// The rendered bundle's root DOM node.
    fn root(bundle: &Bundle) -> pwb_core::spec::PlainNode {
        pwb_web::compress::decompress_bundle(&pwb_web::compress::compress_bundle(bundle))
            .expect("bundle round-trips")
            .root_node
    }

    fn find_text(node: &pwb_core::spec::PlainNode, needle: &str) -> bool {
        if node
            .text_content
            .as_deref()
            .is_some_and(|text| text.contains(needle))
        {
            return true;
        }
        node.children.iter().any(|child| find_text(child, needle))
    }

    fn find_attr(node: &pwb_core::spec::PlainNode, key: &str, value: &str) -> bool {
        if node.attributes.get(key).map(String::as_str) == Some(value) {
            return true;
        }
        node.children
            .iter()
            .any(|child| find_attr(child, key, value))
    }

    /// All text in document order, for checking how the feed reads.
    fn collect_text(node: &pwb_core::spec::PlainNode, out: &mut String) {
        if let Some(text) = &node.text_content {
            out.push_str(text);
            out.push('\n');
        }
        for child in &node.children {
            collect_text(child, out);
        }
    }

    /// One signed entry, as it would be in the feed.
    fn entry(seed: u8, title: &str, text: &str, sent_at: u64) -> BlogEntryWire {
        let keypair = Keypair::from_seed(&[seed; 32]);
        let mut id = [0u8; 16];
        id[0] = seed;
        BlogEntryWire {
            sender: keypair.public,
            id,
            sent_at,
            title: title.to_string(),
            text: text.to_string(),
            hops: 8,
            signature: keypair.sign(&blog_entry_sign_message(
                &keypair.public,
                &id,
                sent_at,
                title,
                text,
            )),
        }
    }

    #[test]
    fn a_feed_shows_entries_its_readers_and_the_post_script() {
        let identity = IdentityManager::new();
        let peer = Keypair::from_seed(&[5u8; 32]).public;
        let blog = BlogSnapshot {
            local: identity.public_key(),
            subscribed: true,
            subscribers: vec![peer],
            entries: vec![
                entry(1, "First post", "hello world", 86_400 * 365 + 3_661),
                entry(5, "Reply post", "hi back", 86_400 * 365 + 3_700),
            ],
            error: None,
        };

        let bundle = handler(&identity, blog).render_system_blog_page(&[]);
        let root = root(&bundle);

        assert_eq!(bundle.title, "blog.plain — The Global Feed");
        assert!(find_text(&root, "First post"));
        assert!(find_text(&root, "hello world"));
        assert!(find_text(&root, "Reply post"));
        assert!(
            find_text(&root, short_hex(&peer.0, 8).as_str()),
            "the roster names the readers I know"
        );
        assert!(find_text(&root, "2 entries"), "entry count");
        assert!(find_text(&root, "2 authors"), "distinct author count");
        assert!(find_text(&root, "2 readers"), "reader count includes me");
        assert!(
            find_text(&root, "1971-01-01 01:01"),
            "entries carry a date and clock"
        );
        assert!(find_text(&root, "No peers connected"));

        // Composer wired to the pending-blog action.
        assert!(find_attr(&root, "id", "blog-title-input"));
        assert!(find_attr(&root, "id", "blog-text-input"));
        assert!(find_attr(&root, "id", "blog-post"));

        let script = String::from_utf8(
            pwb_web::compress::decompress_bundle(&pwb_web::compress::compress_bundle(&bundle))
                .expect("bundle round-trips")
                .script_payload
                .expect("page has a script"),
        )
        .unwrap();
        assert!(script.contains("__pwb_blog_post"));
        assert!(script.contains("addEventHandler(\"enter\""));

        let mut rendered = String::new();
        collect_text(&root, &mut rendered);
        let newest = rendered.find("Reply post").expect("newest entry renders");
        let oldest = rendered.find("First post").expect("oldest entry renders");
        assert!(newest < oldest, "the feed reads newest first: {rendered:?}");
    }

    #[test]
    fn an_empty_feed_says_who_is_subscribing_instead_of_looking_broken() {
        let identity = IdentityManager::new();
        let blog = BlogSnapshot {
            local: identity.public_key(),
            ..BlogSnapshot::default()
        };

        let bundle = handler(&identity, blog).render_system_blog_page(&[]);
        let root = root(&bundle);

        assert!(find_text(&root, "Subscribing to the feed"));
        assert!(find_text(&root, "No entries yet"));
        assert!(find_text(&root, "0 entries"));
        assert!(find_text(&root, "0 readers"));
    }

    #[test]
    fn a_post_failure_from_the_composer_is_shown() {
        let identity = IdentityManager::new();
        let blog = BlogSnapshot {
            local: identity.public_key(),
            subscribed: true,
            error: Some("entry is too long (max 4096 bytes)".to_string()),
            ..BlogSnapshot::default()
        };

        let bundle = handler(&identity, blog).render_system_blog_page(&[]);
        assert!(find_text(&root(&bundle), "entry is too long"));
    }

    #[test]
    fn a_hostile_title_or_body_never_becomes_markup() {
        let identity = IdentityManager::new();
        let blog = BlogSnapshot {
            local: identity.public_key(),
            subscribed: true,
            entries: vec![entry(
                6,
                "<link href=\"https://evil.example\">click</link>",
                "<text>hostile</text>",
                0,
            )],
            ..BlogSnapshot::default()
        };

        let root = root(&handler(&identity, blog).render_system_blog_page(&[]));
        assert!(
            !find_attr(&root, "href", "https://evil.example"),
            "a remote title must not inject a link into the local page"
        );
    }

    #[test]
    fn long_bodies_wrap_on_spaces_and_preserve_blank_lines() {
        let lines = wrap("one two three four", 7);
        assert_eq!(lines, ["one two", "three", "four"]);

        let paragraphs = wrap("first\n\nsecond", 20);
        assert_eq!(paragraphs, ["first", "", "second"]);

        // A word longer than a line is hard-split instead of overflowing.
        let long = wrap("abcdefghij", 4);
        assert_eq!(long, ["abcd", "efgh", "ij"]);
        assert!(long.iter().all(|line| line.chars().count() <= 4));
    }

    #[test]
    fn timestamps_render_utc_dates() {
        assert_eq!(stamp(0), "1970-01-01 00:00");
        assert_eq!(stamp(1_700_000_000), "2023-11-14 22:13");
    }
}
