mod markdown;

use std::{fmt, ops::Range};

use ratatui::{style::Style, text::Span};

use super::safety::visible_characters;

use crate::transcript_budget::MAX_SOURCE_BYTES;
const MAX_TEXT_BYTES: usize = 6 * MAX_SOURCE_BYTES;
const MAX_PART_BYTES: usize = 16 * 1024;
const MAX_PART_SCALARS: usize = 2048;
const MAX_PART_NEWLINES: usize = 128;
const MAX_GRAPHEME_BYTES: usize = 128;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TranscriptText {
    text: String,
    parts: Vec<Range<usize>>,
    escaped_graphemes: bool,
    styles: Vec<(Range<usize>, Style)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptTextError {
    SourceTooLarge,
    RepresentationTooLarge,
}

impl fmt::Display for TranscriptTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceTooLarge => "transcript source exceeds byte limit",
            Self::RepresentationTooLarge => "transcript representation exceeds byte limit",
        })
    }
}

impl std::error::Error for TranscriptTextError {}

impl TranscriptText {
    pub fn from_untrusted(input: &str) -> Result<Self, TranscriptTextError> {
        if input.len() > MAX_SOURCE_BYTES {
            return Err(TranscriptTextError::SourceTooLarge);
        }
        let sanitized: String = visible_characters(input).collect();
        let mut builder = PartBuilder {
            value: Self {
                text: String::with_capacity(sanitized.len()),
                parts: Vec::new(),
                escaped_graphemes: false,
                styles: Vec::new(),
            },
            start: 0,
            scalars: 0,
            newlines: 0,
        };
        // Ratatui filters newline graphemes, so pass them through separately.
        for segment in sanitized.split_inclusive('\n') {
            let line = segment.strip_suffix('\n').unwrap_or(segment);
            let span = Span::raw(line);
            for grapheme in span.styled_graphemes(Style::default()) {
                if grapheme.symbol.len() > MAX_GRAPHEME_BYTES {
                    builder.value.escaped_graphemes = true;
                    for character in grapheme.symbol.chars() {
                        let mut bytes = [0_u8; 10];
                        let mut length = 0;
                        for escaped in character.escape_unicode() {
                            bytes[length] = escaped as u8;
                            length += 1;
                        }
                        let unit = std::str::from_utf8(&bytes[..length])
                            .expect("Unicode escapes are ASCII");
                        builder.push(unit)?;
                    }
                } else {
                    builder.push(grapheme.symbol)?;
                }
            }
            if segment.ends_with('\n') {
                builder.push("\n")?;
            }
        }
        builder
            .value
            .parts
            .push(builder.start..builder.value.text.len());
        Ok(builder.value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn line<'a>(&'a self, text: &'a str) -> ratatui::text::Line<'a> {
        let start = text.as_ptr() as usize - self.text.as_ptr() as usize;
        let end = start + text.len();
        let mut spans = Vec::new();
        let mut cursor = start;
        let first = self.styles.partition_point(|(range, _)| range.end <= start);
        for (range, style) in self.styles[first..]
            .iter()
            .take_while(|(range, _)| range.start < end)
        {
            let left = range.start.max(start);
            let right = range.end.min(end);
            if cursor < left {
                spans.push(Span::raw(&self.text[cursor..left]));
            }
            if left < right {
                spans.push(Span::styled(&self.text[left..right], *style));
            }
            cursor = right;
        }
        if cursor < end {
            spans.push(Span::raw(&self.text[cursor..end]));
        }
        ratatui::text::Line::from(spans)
    }

    pub fn parts(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().map(|range| &self.as_str()[range.clone()])
    }

    #[must_use]
    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.text.len()
    }

    #[must_use]
    pub fn escaped_graphemes(&self) -> bool {
        self.escaped_graphemes
    }
}

impl fmt::Debug for TranscriptText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranscriptText")
            .field("text_bytes", &self.len_bytes())
            .field("part_count", &self.part_count())
            .field("escaped_graphemes", &self.escaped_graphemes)
            .finish()
    }
}

struct PartBuilder {
    value: TranscriptText,
    start: usize,
    scalars: usize,
    newlines: usize,
}

impl PartBuilder {
    fn push(&mut self, unit: &str) -> Result<(), TranscriptTextError> {
        let length = self.value.text.len();
        let next_length = length
            .checked_add(unit.len())
            .filter(|length| *length <= MAX_TEXT_BYTES)
            .ok_or(TranscriptTextError::RepresentationTooLarge)?;
        let scalars = unit.chars().count();
        let newlines = usize::from(unit == "\n");
        if next_length - self.start > MAX_PART_BYTES
            || self.scalars + scalars > MAX_PART_SCALARS
            || self.newlines + newlines > MAX_PART_NEWLINES
        {
            self.value.parts.push(self.start..length);
            self.start = length;
            self.scalars = 0;
            self.newlines = 0;
        }
        if next_length > self.value.text.capacity() {
            let capacity = self.value.text.capacity().max(64).saturating_mul(2);
            self.value
                .text
                .reserve_exact(capacity.min(MAX_TEXT_BYTES).max(next_length) - length);
        }
        self.value.text.push_str(unit);
        self.scalars += scalars;
        self.newlines += newlines;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
