//! Versioned, language-independent diagnostic schema.
//!
//! A [`Diagnostic`] is the canonical unit of feedback the pre-compiler emits
//! (from the semantic front-end, the checker, or the codegen back-end). The
//! schema is intentionally serializable to stable JSON so that tooling built
//! on the daemon — editors, CI, other agents — can parse diagnostics emitted
//! today regardless of the eventual implementation language of any component.
//!
//! The byte-level JSON shape is pinned by a golden-file test under `tests/`.
#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms))]

use serde::{Deserialize, Serialize};

/// The on-disk schema identifier for this crate's diagnostic shape.
///
/// Bumped (`SCHEMA_VERSION + 1`) only on breaking shape changes, accompanied by
/// a migration path (see `rpc-cache`).
pub const SCHEMA_VERSION: u32 = 1;

/// Severity of a diagnostic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A type, borrow, or resolution error.
    Error,
    /// A lint or stylistic concern.
    Warning,
    /// An informational note attached to another diagnostic.
    Note,
    /// A suggestion offered to the user.
    Help,
}

/// A byte range span anchored to a virtual file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSpan {
    /// Virtual file path this span refers to.
    pub file: String,
    /// Inclusive byte offset of the span start.
    pub start: u32,
    /// Exclusive byte offset of the span end.
    pub end: u32,
}

impl TextSpan {
    /// Create a new span over `[start, end)` in `file`.
    pub const fn new(file: String, start: u32, end: u32) -> Self {
        Self { file, start, end }
    }
}

/// A machine-actionable suggestion attached to a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Human-readable description of the suggestion.
    pub description: String,
    /// The span the suggestion applies to.
    pub span: TextSpan,
    /// The text to substitute in place of the spanned region.
    pub replacement: String,
}

/// A single diagnostic message emitted by the pre-compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Schema version this diagnostic was serialized with.
    pub schema_version: u32,
    /// Stable, tool-facing diagnostic code (e.g. `E0308`).
    pub code: String,
    /// Severity level.
    pub severity: Severity,
    /// Human-readable explanation.
    pub message: String,
    /// Source spans the diagnostic covers. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<TextSpan>,
    /// Suggested fixes. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// Create a diagnostic at the current schema version.
    pub fn new(code: impl Into<String>, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            code: code.into(),
            severity,
            message: message.into(),
            spans: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    /// Attach a source span to this diagnostic.
    pub fn with_span(mut self, span: TextSpan) -> Self {
        self.spans.push(span);
        self
    }

    /// Attach a suggested fix to this diagnostic.
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/diagnostic-v1.json");

    fn sample() -> Diagnostic {
        Diagnostic::new("E0308", Severity::Error, "mismatched types")
            .with_span(TextSpan::new("src/main.rs".to_string(), 10, 11))
            .with_suggestion(Suggestion {
                description: "consider adding a semicolon".to_string(),
                span: TextSpan::new("src/main.rs".to_string(), 10, 11),
                replacement: ";".to_string(),
            })
    }

    /// The serialized pretty-JSON shape must match the golden file byte-for-byte.
    #[test]
    fn golden_serialization() {
        let diag = sample();
        let rendered = serde_json::to_string_pretty(&diag).unwrap();
        let golden = std::fs::read_to_string(GOLDEN).unwrap();
        assert_eq!(rendered, golden.trim_end_matches('\n'));
    }

    /// The golden file must round-trip back into the same diagnostic.
    #[test]
    fn golden_round_trip() {
        let golden = std::fs::read_to_string(GOLDEN).unwrap();
        let diag: Diagnostic = serde_json::from_str(&golden).unwrap();
        assert_eq!(diag.schema_version, SCHEMA_VERSION);
        assert_eq!(diag.code, "E0308");
        assert_eq!(diag.severity, Severity::Error);
        let re = serde_json::to_string_pretty(&diag).unwrap();
        assert_eq!(re, golden.trim_end_matches('\n'));
    }

    /// Omitted (empty) fields must not appear in the serialized output.
    #[test]
    fn empty_fields_omitted() {
        let diag = Diagnostic::new("L0001", Severity::Warning, "something");
        let json = serde_json::to_string(&diag).unwrap();
        assert!(!json.contains("spans"));
        assert!(!json.contains("suggestions"));
    }
}
