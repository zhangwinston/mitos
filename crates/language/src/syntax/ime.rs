//! IME (Input Method Editor) sensitive-region detection.
//!
//! Ported from helix `helix-core/src/syntax.rs` (IME auto-control feature):
//! given the syntax tree and the cursor position, decide whether the cursor
//! sits in a *sensitive* region (string/comment content — where an IME is
//! useful) or in code.

use language_config::LanguageConfiguration;
use ropey::RopeSlice;

use super::{Language, Loader, Syntax};

/// IME (Input Method Editor) sensitive region types.
///
/// This enum identifies the type of region the cursor is in, which determines
/// whether IME should be enabled or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeSensitiveRegion {
    /// String content region (excluding leading quote).
    /// IME should be enabled in this region.
    StringContent,
    /// Comment content region (excluding header symbols like `//`, `/*`).
    /// IME should be enabled in this region.
    CommentContent,
    /// Code region (non-sensitive).
    /// IME should be disabled in this region.
    Code,
    /// Entire file (when syntax parsing is unavailable, failed, or still in progress).
    /// IME should be enabled in this region.
    EntireFile,
}

/// Result of IME region detection, with optional byte span used for caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeRegionDetection {
    pub region: ImeSensitiveRegion,
    pub node_range: Option<(usize, usize)>,
}

impl ImeRegionDetection {
    const fn new(region: ImeSensitiveRegion, node_range: Option<(usize, usize)>) -> Self {
        Self { region, node_range }
    }

    const fn code() -> Self {
        Self::new(ImeSensitiveRegion::Code, None)
    }

    fn entire_file(len_bytes: usize) -> Self {
        Self::new(ImeSensitiveRegion::EntireFile, Some((0, len_bytes)))
    }
}

/// Check if the language has string or comment types defined in its grammar.
///
/// This function checks the language configuration and highlights query to determine
/// if the language supports string or comment node types. Used to implement FR-010:
/// if a language doesn't have these types, the entire file is treated as IME sensitive.
///
/// # Arguments
/// * `loader` - The syntax loader to access language configuration
/// * `language` - The language identifier
///
/// # Returns
/// `true` if the language has string or comment types, `false` otherwise.
///
/// # Performance
/// This function uses a OnceLock cache to avoid repeated file I/O operations.
/// The result is computed once per language and cached for the lifetime of the Loader.
pub(super) fn language_has_string_or_comment_types(loader: &Loader, language: Language) -> bool {
    let language_data = loader.language(language);

    // Use cached result if available, otherwise compute and cache
    *language_data.has_string_or_comment_types.get_or_init(|| {
        let config = language_data.config();

        // Check if language has comment tokens configured (indicates comment support)
        let has_comment = config.comment_tokens.is_some() || config.block_comment_tokens.is_some();

        // Check if language has string types by examining highlights query
        // Most languages with string support will have @string captures in highlights.scm
        let has_string = {
            let highlight_query = loader
                .resources
                .query(&config.language_id, "highlights.scm");
            // Check if the query contains @string capture
            highlight_query.contains("@string") || highlight_query.contains("string")
        };

        has_comment || has_string
    })
}

/// Case-insensitive ASCII substring check (`haystack` contains `needle`).
#[inline]
fn contains_ignore_ascii_case(haystack: &str, needle: &[u8]) -> bool {
    let h = haystack.as_bytes();
    if needle.is_empty() {
        return true;
    }
    let n = needle.len();
    if n > h.len() {
        return false;
    }
    h.windows(n).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Detect IME sensitive region at the given cursor position.
///
/// This function queries the syntax tree to determine if the cursor is in a
/// string content region, comment content region, or code region.
///
/// # Arguments
/// * `syntax` - The syntax tree for the document (None if parsing unavailable/failed/in progress)
/// * `source` - The document text
/// * `loader` - The syntax loader for query compilation
/// * `cursor_pos` - The cursor position in bytes
///
/// # Returns
/// The detected IME sensitive region type.
///
/// # Behavior
/// - Returns `EntireFile` if syntax parsing is unavailable, failed, or still in progress (FR-008, FR-009, FR-021)
/// - Returns `EntireFile` if language doesn't have string/comment types (FR-010)
/// - Detects comment and string regions by checking node types (simplified approach)
/// - Excludes leading quote symbols (FR-004) and the full comment marker prefix
///   (`//`, `//!`, `/*`, ...) by matching the language's configured comment tokens (FR-005)
/// - Includes trailing quote first character (FR-006) and comment tail first character (FR-007);
///   line comments also include the end-of-line cursor position, where typed text
///   still continues the comment
pub fn detect_ime_sensitive_region(
    syntax: Option<&Syntax>,
    source: RopeSlice, // Used for full file fallback caching
    loader: &Loader,   // Now used to check language configuration
    cursor_pos: usize,
) -> ImeRegionDetection {
    // If syntax parsing is unavailable, failed, or still in progress, treat entire file as sensitive
    let Some(syntax) = syntax else {
        log::trace!("IME region detection: syntax unavailable, returning EntireFile");
        return ImeRegionDetection::entire_file(source.len());
    };

    let language = syntax.root_language();
    if !language_has_string_or_comment_types(loader, language) {
        log::trace!(
            "IME region detection: language has no string/comment types, returning EntireFile (FR-010)"
        );
        return ImeRegionDetection::entire_file(source.len());
    }

    // Check if the current node or any of its parents is a comment or string node
    // We need to check parents because the cursor might be in a child node
    // Also check for code blocks in document languages (like Markdown)
    let language_config = loader.language(language).config();
    let is_document_language = matches!(
        language_config.language_id.as_str(),
        "markdown" | "markdown-rustdoc" | "markdown.inline"
    );

    // The cursor position is probed directly first. Tree-sitter can resolve a
    // position exactly at a node's end byte to the node *following* it, so a
    // second pass probes the preceding position while still testing the
    // original cursor against each candidate span. This keeps the cursor at
    // the end of a line comment inside the region: anything typed there
    // continues the comment.
    let mut is_in_code_block = false;
    for (probe_index, probe_pos) in [Some(cursor_pos), cursor_pos.checked_sub(1)]
        .into_iter()
        .flatten()
        .enumerate()
    {
        // Get the node at probe position (including unnamed nodes)
        let Some(node) = syntax
            .tree()
            .root_node()
            .descendant_for_byte_range(probe_pos as u32, probe_pos as u32)
        else {
            continue;
        };

        let mut current_check = Some(node);
        while let Some(n) = current_check {
            let node_kind = n.kind();
            let node_start = n.start_byte() as usize;
            let node_end = n.end_byte() as usize;

            log::trace!(
                "IME region detection: checking node_kind={}, node_range=[{}, {})",
                node_kind,
                node_start,
                node_end
            );

            // For document languages, check if we're in a code block. Only the
            // direct probe counts: the preceding-position probe may reach into
            // the previous line's tree and must not leak a code block across
            // the line boundary.
            if probe_index == 0 && is_document_language && node_kind.contains("code_block") {
                is_in_code_block = true;
            }

            if let Some(detection) = node_ime_detection(
                node_kind,
                node_start,
                node_end,
                cursor_pos,
                source,
                language_config,
            ) {
                return detection;
            }

            current_check = n.parent();
        }
    }

    // If we didn't find a comment or string node at the cursor position,
    // check if this is a document language (like Markdown) where the entire file
    // should be treated as IME sensitive except for code blocks.
    if is_document_language {
        if is_in_code_block {
            // Cursor is in a code block, treat as non-sensitive (Code)
            log::trace!(
                "IME region detection: found code_block node in document language, returning Code"
            );
            return ImeRegionDetection::code();
        }
        // Not in a code block, treat entire file as sensitive
        log::trace!(
            "IME region detection: document language (not in code block), returning EntireFile"
        );
        return ImeRegionDetection::entire_file(source.len());
    }

    // For other languages, default to code region (non-sensitive)
    log::trace!("IME region detection: no comment/string node found, returning Code");
    ImeRegionDetection::code()
}

/// Check a single node against the cursor position for IME region detection.
///
/// Returns `Some` only when the cursor lies within the node's *content* span.
/// Positions inside the leading marker — or past the end — return `None` so
/// the caller keeps walking the parent nodes: a `//!` doc comment, for
/// example, resolves through its `doc_comment` child to the outer
/// `line_comment`, whose span starts right after the `//` marker.
fn node_ime_detection(
    node_kind: &str,
    node_start: usize,
    node_end: usize,
    cursor_pos: usize,
    source: RopeSlice,
    language_config: &LanguageConfiguration,
) -> Option<ImeRegionDetection> {
    // Check if node type contains "comment" (case-insensitive check for robustness)
    if contains_ignore_ascii_case(node_kind, b"comment") {
        let (start, end) = if node_kind == "comment_content" {
            // Content child nodes already exclude the comment marker
            (node_start, node_end)
        } else {
            comment_content_span(source, language_config, node_start, node_end)
        };
        if cursor_pos >= start && cursor_pos < end {
            log::trace!(
                "IME region detection: comment content span [{}, {}) contains cursor at {}, returning CommentContent",
                start,
                end,
                cursor_pos
            );
            return Some(ImeRegionDetection::new(
                ImeSensitiveRegion::CommentContent,
                Some((start, end)),
            ));
        }
    }

    // Check if node type contains "string" (excluding string_start and string_end)
    if node_kind.contains("string")
        && !node_kind.contains("string_start")
        && !node_kind.contains("string_end")
    {
        // Exclude leading quote symbols (FR-004), include the position right
        // before the trailing quote (FR-006)
        let (start, end) = if node_kind == "string_content" {
            // Content child nodes already exclude the quotes
            (node_start, node_end)
        } else {
            (node_start.saturating_add(1), node_end)
        };
        if cursor_pos >= start && cursor_pos < end {
            log::trace!(
                "IME region detection: string content span [{}, {}) contains cursor at {}, returning StringContent",
                start,
                end,
                cursor_pos
            );
            return Some(ImeRegionDetection::new(
                ImeSensitiveRegion::StringContent,
                Some((start, end)),
            ));
        }
    }

    None
}

/// Compute the content span `[start, end)` of a comment node for IME detection.
///
/// The leading marker is skipped by matching the node's text against the
/// language's configured comment tokens (FR-005):
///
/// - Line comments match the *shortest* `comment_tokens` entry, so `//!` and
///   `///` content begins right after the base `//` marker. The span extends
///   one byte past the node end: the cursor at the end of the line is still
///   inside the comment, since anything typed there continues it (FR-007).
/// - Block comments match the shortest `block_comment_tokens` start token;
///   their span keeps the node end exclusive so the position right after the
///   closing token is code again.
/// - When no configured token matches (exotic grammars), fall back to
///   skipping a single byte, preserving the historical behavior.
fn comment_content_span(
    source: RopeSlice,
    language_config: &LanguageConfiguration,
    node_start: usize,
    node_end: usize,
) -> (usize, usize) {
    let line_marker_len = language_config
        .comment_tokens
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|token| source_starts_with(source, node_start, node_end, token))
        .map(|token| token.len())
        .min();
    if let Some(len) = line_marker_len {
        // Grammars disagree on whether a line comment node swallows the
        // trailing newline (Rust doc comments like `//!` do, plain `//`
        // comments do not). Either way the span must include the cursor
        // sitting on the line terminator — typing there continues the
        // comment — but not the first position of the next line.
        let end = if node_end > node_start && source.get_byte(node_end - 1) == Some(b'\n') {
            node_end
        } else {
            node_end + 1
        };
        return (node_start + len, end);
    }

    let block_marker_len = language_config
        .block_comment_tokens
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|token| source_starts_with(source, node_start, node_end, &token.start))
        .map(|token| token.start.len())
        .min();
    if let Some(len) = block_marker_len {
        return (node_start + len, node_end);
    }

    (node_start.saturating_add(1), node_end)
}

/// Whether the rope text starting at `start` begins with `token`, bounded by `end`.
fn source_starts_with(source: RopeSlice, start: usize, end: usize, token: &str) -> bool {
    start + token.len() <= end
        && token
            .bytes()
            .enumerate()
            .all(|(i, byte)| source.get_byte(start + i) == Some(byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::default_lang_loader;
    use editor_core::Rope;
    use loader::syntax::Resources;

    fn rust_loader() -> (Loader, Language) {
        let loader = default_lang_loader(Resources::default());
        let rust = loader.language_for_name("rust").unwrap();
        (loader, rust)
    }

    /// Asserts the IME region detected at every byte position of `source`:
    /// positions within `sensitive` must yield `expected`, all others `Code`.
    #[track_caller]
    fn assert_ime_span(
        source: &str,
        sensitive: std::ops::RangeInclusive<usize>,
        expected: ImeSensitiveRegion,
    ) {
        let source = Rope::from_str(source);
        let (loader, language) = rust_loader();
        let syntax = Syntax::new(source.slice(..), language, &loader).unwrap();

        for pos in 0..=source.len() {
            let region =
                detect_ime_sensitive_region(Some(&syntax), source.slice(..), &loader, pos).region;
            let want = if sensitive.contains(&pos) {
                expected
            } else {
                ImeSensitiveRegion::Code
            };
            assert_eq!(region, want, "unexpected IME region at byte position {pos}");
        }
    }

    #[test]
    fn test_ime_region_line_comment_boundaries() {
        // `//` is a complete marker: only positions from right after the second
        // `/` — including the end-of-line cursor — are comment content.
        // Positions 0 and 1 sit inside the marker and must stay Code.
        let line = "// Heavily based on https://github.com/codemirror/closebrackets/";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len(),
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_inner_doc_comment_boundaries() {
        // Everything after the leading `//` of `//!` is content, including the
        // position right after the `!`.
        let line =
            "//! this module provides the functionality to insert the paired closing character.";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len(),
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_outer_doc_comment_boundaries() {
        let line = "/// outer doc comment";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len(),
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_comment_with_multibyte_content() {
        let line = "// 你好";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len(),
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_empty_line_comment() {
        // The end-of-line cursor of a bare `//` is still comment content.
        let line = "//";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len(),
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_block_comment_boundaries() {
        // The `/*` marker is skipped; the closing `*/` stays outside the span
        // (positions inside the closing token remain content per FR-007, but
        // the position after `*/` is Code again).
        let line = "/* block */";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len() - 1,
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_multiline_block_comment() {
        let line = "/* first\nsecond */";
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            2..=line.len() - 1,
            ImeSensitiveRegion::CommentContent,
        );
    }

    #[test]
    fn test_ime_region_string_boundaries() {
        // Quotes are excluded (FR-004); the position right before the trailing
        // quote is content (FR-006).
        let line = r#""string""#;
        assert_ime_span(
            &format!("{line}\nfn f() {{}}\n"),
            1..=line.len() - 1,
            ImeSensitiveRegion::StringContent,
        );
    }

    #[test]
    fn test_ime_region_span_extends_to_eol() {
        // The span cached for cursor-move handling must keep the end-of-line
        // cursor inside a line comment: for `// c` the span is [2, 5),
        // covering positions 2..=4 where position 4 is the newline.
        let source = Rope::from_str("// c\nfn f() {}\n");
        let (loader, language) = rust_loader();
        let syntax = Syntax::new(source.slice(..), language, &loader).unwrap();
        let detection = detect_ime_sensitive_region(Some(&syntax), source.slice(..), &loader, 2);
        assert_eq!(detection.region, ImeSensitiveRegion::CommentContent);
        assert_eq!(detection.node_range, Some((2, 5)));
    }
}
