//! String-deduplicating symbol table for the bundle wire format.

use std::collections::HashMap;

use super::error::WireError;

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
