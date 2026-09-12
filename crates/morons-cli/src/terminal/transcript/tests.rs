use super::*;
use crate::terminal::SafeText;
use ratatui::widgets::{Paragraph, Wrap};

fn check_parts(value: &TranscriptText) {
    assert!(value.part_count() > 0);
    assert_eq!(value.parts().collect::<String>(), value.as_str());
    assert_eq!(value.len_bytes(), value.as_str().len());
    assert!(value.len_bytes() <= MAX_TEXT_BYTES);
    assert!(value.text.capacity() <= MAX_TEXT_BYTES);
    for part in value.parts() {
        assert!(part.len() <= MAX_PART_BYTES);
        assert!(part.chars().count() <= MAX_PART_SCALARS);
        assert!(part.matches('\n').count() <= MAX_PART_NEWLINES);
        assert!(!part.chars().any(|c| c.is_control() && c != '\n'));
        let height = Paragraph::new(part)
            .wrap(Wrap { trim: false })
            .line_count(1);
        assert!(height < usize::from(u16::MAX));
    }
}

#[test]
fn transcript_retains_text_beyond_each_old_cap_and_late_tail() {
    for input in [
        format!("{}\nUNIQUE_TAIL", "x".repeat(70_000)),
        format!("{}UNIQUE_TAIL", "x\n".repeat(70_000)),
        format!("{}UNIQUE_TAIL", "é文字\n".repeat(10_000)),
    ] {
        let value = TranscriptText::from_untrusted(&input).unwrap();
        assert_eq!(value.as_str(), input);
        assert!(value.as_str().ends_with("UNIQUE_TAIL"));
        assert!(value.part_count() > 1);
        assert!(!value.escaped_graphemes());
        check_parts(&value);
        assert!(SafeText::from_untrusted(&input).was_truncated());
    }
}
#[test]
fn transcript_preserves_newline_and_empty_part_semantics() {
    for input in ["", "\n", "a\n", "\n\na", "a\n\n", "\u{1b}]hidden\u{7}"] {
        let value = TranscriptText::from_untrusted(input).unwrap();
        let expected = if input.starts_with('\u{1b}') {
            ""
        } else {
            input
        };
        assert_eq!(value.as_str(), expected);
        check_parts(&value);
    }
    let value = TranscriptText::from_untrusted(&"\n".repeat(257)).unwrap();
    assert_eq!(
        value.parts().map(|p| p.len()).collect::<Vec<_>>(),
        vec![128, 128, 1]
    );
}
#[test]
fn transcript_expands_tabs_and_keeps_graphemes_whole_at_part_edges() {
    for cluster in ["e\u{301}", "👩‍👩‍👧‍👦", "🇫🇷", "文字"] {
        let prefix = "x".repeat(MAX_PART_SCALARS - 1);
        let input = format!("{prefix}{cluster}\tTAIL");
        let value = TranscriptText::from_untrusted(&input).unwrap();
        assert_eq!(value.as_str(), format!("{prefix}{cluster}    TAIL"));
        assert!(!value.escaped_graphemes());
        check_parts(&value);
        // Every original grapheme boundary remains a permitted part boundary.
        let span = Span::raw(value.as_str());
        let mut boundaries = vec![0];
        let mut at = 0;
        for g in span.styled_graphemes(Style::default()) {
            at += g.symbol.len();
            boundaries.push(at);
        }
        for range in &value.parts {
            assert!(boundaries.contains(&range.start));
            assert!(boundaries.contains(&range.end));
        }
    }
}
#[test]
fn transcript_escapes_oversized_graphemes_without_losing_scalars_or_tail() {
    let ordinary = format!("ab{}", "\u{301}".repeat(63));
    let value = TranscriptText::from_untrusted(&ordinary).unwrap();
    assert!(!value.escaped_graphemes());
    let cluster = format!("a{}", "\u{301}".repeat(64));
    let expected = cluster
        .chars()
        .flat_map(char::escape_unicode)
        .collect::<String>();
    let input = format!("{}{cluster}\nTAIL", "x".repeat(MAX_PART_SCALARS - 1));
    let value = TranscriptText::from_untrusted(&input).unwrap();
    assert!(value.escaped_graphemes());
    assert_eq!(
        value.as_str(),
        format!("{}{expected}\nTAIL", "x".repeat(MAX_PART_SCALARS - 1))
    );
    check_parts(&value);
}
#[test]
fn transcript_controls_cross_prospective_boundaries_without_leaking_fragments() {
    for opening in [
        "\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_", "\u{009d}", "\u{0090}",
    ] {
        for ending in ["\u{7}", "\u{1b}\\", "\u{009c}"] {
            let prefix = "a".repeat(MAX_PART_SCALARS - 1);
            let input = format!("{prefix}{opening}{}SECRET{ending}TAIL", "z".repeat(20_000));
            let value = TranscriptText::from_untrusted(&input).unwrap();
            assert_eq!(value.as_str(), format!("{prefix}TAIL"));
            check_parts(&value);
        }
        assert_eq!(
            TranscriptText::from_untrusted(&format!("before{opening}unterminated"))
                .unwrap()
                .as_str(),
            "before"
        );
    }
    let value =
        TranscriptText::from_untrusted("a\u{1b}[31mb\u{009b}0mc\u{202e}d\u{2066}e\u{0000}\r\nf")
            .unwrap();
    assert_eq!(value.as_str(), "abcde\nf");
    check_parts(&value);
}
#[test]
fn transcript_source_boundary_and_worst_tab_expansion_are_bounded() {
    for input in ["x".repeat(MAX_SOURCE_BYTES), "\t".repeat(MAX_SOURCE_BYTES)] {
        let value = TranscriptText::from_untrusted(&input).unwrap();
        assert_eq!(
            value.len_bytes(),
            if input.starts_with('\t') {
                4 * MAX_SOURCE_BYTES
            } else {
                MAX_SOURCE_BYTES
            }
        );
        check_parts(&value);
    }
    assert_eq!(
        TranscriptText::from_untrusted(&"x".repeat(MAX_SOURCE_BYTES + 1)),
        Err(TranscriptTextError::SourceTooLarge)
    );
}
#[test]
fn transcript_debug_and_errors_are_content_free() {
    let text = TranscriptText::from_untrusted("DO_NOT_EXPOSE").unwrap();
    assert!(!format!("{text:?}").contains("DO_NOT_EXPOSE"));
    for error in [
        TranscriptTextError::SourceTooLarge,
        TranscriptTextError::RepresentationTooLarge,
    ] {
        assert!(!format!("{error:?} {error}").contains("DO_NOT_EXPOSE"));
        assert!(std::error::Error::source(&error).is_none());
    }
}
#[test]
fn metadata_still_has_its_old_prefix_and_truncation_contract() {
    let exact = "x".repeat(2048);
    let value = SafeText::from_untrusted(&exact);
    assert_eq!(value.as_str(), exact);
    assert!(!value.was_truncated());
    let value = SafeText::from_untrusted(&format!("{exact}\u{1b}]ignored\u{7}"));
    assert_eq!(value.as_str(), exact);
    assert!(!value.was_truncated());
    let value = SafeText::from_untrusted(&format!("{}\tTAIL", "x".repeat(2047)));
    assert_eq!(value.as_str(), format!("{} ", "x".repeat(2047)));
    assert!(value.was_truncated());
    let value = SafeText::from_untrusted(&"x\n".repeat(1024));
    assert_eq!(value.as_str(), format!("{}x", "x\n".repeat(1023)));
    assert!(value.was_truncated());
}
