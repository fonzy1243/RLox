use std::ops::Range as IndexRange;

use lsp_types::{Position, Range};
use rlox::{RevisionId, SourceDocument, SourceId, SourceSpan, TextPosition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PositionMapping {
    Offset(usize),
    OutsideCompilerText,
}

#[cfg(test)]
mod tests {
    use lsp_types::Position;
    use rlox::{RevisionId, SourceDocument, SourceId, SourceSpan, TextPosition};

    use super::{PositionMapping, TextIndex};

    const SOURCE_ID: SourceId = SourceId(41);
    const REVISION: RevisionId = RevisionId(9);

    fn indexed(raw: &str) -> (SourceDocument, TextIndex) {
        let source = SourceDocument::new(SOURCE_ID, REVISION, "index.lox", raw);
        let index = TextIndex::new(raw, &source);
        (source, index)
    }

    #[test]
    fn raw_positions_handle_bom_crlf_lone_cr_trailing_lines_and_utf16_edges() {
        let (source, index) = indexed("\u{feff}name\r\n😀x\ry\n");
        assert_eq!(
            index.raw_to_canonical(Position::new(0, 0)),
            Some(PositionMapping::OutsideCompilerText)
        );
        assert_eq!(
            index.raw_to_canonical(Position::new(0, 1)),
            Some(PositionMapping::Offset(0))
        );
        assert_eq!(
            index.raw_to_canonical(Position::new(0, u32::MAX)),
            Some(PositionMapping::Offset(4))
        );
        assert_eq!(
            index.raw_to_canonical(Position::new(1, 0)),
            Some(PositionMapping::Offset(5))
        );
        assert_eq!(index.raw_to_canonical(Position::new(1, 1)), None);
        assert_eq!(
            index.raw_to_canonical(Position::new(1, 2)),
            Some(PositionMapping::Offset(9))
        );
        assert_eq!(
            index.raw_to_canonical(Position::new(1, 3)),
            Some(PositionMapping::Offset(10))
        );
        assert_eq!(
            index.raw_to_canonical(Position::new(3, 0)),
            Some(PositionMapping::Offset(source.text.len()))
        );
        assert_eq!(index.raw_to_canonical(Position::new(4, 0)), None);
    }

    #[test]
    fn canonical_spans_validate_identity_coordinates_boundaries_and_order() {
        let (_, index) = indexed("\u{feff}name\r\nnext");
        let valid = SourceSpan {
            source_id: SOURCE_ID,
            revision: REVISION,
            start: TextPosition {
                byte_offset: 0,
                line: 1,
                column: 1,
            },
            end: TextPosition {
                byte_offset: 4,
                line: 1,
                column: 5,
            },
        };
        assert_eq!(
            index.span_to_range(valid),
            Some(lsp_types::Range::new(
                Position::new(0, 1),
                Position::new(0, 5)
            ))
        );

        let mut invalid = valid;
        invalid.source_id = SourceId(42);
        assert_eq!(index.span_to_range(invalid), None);
        invalid = valid;
        invalid.revision = RevisionId(10);
        assert_eq!(index.span_to_range(invalid), None);
        invalid = valid;
        invalid.start.line = 2;
        assert_eq!(index.span_to_range(invalid), None);
        invalid = valid;
        invalid.start.byte_offset = 2;
        invalid.start.column = 3;
        invalid.end.byte_offset = 1;
        assert_eq!(index.span_to_range(invalid), None);
    }

    #[test]
    fn multiline_segments_exclude_every_raw_line_break_sequence() {
        let (source, index) = indexed("\"a😀\r\nb\r\"\n");
        let span = SourceSpan {
            source_id: SOURCE_ID,
            revision: REVISION,
            start: TextPosition {
                byte_offset: 0,
                line: 1,
                column: 1,
            },
            end: TextPosition {
                byte_offset: source.text.len() - 1,
                line: 3,
                column: 2,
            },
        };
        assert_eq!(
            index.span_segments(span),
            Some(vec![(0, 0, 4), (1, 0, 1), (2, 0, 1)])
        );

        let mut wrong_revision = span;
        wrong_revision.revision = RevisionId(99);
        assert_eq!(index.span_segments(wrong_revision), None);

        let mut inconsistent_coordinates = span;
        inconsistent_coordinates.end.line = 2;
        assert_eq!(index.span_segments(inconsistent_coordinates), None);
    }
}

#[derive(Debug)]
pub(crate) struct TextIndex {
    source_id: SourceId,
    revision: RevisionId,
    canonical_len: usize,
    canonical_boundaries: Vec<CanonicalBoundary>,
    raw_boundaries: Vec<RawBoundary>,
    raw_lines: Vec<RawLine>,
}

#[derive(Debug, Clone, Copy)]
struct CanonicalBoundary {
    byte_offset: usize,
    compiler_line: usize,
    compiler_column: usize,
    raw_position: Position,
}

#[derive(Debug, Clone, Copy)]
struct RawBoundary {
    utf16_column: u32,
    canonical_byte: Option<usize>,
}

#[derive(Debug)]
struct RawLine {
    boundaries: IndexRange<usize>,
    utf16_len: u32,
}

impl TextIndex {
    pub(crate) fn new(raw: &str, source: &SourceDocument) -> Self {
        let mut canonical = String::with_capacity(raw.len());
        let mut canonical_boundaries = Vec::with_capacity(raw.chars().count() + 1);
        let mut raw_boundaries = Vec::with_capacity(raw.chars().count() + 1);
        let mut raw_lines = Vec::new();

        let stripped_bom = raw.starts_with('\u{feff}');
        let mut raw_byte = 0usize;
        let mut raw_line = 0u32;
        let mut raw_utf16_column = 0u32;
        let mut compiler_line = 1usize;
        let mut compiler_column = 1usize;

        raw_boundaries.push(RawBoundary {
            utf16_column: 0,
            canonical_byte: (!stripped_bom).then_some(0),
        });
        if stripped_bom {
            raw_byte = '\u{feff}'.len_utf8();
            raw_utf16_column = 1;
            raw_boundaries.push(RawBoundary {
                utf16_column: raw_utf16_column,
                canonical_byte: Some(0),
            });
            canonical_boundaries.push(CanonicalBoundary {
                byte_offset: 0,
                compiler_line,
                compiler_column,
                raw_position: Position::new(raw_line, raw_utf16_column),
            });
        } else {
            canonical_boundaries.push(CanonicalBoundary {
                byte_offset: 0,
                compiler_line,
                compiler_column,
                raw_position: Position::new(raw_line, 0),
            });
        }

        let mut line_boundary_start = 0usize;
        loop {
            if raw_byte == raw.len() {
                raw_lines.push(RawLine {
                    boundaries: line_boundary_start..raw_boundaries.len(),
                    utf16_len: raw_utf16_column,
                });
                break;
            }

            let remaining = &raw[raw_byte..];
            let line_break_len = if remaining.starts_with("\r\n") {
                Some(2)
            } else if remaining.starts_with('\r') || remaining.starts_with('\n') {
                Some(1)
            } else {
                None
            };
            if let Some(line_break_len) = line_break_len {
                raw_lines.push(RawLine {
                    boundaries: line_boundary_start..raw_boundaries.len(),
                    utf16_len: raw_utf16_column,
                });
                canonical.push('\n');
                raw_byte += line_break_len;
                raw_line = raw_line.checked_add(1).expect("bounded source line count");
                raw_utf16_column = 0;
                compiler_line += 1;
                compiler_column = 1;
                line_boundary_start = raw_boundaries.len();
                raw_boundaries.push(RawBoundary {
                    utf16_column: 0,
                    canonical_byte: Some(canonical.len()),
                });
                canonical_boundaries.push(CanonicalBoundary {
                    byte_offset: canonical.len(),
                    compiler_line,
                    compiler_column,
                    raw_position: Position::new(raw_line, 0),
                });
                continue;
            }

            let scalar = remaining
                .chars()
                .next()
                .expect("raw byte offset remains on a character boundary");
            canonical.push(scalar);
            raw_byte += scalar.len_utf8();
            raw_utf16_column = raw_utf16_column
                .checked_add(u32::try_from(scalar.len_utf16()).expect("UTF-16 scalar length fits"))
                .expect("bounded UTF-16 line length");
            compiler_column += 1;
            raw_boundaries.push(RawBoundary {
                utf16_column: raw_utf16_column,
                canonical_byte: Some(canonical.len()),
            });
            canonical_boundaries.push(CanonicalBoundary {
                byte_offset: canonical.len(),
                compiler_line,
                compiler_column,
                raw_position: Position::new(raw_line, raw_utf16_column),
            });
        }

        assert_eq!(
            canonical,
            source.text.as_ref(),
            "LSP raw/canonical normalization drifted from SourceDocument"
        );
        Self {
            source_id: source.id,
            revision: source.revision,
            canonical_len: canonical.len(),
            canonical_boundaries,
            raw_boundaries,
            raw_lines,
        }
    }

    pub(crate) fn raw_to_canonical(&self, position: Position) -> Option<PositionMapping> {
        let line = self.raw_lines.get(usize::try_from(position.line).ok()?)?;
        let character = position.character.min(line.utf16_len);
        let boundaries = &self.raw_boundaries[line.boundaries.clone()];
        let boundary = boundaries
            .binary_search_by_key(&character, |boundary| boundary.utf16_column)
            .ok()
            .map(|index| boundaries[index])?;
        Some(match boundary.canonical_byte {
            Some(byte_offset) => PositionMapping::Offset(byte_offset),
            None => PositionMapping::OutsideCompilerText,
        })
    }

    pub(crate) fn span_to_range(&self, span: SourceSpan) -> Option<Range> {
        if span.source_id != self.source_id
            || span.revision != self.revision
            || span.start.byte_offset > span.end.byte_offset
            || span.end.byte_offset > self.canonical_len
        {
            return None;
        }
        let start = self.compiler_position_to_raw(span.start)?;
        let end = self.compiler_position_to_raw(span.end)?;
        Some(Range::new(start, end))
    }

    pub(crate) fn span_segments(&self, span: SourceSpan) -> Option<Vec<(u32, u32, u32)>> {
        let range = self.span_to_range(span)?;
        let start = range.start;
        let end = range.end;
        if start.line == end.line {
            let length = end.character.checked_sub(start.character)?;
            return Some(
                (length != 0)
                    .then_some((start.line, start.character, length))
                    .into_iter()
                    .collect(),
            );
        }

        let mut segments = Vec::new();
        let first_line = self.raw_lines.get(usize::try_from(start.line).ok()?)?;
        let first_length = first_line.utf16_len.checked_sub(start.character)?;
        if first_length != 0 {
            segments.push((start.line, start.character, first_length));
        }
        for line_number in start.line.checked_add(1)?..end.line {
            let line = self.raw_lines.get(usize::try_from(line_number).ok()?)?;
            if line.utf16_len != 0 {
                segments.push((line_number, 0, line.utf16_len));
            }
        }
        if end.character != 0 {
            segments.push((end.line, 0, end.character));
        }
        Some(segments)
    }

    fn compiler_position_to_raw(&self, position: TextPosition) -> Option<Position> {
        let boundary = self.boundary_at(position.byte_offset)?;
        (boundary.compiler_line == position.line && boundary.compiler_column == position.column)
            .then_some(boundary.raw_position)
    }

    fn boundary_at(&self, byte_offset: usize) -> Option<CanonicalBoundary> {
        self.canonical_boundaries
            .binary_search_by_key(&byte_offset, |boundary| boundary.byte_offset)
            .ok()
            .map(|index| self.canonical_boundaries[index])
    }
}
