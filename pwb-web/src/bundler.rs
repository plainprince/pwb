//! Bundling for scripts and stylesheets (`pwb-web::bundler`).
//!
//! Multiple `.pjs`/`.js` and `.pss`/`.css` files are combined into single,
//! **non-minified** payloads (so the author's source stays readable) and then
//! carried by the `.pwb` artifact's bit-packed wire format.
//!
//! - **JS**: the QuickJS runtime is script-mode (no ESM execution), so the
//!   bundler concatenates all files in deterministic order with readable
//!   `// ── file: x.pjs ──` separators and rewrites the supported ESM subset
//!   into script mode: `import` statements and `export {...}`/`export *`
//!   lists are removed, and the `export` keyword is dropped from declarations.
//!   Anything script mode cannot express (`export default`) is a build error
//!   that names the file and line instead of silently corrupting the script.
//!   The runtime provides `pwb.import(name)` / `pwb.hotload(name)` bindings
//!   backed by the named-module map for dynamic loading instead.
//! - **CSS**: `.css` files are bundled through `lightningcss` (resolving
//!   `@import` chains, non-minified); native `.pss` files are concatenated
//!   unchanged. Both end up in the single stylesheet applied at build time.

use std::collections::HashMap;
use std::path::Path;

use crate::parse::ParseError;

/// A bundled entry script plus the named-module map for `pwb.import`.
pub type BundledScripts = (Vec<u8>, HashMap<String, Vec<u8>>);

/// Bundle all script files into (a) the entry script payload and (b) a named
/// module map for the runtime `pwb.import`/`pwb.hotload` bindings.
///
/// `files` is `(name, contents)` in any order; the output is deterministic
/// (sorted by name). The supported ESM subset is rewritten to script mode so
/// the result runs under script-mode `eval`; unsupported ESM returns a
/// [`ParseError::SyntaxError`] that names the offending file and line. The
/// module map stores the same script-mode source the bundle runs, so
/// `pwb.import(name)` works for modules that used `import`/`export`.
pub fn bundle_js_files(
    files: &[(String, Vec<u8>)],
    entry_name: Option<&str>,
) -> Result<BundledScripts, ParseError> {
    // Deterministic order: modules first (sorted), the entry script last so its
    // dependencies are defined before it runs.
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ordered: Vec<&(String, Vec<u8>)> = sorted
        .iter()
        .filter(|(n, _)| Some(n.as_str()) != entry_name)
        .cloned()
        .collect();
    if let Some(entry) = entry_name.and_then(|e| files.iter().find(|(n, _)| n == e)) {
        ordered.push(entry);
    }

    let mut out = String::new();
    let mut modules = HashMap::new();
    for (name, contents) in ordered {
        let text = String::from_utf8_lossy(contents);
        let cleaned = strip_esm_syntax(&text, name)?;
        // The runtime evals `pwb.import(name)` source, so the module map must
        // match what the bundle runs (script mode), not the authored ESM.
        modules.insert(name.clone(), cleaned.clone().into_bytes());
        out.push_str(&format!("// ── file: {} ──\n", name));
        out.push_str(&cleaned);
        if !cleaned.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    Ok((out.into_bytes(), modules))
}

/// The ESM forms the script-mode bundler understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Esm {
    /// `import ...` / `import "x"` — removed whole.
    Import,
    /// `export { ... }` / `export {...} from "x"` — removed whole.
    ExportList,
    /// `export * from "x"` — removed whole.
    ExportAll,
    /// `export const|let|var|function|class|async ...` — only `export` is
    /// dropped, so the declaration still runs.
    ExportDeclaration,
    /// `export default ...` — cannot be represented without a module object.
    ExportDefault,
    /// Some other `export ...` form (e.g. TypeScript `export type`).
    UnsupportedExport,
}

/// Rewrite the supported ESM subset into script mode without touching strings,
/// comments, template literals, or the rest of the source.
///
/// A byte-level mask marks which characters are real code (not inside a
/// string/template/comment). Only a line whose first non-whitespace *code*
/// character starts `import`/`export` is treated as ESM, so the same words in
/// data or comments survive. Statements are consumed up to their semicolon (or
/// end of line under ASI), which keeps multi-line `import {\n ...\n} from`
/// intact, and code after the semicolon on the same line is preserved.
fn strip_esm_syntax(source: &str, file: &str) -> Result<String, ParseError> {
    let code = code_mask(source);
    let mut out = String::with_capacity(source.len());
    let mut pos = 0usize;
    let mut line_no = 1usize;

    while pos < source.len() {
        let line_end = source[pos..]
            .find('\n')
            .map(|offset| pos + offset + 1)
            .unwrap_or(source.len());
        let mut body_end = line_end;
        if source.as_bytes().get(body_end.wrapping_sub(1)) == Some(&b'\n') {
            body_end -= 1;
        }
        let body = &source[pos..body_end];
        let lead = body.len() - body.trim_start().len();
        let statement = pos + lead;

        if statement < body_end && code.get(statement).copied().unwrap_or(false) {
            // Classify against the rest of the source, not just this line: the
            // specifier may start on the next line (`export` / `import` alone
            // on a line).
            match classify_esm(&source[statement..]) {
                Some(kind @ (Esm::Import | Esm::ExportList | Esm::ExportAll)) => {
                    let end = statement_end(source, &code, statement, kind == Esm::ExportList);
                    let removed = &source[statement..end];
                    out.push_str(&source[pos..statement]);
                    // Keep line numbers stable for later diagnostics while
                    // dropping the statement's text.
                    out.extend(removed.chars().filter(|ch| *ch == '\n'));
                    line_no += removed.matches('\n').count();
                    pos = end;
                    continue;
                }
                Some(Esm::ExportDeclaration) => {
                    let mut after = statement + "export".len();
                    while source[after..body_end]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace)
                    {
                        after += source[after..].chars().next().map_or(0, char::len_utf8);
                    }
                    out.push_str(&source[pos..statement]);
                    pos = after;
                    continue;
                }
                Some(Esm::ExportDefault) => {
                    return Err(ParseError::SyntaxError(format!(
                        "{}:{}: `export default` is not supported in script-mode PlainJS; \
                         assign the value to a global or load the file with pwb.import",
                        file, line_no
                    )));
                }
                Some(Esm::UnsupportedExport) => {
                    return Err(ParseError::SyntaxError(format!(
                        "{}:{}: unsupported `export` form in script-mode PlainJS; \
                         use export const/let/var/function/class, export {{ ... }}, \
                         export * from, or pwb.import",
                        file, line_no
                    )));
                }
                None => {}
            }
        }

        out.push_str(&source[pos..line_end]);
        pos = line_end;
        line_no += 1;
    }

    Ok(out)
}

/// Classify the ESM statement starting at the trimmed line, if any.
fn classify_esm(text: &str) -> Option<Esm> {
    if let Some(rest) = text.strip_prefix("import") {
        if !keyword_boundary(rest) {
            return None;
        }
        // `import(...)` and `import.meta` are expressions, not module syntax.
        let rh = rest.trim_start();
        if rh.starts_with('(') || rh.starts_with('.') {
            return None;
        }
        return Some(Esm::Import);
    }

    let rest = text.strip_prefix("export")?;
    if !keyword_boundary(rest) {
        return None;
    }
    let rh = rest.trim_start();
    if word_start(rh, "default") {
        return Some(Esm::ExportDefault);
    }
    if rh.starts_with('{') {
        return Some(Esm::ExportList);
    }
    if rh.starts_with('*') {
        return Some(Esm::ExportAll);
    }
    const DECLARATIONS: [&str; 6] = ["const", "let", "var", "function", "class", "async"];
    if DECLARATIONS.iter().any(|keyword| word_start(rh, keyword)) {
        return Some(Esm::ExportDeclaration);
    }
    Some(Esm::UnsupportedExport)
}

/// True when `rest` does not continue the keyword (`imported`, `exports`).
fn keyword_boundary(rest: &str) -> bool {
    !rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
}

/// True when `text` starts with `word` as a whole word.
fn word_start(text: &str, word: &str) -> bool {
    text.strip_prefix(word).is_some_and(|rest| {
        !rest.starts_with(|ch: char| ch.is_alphanumeric() || ch == '_' || ch == '$')
    })
}

/// Find the byte after an import/export statement: the first `;` at brace
/// depth zero, or the newline that ends it under ASI. A newline only ends the
/// statement once it is complete — after a module string or, for an export
/// list, after its closing brace — so `import`/`export` on a line by themselves
/// continue to the next line. Strings, comments, and templates are ignored
/// through `code`.
fn statement_end(source: &str, code: &[bool], start: usize, export_list: bool) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    let mut opened_brace = false;
    let mut closed_brace = false;
    let mut saw_module_string = false;
    let mut index = start;
    while index < bytes.len() {
        let is_code = code.get(index).copied().unwrap_or(false);
        // A quote at a string/template start is masked, but the code byte just
        // before it is not; comment quotes are preceded by masked bytes.
        if !is_code
            && matches!(bytes[index], b'"' | b'\'')
            && index > start
            && code.get(index - 1).copied().unwrap_or(false)
        {
            saw_module_string = true;
        }
        if !is_code {
            index += 1;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' | b'(' => {
                if bytes[index] == b'{' && depth <= 0 {
                    opened_brace = true;
                }
                depth += 1;
            }
            b'}' | b']' | b')' => {
                depth -= 1;
                if bytes[index] == b'}' && export_list && opened_brace && depth <= 0 {
                    closed_brace = true;
                }
            }
            b';' if depth <= 0 => return index + 1,
            b'\n' if depth <= 0 && (saw_module_string || closed_brace) => return index + 1,
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

/// Mark every source byte that is real code — not inside a string, template
/// literal, template `${...}` expression, line comment, or block comment.
fn code_mask(source: &str) -> Vec<bool> {
    #[derive(Clone, Copy)]
    enum Frame {
        /// Ordinary code; `template_expr` frames carry the brace depth of a
        /// `${ ... }` expression inside a template literal.
        Code {
            template_expr: bool,
            braces: usize,
        },
        Template,
    }

    let bytes = source.as_bytes();
    let mut mask = vec![false; bytes.len()];
    let mut stack = vec![Frame::Code {
        template_expr: false,
        braces: 0,
    }];
    let mut index = 0usize;

    while index < bytes.len() {
        if matches!(stack.last(), Some(Frame::Template)) {
            match bytes[index] {
                b'\\' => index = (index + 2).min(bytes.len()),
                b'`' => {
                    stack.pop();
                    index += 1;
                }
                b'$' if bytes.get(index + 1) == Some(&b'{') => {
                    stack.push(Frame::Code {
                        template_expr: true,
                        braces: 0,
                    });
                    index += 2;
                }
                _ => index += 1,
            }
            continue;
        }

        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            quote @ (b'\'' | b'"') => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index = (index + 2).min(bytes.len()),
                        b if b == quote => {
                            index += 1;
                            break;
                        }
                        // An unterminated single-line string ends at the newline.
                        b'\n' => break,
                        _ => index += 1,
                    }
                }
            }
            b'`' => {
                stack.push(Frame::Template);
                index += 1;
            }
            b'{' => {
                if let Some(Frame::Code { braces, .. }) = stack.last_mut() {
                    *braces += 1;
                }
                mask[index] = true;
                index += 1;
            }
            b'}' => {
                let closes_template_expression = matches!(
                    stack.last(),
                    Some(Frame::Code {
                        template_expr: true,
                        braces: 0
                    })
                );
                if closes_template_expression {
                    stack.pop();
                } else {
                    if let Some(Frame::Code { braces, .. }) = stack.last_mut() {
                        *braces = braces.saturating_sub(1);
                    }
                    mask[index] = true;
                }
                index += 1;
            }
            _ => {
                mask[index] = true;
                index += 1;
            }
        }
    }

    mask
}

/// Bundle all `.css` files into a single non-minified stylesheet via
/// `lightningcss`, resolving `@import` chains relative to the site directory.
/// Returns the CSS text, or an empty string when there is nothing to bundle.
pub fn bundle_css_files(site_dir: &Path, css_files: &[String]) -> String {
    if css_files.is_empty() {
        return String::new();
    }
    // lightningcss resolves @import relative to the entry file's location, so
    // the synthetic entry must live inside the site directory.
    let entry_name = format!(".pwb-entry-{}.css", std::process::id());
    let entry_path = site_dir.join(&entry_name);
    let mut entry = String::new();
    for name in css_files {
        entry.push_str(&format!("@import \"./{}\";\n", name));
    }
    if std::fs::write(&entry_path, entry).is_err() {
        return String::new();
    }

    let mut css = String::new();
    let provider = lightningcss::bundler::FileProvider::new();
    let mut bundler = lightningcss::bundler::Bundler::new(
        &provider,
        None,
        lightningcss::stylesheet::ParserOptions::default(),
    );
    match bundler.bundle(entry_path.as_path()) {
        Ok(stylesheet) => {
            if let Ok(res) = stylesheet.to_css(lightningcss::stylesheet::PrinterOptions::default())
            {
                css = res.code;
            }
        }
        Err(e) => eprintln!("[pwb-web] CSS bundling failed: {:?}", e),
    }

    let _ = std::fs::remove_file(&entry_path);
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_bundle_orders_entry_last_and_strips_esm() {
        let files = vec![
            (
                "app.pjs".to_string(),
                b"import \"./lib.js\";\nmain();".to_vec(),
            ),
            (
                "lib.js".to_string(),
                b"function main() { return 42; }".to_vec(),
            ),
        ];
        let (payload, modules) = bundle_js_files(&files, Some("app.pjs")).unwrap();

        // Entry runs last so its dependencies are defined first.
        let text = String::from_utf8(payload).unwrap();
        assert!(text.contains("── file: lib.js ──"));
        assert!(text.contains("── file: app.pjs ──"));
        assert!(text.find("lib.js ──").unwrap() < text.find("app.pjs ──").unwrap());
        // `import` statements are stripped (the runtime is script-mode).
        assert!(!text.contains("import "));
        assert!(text.contains("main();"));

        // Every file is available as a named module for pwb.import/hotload.
        assert_eq!(modules.len(), 2);
        assert!(modules.contains_key("lib.js"));
        assert!(modules.contains_key("app.pjs"));
    }

    #[test]
    fn test_multiline_imports_and_export_declarations_become_script_mode() {
        let source = br#"import {
    a,
    b,
} from "./math.js";
export { a as alpha, b } from "./other.js";
export * from "./all.js";
export const sum = a + b;
export function twice(value) {
    return value * 2;
}
export async function later() { return sum; }
"#;
        let files = vec![("lib.js".to_string(), source.to_vec())];
        let (payload, modules) = bundle_js_files(&files, None).unwrap();
        let text = String::from_utf8(payload).unwrap();

        // Module syntax is gone, including the braces that spanned lines.
        assert!(!text.contains("from \"./math.js\""), "{text}");
        assert!(!text.contains("from \"./other.js\""), "{text}");
        assert!(!text.contains("from \"./all.js\""), "{text}");
        assert!(!text.contains("import"), "{text}");
        assert!(!text.contains("export"), "{text}");

        // Declarations survive with the `export` keyword removed.
        assert!(text.contains("const sum = a + b;"), "{text}");
        assert!(text.contains("function twice(value) {"), "{text}");
        assert!(
            text.contains("async function later() { return sum; }"),
            "{text}"
        );

        // Removed statements leave their newlines behind, so a runtime stack
        // frame still points at the authored line.
        assert_eq!(
            text.matches('\n').count(),
            String::from_utf8_lossy(source).matches('\n').count() + 2,
            "separator and trailing blank line account for the difference"
        );

        // `pwb.import("lib.js")` evals the module map, so it must be script
        // mode too — not the raw ESM that QuickJS cannot parse.
        let module = String::from_utf8(modules["lib.js"].clone()).unwrap();
        assert!(!module.contains("import"), "{module}");
        assert!(!module.contains("export"), "{module}");
        assert!(module.contains("const sum = a + b;"), "{module}");
        assert!(module.contains("function twice(value) {"), "{module}");
    }

    #[test]
    fn test_esm_looking_text_is_not_rewritten() {
        let source = r#"const statement = "import x from 'y';";
const template = `
import { z } from "q";
export const notCode = 1;
`;
/*
import q from "w";
export default 9;
*/
// export const commented = true;
const dynamic = import("./lazy.js");
const meta = import.meta;
"#;
        let files = vec![("app.pjs".to_string(), source.as_bytes().to_vec())];
        let (payload, _) = bundle_js_files(&files, Some("app.pjs")).unwrap();
        let text = String::from_utf8(payload).unwrap();

        // Each of these is data, not module syntax, so none may be stripped.
        assert!(text.contains("\"import x from 'y';\""), "{text}");
        assert!(text.contains("import { z } from \"q\";"), "{text}");
        assert!(text.contains("export const notCode = 1;"), "{text}");
        assert!(text.contains("import q from \"w\";"), "{text}");
        assert!(text.contains("export default 9;"), "{text}");
        assert!(text.contains("// export const commented = true;"), "{text}");
        assert!(text.contains("import(\"./lazy.js\")"), "{text}");
        assert!(text.contains("import.meta"), "{text}");
    }

    #[test]
    fn test_keyword_and_specifier_on_separate_lines_are_still_one_statement() {
        let source =
            b"import\n    { greet }\n    from \"./lib.js\";\nexport\n    { greet };\ngreet();\n";
        let files = vec![("app.pjs".to_string(), source.to_vec())];
        let (payload, _) = bundle_js_files(&files, Some("app.pjs")).unwrap();
        let text = String::from_utf8(payload).unwrap();

        assert!(!text.contains("import"), "{text}");
        assert!(!text.contains("export"), "{text}");
        assert!(!text.contains("from"), "{text}");
        // Both specifier lists are gone; only the call survives.
        assert_eq!(text.matches("greet").count(), 1, "{text}");
        assert!(text.contains("greet();"), "{text}");
    }

    #[test]
    fn test_export_default_is_a_build_error_with_file_and_line() {
        let source = b"const ok = 1;\nexport default function broken() {}\n";
        let files = vec![("app.pjs".to_string(), source.to_vec())];
        let error = bundle_js_files(&files, None).unwrap_err();
        let message = format!("{error:?}");
        assert!(message.contains("app.pjs:2"), "{message}");
        assert!(message.contains("export default"), "{message}");
    }

    #[test]
    fn test_code_after_a_stripped_import_on_the_same_line_runs() {
        let source = b"import \"./lib.js\"; main();\n";
        let files = vec![("app.pjs".to_string(), source.to_vec())];
        let (payload, _) = bundle_js_files(&files, Some("app.pjs")).unwrap();
        let text = String::from_utf8(payload).unwrap();
        assert!(!text.contains("import"), "{text}");
        assert!(
            text.contains(" main();"),
            "the rest of the line must survive: {text}"
        );
    }

    #[test]
    fn test_css_bundle_via_lightningcss() {
        // A site dir with two css files, one importing the other: lightningcss
        // resolves the @import chain and emits both (non-minified).
        let dir = std::env::temp_dir().join(format!("pwb-css-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("base.css"), ".a { color: red; }\n").unwrap();
        std::fs::write(
            dir.join("main.css"),
            "@import \"./base.css\";\n.b { padding: 4px; }\n",
        )
        .unwrap();

        let bundled = bundle_css_files(&dir, &["main.css".to_string()]);
        assert!(
            bundled.contains(".a"),
            "imported rule should be inlined: {}",
            bundled
        );
        assert!(
            bundled.contains(".b"),
            "entry rule should be present: {}",
            bundled
        );
        // Non-minified: selectors and braces survive.
        assert!(bundled.contains("{") && bundled.contains("}"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
