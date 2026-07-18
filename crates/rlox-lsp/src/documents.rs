use std::collections::HashMap;

use lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticSeverity as LspDiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, GotoDefinitionResponse, Location, NumberOrString, Position,
    PublishDiagnosticsParams, Range, SemanticToken, SemanticTokens, SemanticTokensParams,
    SemanticTokensResult, Uri,
};
use rlox::{
    AnalysisError, AnalysisLimit, DiagnosticPhase, DiagnosticSeverity, HighlightKind,
    LanguageAnalysis, MAX_ANALYSIS_SOURCE_BYTES, RevisionId, SemanticStatus, SourceDocument,
    SourceId, analyze,
};

use crate::text_index::{PositionMapping, TextIndex};

pub(crate) const MAX_OPEN_DOCUMENTS: usize = 32;
pub(crate) const MAX_URI_BYTES: usize = 4 * 1024;

pub(crate) struct DocumentStore {
    documents: HashMap<Uri, OpenDocument>,
    next_source_id: u64,
    next_revision: u64,
    diagnostic_data_support: bool,
}

struct OpenDocument {
    version: i32,
    source_id: SourceId,
    _revision: RevisionId,
    content: DocumentContent,
}

enum DocumentContent {
    Ready(Box<ReadyDocument>),
    Oversized(AnalysisError),
}

struct ReadyDocument {
    _source: SourceDocument,
    index: TextIndex,
    analysis: Result<LanguageAnalysis, AnalysisError>,
}

impl DocumentStore {
    pub(crate) fn new(diagnostic_data_support: bool) -> Self {
        Self {
            documents: HashMap::new(),
            next_source_id: 1,
            next_revision: 1,
            diagnostic_data_support,
        }
    }

    pub(crate) fn open(
        &mut self,
        params: DidOpenTextDocumentParams,
    ) -> Option<PublishDiagnosticsParams> {
        let item = params.text_document;
        if !uri_is_bounded(&item.uri)
            || self.documents.contains_key(&item.uri)
            || self.documents.len() >= MAX_OPEN_DOCUMENTS
        {
            return None;
        }
        let (source_id, revision) = self.allocate_open_identity()?;
        let document = OpenDocument {
            version: item.version,
            source_id,
            _revision: revision,
            content: build_content(&item.uri, source_id, revision, item.text),
        };
        let diagnostics = document.diagnostics(self.diagnostic_data_support);
        let uri = item.uri.clone();
        self.documents.insert(item.uri, document);
        Some(PublishDiagnosticsParams::new(
            uri,
            diagnostics,
            Some(item.version),
        ))
    }

    pub(crate) fn change(
        &mut self,
        params: DidChangeTextDocumentParams,
    ) -> Option<PublishDiagnosticsParams> {
        let identifier = params.text_document;
        if !uri_is_bounded(&identifier.uri) {
            return None;
        }
        let [change] =
            <[lsp_types::TextDocumentContentChangeEvent; 1]>::try_from(params.content_changes)
                .ok()?;
        if change.range.is_some() || change.range_length.is_some() {
            return None;
        }
        let current = self.documents.get(&identifier.uri)?;
        if identifier.version <= current.version {
            return None;
        }
        let source_id = current.source_id;
        let revision = self.allocate_revision()?;
        let updated = OpenDocument {
            version: identifier.version,
            source_id,
            _revision: revision,
            content: build_content(&identifier.uri, source_id, revision, change.text),
        };
        let diagnostics = updated.diagnostics(self.diagnostic_data_support);
        self.documents.insert(identifier.uri.clone(), updated);
        Some(PublishDiagnosticsParams::new(
            identifier.uri,
            diagnostics,
            Some(identifier.version),
        ))
    }

    pub(crate) fn close(
        &mut self,
        params: DidCloseTextDocumentParams,
    ) -> Option<PublishDiagnosticsParams> {
        if !uri_is_bounded(&params.text_document.uri) {
            return None;
        }
        let removed = self.documents.remove(&params.text_document.uri)?;
        Some(PublishDiagnosticsParams::new(
            params.text_document.uri,
            Vec::new(),
            Some(removed.version),
        ))
    }

    pub(crate) fn semantic_tokens(&self, params: SemanticTokensParams) -> SemanticTokensResult {
        let data = if uri_is_bounded(&params.text_document.uri) {
            self.documents
                .get(&params.text_document.uri)
                .and_then(OpenDocument::semantic_tokens)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })
    }

    pub(crate) fn definition(&self, params: GotoDefinitionParams) -> GotoDefinitionResponse {
        let uri = &params.text_document_position_params.text_document.uri;
        let document = uri_is_bounded(uri)
            .then(|| self.documents.get(uri))
            .flatten();
        let locations = document
            .map(|document| {
                document.definition(
                    &params.text_document_position_params.text_document.uri,
                    params.text_document_position_params.position,
                )
            })
            .unwrap_or_default();
        GotoDefinitionResponse::Array(locations)
    }

    fn allocate_open_identity(&mut self) -> Option<(SourceId, RevisionId)> {
        let source_id = self.next_source_id;
        let revision = self.next_revision;
        let next_source_id = source_id.checked_add(1)?;
        let next_revision = revision.checked_add(1)?;
        self.next_source_id = next_source_id;
        self.next_revision = next_revision;
        Some((SourceId(source_id), RevisionId(revision)))
    }

    fn allocate_revision(&mut self) -> Option<RevisionId> {
        let value = self.next_revision;
        self.next_revision = self.next_revision.checked_add(1)?;
        Some(RevisionId(value))
    }
}

fn uri_is_bounded(uri: &Uri) -> bool {
    uri.as_str().len() <= MAX_URI_BYTES
}

impl OpenDocument {
    fn diagnostics(&self, data_support: bool) -> Vec<LspDiagnostic> {
        match &self.content {
            DocumentContent::Oversized(error) => vec![limit_diagnostic(error)],
            DocumentContent::Ready(ready) => match &ready.analysis {
                Ok(analysis) => analysis
                    .diagnostics
                    .iter()
                    .map(|diagnostic| LspDiagnostic {
                        range: ready
                            .index
                            .span_to_range(diagnostic.span)
                            .unwrap_or_else(zero_range),
                        severity: Some(match diagnostic.severity {
                            DiagnosticSeverity::Error => LspDiagnosticSeverity::ERROR,
                            DiagnosticSeverity::Warning => LspDiagnosticSeverity::WARNING,
                        }),
                        code: Some(NumberOrString::String(diagnostic.code.clone())),
                        source: Some("rlox".to_owned()),
                        message: diagnostic.message.clone(),
                        data: data_support.then(|| {
                            serde_json::json!({
                                "phase": match diagnostic.phase {
                                    DiagnosticPhase::Scanner => "scanner",
                                    DiagnosticPhase::Parser => "parser",
                                    DiagnosticPhase::Compiler => "compiler",
                                    DiagnosticPhase::Runtime => "runtime",
                                    DiagnosticPhase::Worker => "worker",
                                }
                            })
                        }),
                        ..LspDiagnostic::default()
                    })
                    .collect(),
                Err(error) => vec![limit_diagnostic(error)],
            },
        }
    }

    fn semantic_tokens(&self) -> Option<Vec<SemanticToken>> {
        let DocumentContent::Ready(ready) = &self.content else {
            return None;
        };
        let Ok(analysis) = &ready.analysis else {
            return None;
        };
        let mut absolute = Vec::new();
        for highlight in &analysis.highlights {
            let token_type = match highlight.kind {
                HighlightKind::Keyword => 0,
                HighlightKind::Comment => 1,
                HighlightKind::String => 2,
                HighlightKind::Number => 3,
                HighlightKind::Identifier => 4,
                HighlightKind::Operator => 5,
                HighlightKind::Punctuation | HighlightKind::Invalid => continue,
            };
            for (line, start, length) in ready.index.span_segments(highlight.span)? {
                if length != 0 {
                    absolute.push((line, start, length, token_type));
                }
            }
        }

        let mut encoded = Vec::with_capacity(absolute.len());
        let mut previous_line = 0u32;
        let mut previous_start = 0u32;
        for (line, start, length, token_type) in absolute {
            let delta_line = line.checked_sub(previous_line)?;
            let delta_start = if delta_line == 0 {
                start.checked_sub(previous_start)?
            } else {
                start
            };
            encoded.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: 0,
            });
            previous_line = line;
            previous_start = start;
        }
        Some(encoded)
    }

    fn definition(&self, uri: &Uri, position: Position) -> Vec<Location> {
        let DocumentContent::Ready(ready) = &self.content else {
            return Vec::new();
        };
        let Ok(analysis) = &ready.analysis else {
            return Vec::new();
        };
        if analysis.semantic_status != SemanticStatus::Available {
            return Vec::new();
        }
        let Some(PositionMapping::Offset(offset)) = ready.index.raw_to_canonical(position) else {
            return Vec::new();
        };
        let Some(occurrence) = analysis.symbol_occurrences.iter().find(|occurrence| {
            occurrence.span.start.byte_offset < occurrence.span.end.byte_offset
                && occurrence.span.start.byte_offset <= offset
                && offset < occurrence.span.end.byte_offset
                && ready.index.span_to_range(occurrence.span).is_some()
        }) else {
            return Vec::new();
        };

        let mut targets = occurrence.declaration_targets.to_vec();
        targets.sort_by_key(|span| (span.start.byte_offset, span.end.byte_offset));
        targets.dedup();
        targets
            .into_iter()
            .filter_map(|span| {
                ready.index.span_to_range(span).map(|range| Location {
                    uri: uri.clone(),
                    range,
                })
            })
            .collect()
    }
}

fn build_content(
    uri: &Uri,
    source_id: SourceId,
    revision: RevisionId,
    raw: String,
) -> DocumentContent {
    if raw.len() > MAX_ANALYSIS_SOURCE_BYTES {
        return DocumentContent::Oversized(AnalysisError::LimitExceeded {
            limit: AnalysisLimit::SourceBytes,
            max: MAX_ANALYSIS_SOURCE_BYTES,
            actual: raw.len(),
        });
    }
    let source = SourceDocument::new(source_id, revision, uri.as_str(), &raw);
    let index = TextIndex::new(&raw, &source);
    let analysis = analyze(&source);
    DocumentContent::Ready(Box::new(ReadyDocument {
        _source: source,
        index,
        analysis,
    }))
}

fn limit_diagnostic(error: &AnalysisError) -> LspDiagnostic {
    let AnalysisError::LimitExceeded { limit, max, actual } = error;
    let (suffix, label) = match limit {
        AnalysisLimit::SourceBytes => ("source_bytes", "source byte"),
        AnalysisLimit::LexicalItems => ("lexical_items", "lexical item"),
        AnalysisLimit::Diagnostics => ("diagnostics", "diagnostic"),
        AnalysisLimit::NestingDepth => ("nesting_depth", "nesting depth"),
    };
    LspDiagnostic {
        range: zero_range(),
        severity: Some(LspDiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(format!("analysis.limit.{suffix}"))),
        source: Some("rlox".to_owned()),
        message: format!("Analysis {label} limit exceeded: maximum {max}, got {actual}."),
        ..LspDiagnostic::default()
    }
}

fn zero_range() -> Range {
    Range::new(Position::new(0, 0), Position::new(0, 0))
}

#[cfg(test)]
mod tests {
    use lsp_types::{
        DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
        TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
        VersionedTextDocumentIdentifier,
    };
    use rlox::MAX_ANALYSIS_SOURCE_BYTES;

    use super::{DocumentContent, DocumentStore, MAX_OPEN_DOCUMENTS, MAX_URI_BYTES};

    fn uri(index: usize) -> lsp_types::Uri {
        format!("file:///state/{index}.lox").parse().unwrap()
    }

    fn open_params(index: usize, version: i32, text: String) -> DidOpenTextDocumentParams {
        open_uri_params(uri(index), version, text)
    }

    fn open_uri_params(
        uri: lsp_types::Uri,
        version: i32,
        text: String,
    ) -> DidOpenTextDocumentParams {
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: "lox".to_owned(),
                version,
                text,
            },
        }
    }

    fn change_params(
        index: usize,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) -> DidChangeTextDocumentParams {
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri(index),
                version,
            },
            content_changes: changes,
        }
    }

    fn full(text: impl Into<String>) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.into(),
        }
    }

    #[test]
    fn ids_advance_only_for_accepted_revisions_and_reopen_gets_a_fresh_source() {
        let mut store = DocumentStore::new(false);
        assert!(
            store
                .open(open_params(0, 1, "var value = 1;".to_owned()))
                .is_some()
        );
        let first = store.documents.get(&uri(0)).unwrap();
        assert_eq!(first.source_id.0, 1);
        assert_eq!(first._revision.0, 1);
        assert_eq!((store.next_source_id, store.next_revision), (2, 2));

        assert!(store.open(open_params(0, 9, String::new())).is_none());
        assert!(
            store
                .change(change_params(0, 1, vec![full("stale")]))
                .is_none()
        );
        assert!(store.change(change_params(0, 2, Vec::new())).is_none());
        assert!(
            store
                .change(change_params(0, 2, vec![full("one"), full("two")]))
                .is_none()
        );
        assert!(
            store
                .change(change_params(1, 2, vec![full("missing")]))
                .is_none()
        );
        assert_eq!((store.next_source_id, store.next_revision), (2, 2));

        assert!(
            store
                .change(change_params(0, 2, vec![full("var updated = 2;")]))
                .is_some()
        );
        let changed = store.documents.get(&uri(0)).unwrap();
        assert_eq!(changed.source_id.0, 1);
        assert_eq!(changed._revision.0, 2);

        assert!(
            store
                .close(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: uri(0) },
                })
                .is_some()
        );
        assert!(store.open(open_params(0, 1, String::new())).is_some());
        let reopened = store.documents.get(&uri(0)).unwrap();
        assert_eq!(reopened.source_id.0, 2);
        assert_eq!(reopened._revision.0, 3);
    }

    #[test]
    fn oversized_sentinel_drops_content_and_a_newer_bounded_change_recovers() {
        let mut store = DocumentStore::new(false);
        assert!(
            store
                .open(open_params(0, 1, "x".repeat(MAX_ANALYSIS_SOURCE_BYTES + 1)))
                .is_some()
        );
        let oversized = store.documents.get(&uri(0)).unwrap();
        assert!(matches!(oversized.content, DocumentContent::Oversized(_)));
        assert_eq!(oversized.version, 1);
        assert_eq!(oversized.source_id.0, 1);
        assert_eq!(oversized._revision.0, 1);

        assert!(
            store
                .change(change_params(0, 2, vec![full("var recovered = 1;")]))
                .is_some()
        );
        let recovered = store.documents.get(&uri(0)).unwrap();
        assert!(matches!(recovered.content, DocumentContent::Ready(_)));
        assert_eq!(recovered.source_id.0, 1);
        assert_eq!(recovered._revision.0, 2);
    }

    #[test]
    fn rejected_capacity_open_consumes_no_identity() {
        let mut store = DocumentStore::new(false);
        for index in 0..MAX_OPEN_DOCUMENTS {
            assert!(store.open(open_params(index, 1, String::new())).is_some());
        }
        let counters = (store.next_source_id, store.next_revision);
        assert!(
            store
                .open(open_params(MAX_OPEN_DOCUMENTS, 1, String::new()))
                .is_none()
        );
        assert_eq!((store.next_source_id, store.next_revision), counters);
    }

    #[test]
    fn open_identity_allocation_is_atomic_at_counter_exhaustion() {
        let mut store = DocumentStore::new(false);
        store.next_source_id = 7;
        store.next_revision = u64::MAX;

        assert!(store.open(open_params(0, 1, String::new())).is_none());
        assert_eq!((store.next_source_id, store.next_revision), (7, u64::MAX));
        assert!(store.documents.is_empty());
    }

    #[test]
    fn over_cap_uri_is_rejected_before_identity_allocation() {
        let oversized_uri: lsp_types::Uri = format!("untitled:{}", "x".repeat(MAX_URI_BYTES))
            .parse()
            .unwrap();
        assert!(oversized_uri.as_str().len() > MAX_URI_BYTES);
        let mut store = DocumentStore::new(false);

        assert!(
            store
                .open(open_uri_params(oversized_uri.clone(), 1, String::new()))
                .is_none()
        );
        assert_eq!((store.next_source_id, store.next_revision), (1, 1));
        assert!(store.documents.is_empty());

        let synthetic_uri = "untitled:oxide-buffer".parse().unwrap();
        assert!(
            store
                .open(open_uri_params(synthetic_uri, 1, String::new()))
                .is_some()
        );
        assert!(store.open(open_params(0, 1, String::new())).is_some());
        let accepted_state = (
            store.documents.len(),
            store.next_source_id,
            store.next_revision,
        );

        assert!(
            store
                .change(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: oversized_uri.clone(),
                        version: 2,
                    },
                    content_changes: vec![full("ignored")],
                })
                .is_none()
        );
        assert!(
            store
                .close(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: oversized_uri },
                })
                .is_none()
        );
        assert_eq!(
            (
                store.documents.len(),
                store.next_source_id,
                store.next_revision,
            ),
            accepted_state
        );
    }
}
