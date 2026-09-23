//! PlainWeb wire format & binary bundle specification (`pwb-core::wire`).
//!
//! A `.pwb` artifact is a single, compact, losslessly compressed binary bundle.
//! It contains:
//! 1. Header (Magic `PWB1`, version, identity key, timestamp).
//! 2. Symbol Table (deduplicated tag IDs, attribute keys, string values).
//! 3. Binary DOM Tree (decompilable back to the exact Plain AST).
//! 4. Optional Script payload (opaque binary blob).

use crate::spec::{PlainNode, Tag};
use std::collections::HashMap;

pub const MAGIC: &[u8; 4] = b"PWB1";
/// v2 adds the component registry and the named script-module map (bundling).
pub const CURRENT_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    InvalidMagic,
    UnsupportedVersion(u16),
    UnexpectedEof,
    InvalidTag(u8),
    InvalidSymbol(u16),
    CorruptedData(String),
}

/// A complete compiled PlainWeb bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    pub version: u16,
    pub title: String,
    pub author_pubkey: Option<[u8; 32]>,
    pub root_node: PlainNode,
    pub script_payload: Option<Vec<u8>>,
    /// Component registry: component name → component body node (without the
    /// wrapping `<component>` element). Filled by the HTML bundler from
    /// `<component name="...">` definitions and `<import>` usages.
    pub components: HashMap<String, PlainNode>,
    /// Named script modules (every bundled `.pjs`/`.js` file), used by the
    /// runtime `pwb.import(name)` / `pwb.hotload(name)` bindings. The entry
    /// script itself is the `script_payload`.
    pub modules: HashMap<String, Vec<u8>>,
}

/// Symbol table for string deduplication across the bundle.
#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    symbols: Vec<String>,
    lookup: HashMap<String, u16>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, s: &str) -> u16 {
        if let Some(&id) = self.lookup.get(s) {
            id
        } else {
            let id = self.symbols.len() as u16;
            self.symbols.push(s.to_string());
            self.lookup.insert(s.to_string(), id);
            id
        }
    }

    pub fn get(&self, id: u16) -> Option<&str> {
        self.symbols.get(id as usize).map(|s| s.as_str())
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.symbols.len() as u16).to_be_bytes());
        for sym in &self.symbols {
            let bytes = sym.as_bytes();
            out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            out.extend_from_slice(bytes);
        }
    }

    pub fn decode(src: &[u8], cursor: &mut usize) -> Result<Self, WireError> {
        if *cursor + 2 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let count = u16::from_be_bytes([src[*cursor], src[*cursor + 1]]);
        *cursor += 2;

        let mut table = SymbolTable::new();
        for _ in 0..count {
            if *cursor + 2 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let len = u16::from_be_bytes([src[*cursor], src[*cursor + 1]]) as usize;
            *cursor += 2;

            if *cursor + len > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let s = std::str::from_utf8(&src[*cursor..*cursor + len])
                .map_err(|e| WireError::CorruptedData(e.to_string()))?;
            *cursor += len;
            table.intern(s);
        }
        Ok(table)
    }
}

impl Bundle {
    pub fn serialize(&self) -> Vec<u8> {
        // Pass 1: intern every string the bundle carries (main tree, component
        // trees, names) so all sections share one deduplicated symbol table.
        let mut symbols = SymbolTable::new();
        intern_node(&self.root_node, &mut symbols);
        let title_sym = symbols.intern(&self.title);
        let mut comp_syms: Vec<(u16, &PlainNode)> = Vec::new();
        let mut comp_names: Vec<String> = self.components.keys().cloned().collect();
        comp_names.sort();
        for name in &comp_names {
            if let Some(body) = self.components.get(name) {
                let name_sym = symbols.intern(name);
                intern_node(body, &mut symbols);
                comp_syms.push((name_sym, body));
            }
        }
        let mut mod_syms: Vec<(u16, &Vec<u8>)> = Vec::new();
        let mut mod_names: Vec<String> = self.modules.keys().cloned().collect();
        mod_names.sort();
        for name in &mod_names {
            if let Some(script) = self.modules.get(name) {
                mod_syms.push((symbols.intern(name), script));
            }
        }

        // Pass 2: encode.
        let mut tree_buf = Vec::new();
        encode_node(&self.root_node, &mut symbols, &mut tree_buf);

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.version.to_be_bytes());

        // Author pubkey flag + bytes
        if let Some(pk) = &self.author_pubkey {
            out.push(1);
            out.extend_from_slice(pk);
        } else {
            out.push(0);
        }

        // Title symbol
        out.extend_from_slice(&title_sym.to_be_bytes());

        // Symbol Table
        symbols.encode(&mut out);

        // Binary Tree length + bytes
        out.extend_from_slice(&(tree_buf.len() as u32).to_be_bytes());
        out.extend_from_slice(&tree_buf);

        // Script payload
        if let Some(script) = &self.script_payload {
            out.extend_from_slice(&(script.len() as u32).to_be_bytes());
            out.extend_from_slice(script);
        } else {
            out.extend_from_slice(&0u32.to_be_bytes());
        }

        // Component registry: count, then (name symbol + encoded tree) pairs.
        out.extend_from_slice(&(comp_syms.len() as u32).to_be_bytes());
        for (name_sym, body) in comp_syms {
            out.extend_from_slice(&name_sym.to_be_bytes());
            let mut body_buf = Vec::new();
            encode_node(body, &mut symbols, &mut body_buf);
            out.extend_from_slice(&(body_buf.len() as u32).to_be_bytes());
            out.extend_from_slice(&body_buf);
        }

        // Script module map: count, then (name symbol + length + bytes) pairs.
        out.extend_from_slice(&(mod_syms.len() as u32).to_be_bytes());
        for (name_sym, script) in mod_syms {
            out.extend_from_slice(&name_sym.to_be_bytes());
            out.extend_from_slice(&(script.len() as u32).to_be_bytes());
            out.extend_from_slice(script);
        }

        out
    }

    pub fn deserialize(src: &[u8]) -> Result<Self, WireError> {
        if src.len() < 7 {
            return Err(WireError::UnexpectedEof);
        }

        if &src[0..4] != MAGIC {
            return Err(WireError::InvalidMagic);
        }

        let version = u16::from_be_bytes([src[4], src[5]]);
        if version != CURRENT_VERSION {
            return Err(WireError::UnsupportedVersion(version));
        }

        let mut cursor = 6;
        let has_pk = src[cursor];
        cursor += 1;

        let author_pubkey = if has_pk == 1 {
            if cursor + 32 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&src[cursor..cursor + 32]);
            cursor += 32;
            Some(pk)
        } else {
            None
        };

        if cursor + 2 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let title_sym = u16::from_be_bytes([src[cursor], src[cursor + 1]]);
        cursor += 2;

        let symbols = SymbolTable::decode(src, &mut cursor)?;
        let title = symbols
            .get(title_sym)
            .ok_or(WireError::InvalidSymbol(title_sym))?
            .to_string();

        if cursor + 4 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let tree_len = u32::from_be_bytes([
            src[cursor],
            src[cursor + 1],
            src[cursor + 2],
            src[cursor + 3],
        ]) as usize;
        cursor += 4;

        if cursor + tree_len > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let tree_slice = &src[cursor..cursor + tree_len];
        cursor += tree_len;

        let mut tree_cursor = 0;
        let mut id_counter = 0;
        let root_node = decode_node(tree_slice, &mut tree_cursor, &symbols, &mut id_counter)?;

        if cursor + 4 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let script_len = u32::from_be_bytes([
            src[cursor],
            src[cursor + 1],
            src[cursor + 2],
            src[cursor + 3],
        ]) as usize;
        cursor += 4;

        let script_payload = if script_len > 0 {
            if cursor + script_len > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let script = src[cursor..cursor + script_len].to_vec();
            cursor += script_len;
            Some(script)
        } else {
            None
        };

        // Component registry: count, then (name symbol + tree length + tree).
        let mut components = HashMap::new();
        if cursor + 4 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let comp_count = u32::from_be_bytes([
            src[cursor],
            src[cursor + 1],
            src[cursor + 2],
            src[cursor + 3],
        ]) as usize;
        cursor += 4;
        for _ in 0..comp_count {
            if cursor + 2 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let name_sym = u16::from_be_bytes([src[cursor], src[cursor + 1]]);
            cursor += 2;
            if cursor + 4 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let tree_len = u32::from_be_bytes([
                src[cursor],
                src[cursor + 1],
                src[cursor + 2],
                src[cursor + 3],
            ]) as usize;
            cursor += 4;
            if cursor + tree_len > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let tree_slice = &src[cursor..cursor + tree_len];
            cursor += tree_len;
            let mut tree_cursor = 0;
            let mut id_counter = 0;
            let body = decode_node(tree_slice, &mut tree_cursor, &symbols, &mut id_counter)?;
            let name = symbols
                .get(name_sym)
                .ok_or(WireError::InvalidSymbol(name_sym))?
                .to_string();
            components.insert(name, body);
        }

        // Script module map: count, then (name symbol + length + bytes).
        let mut modules = HashMap::new();
        if cursor + 4 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let mod_count = u32::from_be_bytes([
            src[cursor],
            src[cursor + 1],
            src[cursor + 2],
            src[cursor + 3],
        ]) as usize;
        cursor += 4;
        for _ in 0..mod_count {
            if cursor + 2 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let name_sym = u16::from_be_bytes([src[cursor], src[cursor + 1]]);
            cursor += 2;
            if cursor + 4 > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let len = u32::from_be_bytes([
                src[cursor],
                src[cursor + 1],
                src[cursor + 2],
                src[cursor + 3],
            ]) as usize;
            cursor += 4;
            if cursor + len > src.len() {
                return Err(WireError::UnexpectedEof);
            }
            let script = src[cursor..cursor + len].to_vec();
            cursor += len;
            let name = symbols
                .get(name_sym)
                .ok_or(WireError::InvalidSymbol(name_sym))?
                .to_string();
            modules.insert(name, script);
        }

        Ok(Bundle {
            version,
            title,
            author_pubkey,
            root_node,
            script_payload,
            components,
            modules,
        })
    }
}

/// Intern every attribute key/value and text content of a node subtree into
/// the bundle's shared symbol table (pass 1 of serialization).
fn intern_node(node: &PlainNode, symbols: &mut SymbolTable) {
    for (k, v) in &node.attributes {
        symbols.intern(k);
        symbols.intern(v);
    }
    if let Some(txt) = &node.text_content {
        symbols.intern(txt);
    }
    for child in &node.children {
        intern_node(child, symbols);
    }
}

fn encode_node(node: &PlainNode, symbols: &mut SymbolTable, out: &mut Vec<u8>) {
    out.push(node.tag as u8);

    // Attributes
    out.extend_from_slice(&(node.attributes.len() as u16).to_be_bytes());
    for (k, v) in &node.attributes {
        let k_sym = symbols.intern(k);
        let v_sym = symbols.intern(v);
        out.extend_from_slice(&k_sym.to_be_bytes());
        out.extend_from_slice(&v_sym.to_be_bytes());
    }

    // Text content
    if let Some(txt) = &node.text_content {
        out.push(1);
        let txt_sym = symbols.intern(txt);
        out.extend_from_slice(&txt_sym.to_be_bytes());
    } else {
        out.push(0);
    }

    // Children
    out.extend_from_slice(&(node.children.len() as u32).to_be_bytes());
    for child in &node.children {
        encode_node(child, symbols, out);
    }
}

fn decode_node(
    src: &[u8],
    cursor: &mut usize,
    symbols: &SymbolTable,
    id_counter: &mut usize,
) -> Result<PlainNode, WireError> {
    if *cursor >= src.len() {
        return Err(WireError::UnexpectedEof);
    }

    let tag_u8 = src[*cursor];
    *cursor += 1;
    let tag = Tag::from_u8(tag_u8).ok_or(WireError::InvalidTag(tag_u8))?;

    *id_counter += 1;
    let mut node = PlainNode::new(*id_counter, tag);

    if *cursor + 2 > src.len() {
        return Err(WireError::UnexpectedEof);
    }
    let attr_count = u16::from_be_bytes([src[*cursor], src[*cursor + 1]]);
    *cursor += 2;

    for _ in 0..attr_count {
        if *cursor + 4 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let k_sym = u16::from_be_bytes([src[*cursor], src[*cursor + 1]]);
        let v_sym = u16::from_be_bytes([src[*cursor + 2], src[*cursor + 3]]);
        *cursor += 4;

        let key = symbols
            .get(k_sym)
            .ok_or(WireError::InvalidSymbol(k_sym))?
            .to_string();
        let val = symbols
            .get(v_sym)
            .ok_or(WireError::InvalidSymbol(v_sym))?
            .to_string();
        node.attributes.insert(key, val);
    }

    if *cursor >= src.len() {
        return Err(WireError::UnexpectedEof);
    }
    let has_txt = src[*cursor];
    *cursor += 1;

    if has_txt == 1 {
        if *cursor + 2 > src.len() {
            return Err(WireError::UnexpectedEof);
        }
        let txt_sym = u16::from_be_bytes([src[*cursor], src[*cursor + 1]]);
        *cursor += 2;
        let txt = symbols
            .get(txt_sym)
            .ok_or(WireError::InvalidSymbol(txt_sym))?
            .to_string();
        node.text_content = Some(txt);
    }

    if *cursor + 4 > src.len() {
        return Err(WireError::UnexpectedEof);
    }
    let child_count = u32::from_be_bytes([
        src[*cursor],
        src[*cursor + 1],
        src[*cursor + 2],
        src[*cursor + 3],
    ]) as usize;
    *cursor += 4;

    for _ in 0..child_count {
        let child = decode_node(src, cursor, symbols, id_counter)?;
        node.children.push(child);
    }

    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{PlainNode, Tag};

    #[test]
    fn test_wire_roundtrip() {
        let mut root = PlainNode::new(1, Tag::Document);
        let mut text_node = PlainNode::new(2, Tag::Text);
        text_node.text_content = Some("Hello PlainWeb!".to_string());
        root.children.push(text_node);

        // An icon element survives the round trip with its `icon` attribute.
        let mut icon = PlainNode::new(0, Tag::Icon);
        icon.attributes
            .insert("icon".to_string(), "home".to_string());
        root.children.push(icon);

        // A `<code>` node survives with its highlighted text.
        let mut code = PlainNode::new(0, Tag::Code);
        code.text_content = Some("var x = 42;".to_string());
        root.children.push(code);

        // A component body with a {param} placeholder and a named script module
        // must survive the bit-packed wire round trip (v2 bundling fields).
        let mut comp = PlainNode::new(0, Tag::View);
        comp.attributes
            .insert("class".to_string(), "card".to_string());
        let mut comp_text = PlainNode::new(0, Tag::Text);
        comp_text.text_content = Some("Hello {name}".to_string());
        comp.children.push(comp_text);

        let bundle = Bundle {
            version: CURRENT_VERSION,
            title: "Test Site".to_string(),
            author_pubkey: None,
            root_node: root,
            script_payload: Some(vec![1, 2, 3, 4]),
            components: HashMap::from([("greeting".to_string(), comp)]),
            modules: HashMap::from([("lib.js".to_string(), b"var x = 1;".to_vec())]),
        };

        let binary = bundle.serialize();
        let decoded = Bundle::deserialize(&binary).unwrap();

        assert_eq!(bundle.title, decoded.title);
        assert_eq!(
            bundle.root_node.children.len(),
            decoded.root_node.children.len()
        );
        let decoded_icon = decoded
            .root_node
            .children
            .iter()
            .find(|n| n.tag == Tag::Icon)
            .expect("icon survives");
        assert_eq!(
            decoded_icon.attributes.get("icon").map(|s| s.as_str()),
            Some("home")
        );
        let decoded_code = decoded
            .root_node
            .children
            .iter()
            .find(|n| n.tag == Tag::Code)
            .expect("code survives");
        assert_eq!(decoded_code.text_content.as_deref(), Some("var x = 42;"));
        assert_eq!(bundle.script_payload, decoded.script_payload);
        assert_eq!(bundle.modules, decoded.modules);
        // Node ids are intentionally re-assigned on decode (never encoded), so
        // compare the component structure (tag/attrs/text) without ids.
        let greeting = decoded.components.get("greeting").unwrap();
        assert_eq!(greeting.tag, Tag::View);
        assert_eq!(
            greeting.attributes.get("class").map(|s| s.as_str()),
            Some("card")
        );
        assert_eq!(greeting.children.len(), 1);
        assert_eq!(greeting.children[0].tag, Tag::Text);
        assert_eq!(
            greeting.children[0].text_content.as_deref(),
            Some("Hello {name}")
        );
    }
}
