use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};

use super::{TranscriptText, TranscriptTextError};

impl TranscriptText {
    pub(crate) fn from_markdown(input: &str) -> Result<Self, TranscriptTextError> {
        let plain = Self::from_untrusted(input)?;
        if plain.escaped_graphemes() {
            return Ok(plain);
        }
        let mut output = String::new();
        let mut styles = Vec::new();
        let mut stack = vec![Style::default()];
        let mut lists = Vec::new();
        let mut block_end = 0;
        let mut text_end = 0;
        for (event, range) in
            Parser::new_ext(plain.as_str(), Options::ENABLE_STRIKETHROUGH).into_offset_iter()
        {
            if stack.len() == 1 && range.start >= block_end {
                output.extend(
                    plain.as_str()[block_end..range.start]
                        .chars()
                        .filter(|character| *character == '\n'),
                );
            }
            let closes_code = matches!(event, Event::End(TagEnd::CodeBlock));
            let style = *stack.last().expect("base style");
            let needs_line_break = !output.is_empty() && !output.ends_with('\n');
            let mut emit = |text: &str, style: Style| -> Result<(), TranscriptTextError> {
                let safe = Self::from_untrusted(text)?;
                let start = output.len();
                output.push_str(safe.as_str());
                if output.len() > crate::transcript_budget::MAX_SOURCE_BYTES {
                    return Err(TranscriptTextError::RepresentationTooLarge);
                }
                if !safe.as_str().is_empty() && style != Style::default() {
                    styles.push((start..output.len(), style));
                }
                Ok(())
            };
            let result = match event {
                Event::Start(tag) => {
                    let next = match tag {
                        Tag::Strong | Tag::Heading { .. } => style.add_modifier(Modifier::BOLD),
                        Tag::Emphasis => style.add_modifier(Modifier::ITALIC),
                        Tag::Strikethrough => style.add_modifier(Modifier::CROSSED_OUT),
                        Tag::Link { .. } => {
                            style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
                        }
                        Tag::CodeBlock(_) => style.fg(Color::Yellow),
                        _ => style,
                    };
                    let result = match tag {
                        Tag::List(start) => {
                            let nested = !lists.is_empty();
                            lists.push(start);
                            if nested && needs_line_break {
                                emit("\n", style)
                            } else {
                                Ok(())
                            }
                        }
                        Tag::Item => {
                            let prefix = match lists.last_mut() {
                                Some(Some(number)) => {
                                    let prefix = format!("{number}. ");
                                    *number = number.saturating_add(1);
                                    prefix
                                }
                                _ => "• ".to_owned(),
                            };
                            let indent = "  ".repeat(lists.len().saturating_sub(1));
                            emit(&format!("{indent}{prefix}"), style)
                        }
                        Tag::BlockQuote(_) => emit("│ ", style.fg(Color::DarkGray)),
                        _ => Ok(()),
                    };
                    stack.push(next);
                    result
                }
                Event::End(tag) => {
                    stack.pop();
                    if tag == TagEnd::List(false) || tag == TagEnd::List(true) {
                        lists.pop();
                    }
                    match tag {
                        TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::Item
                        | TagEnd::BlockQuote(_)
                        | TagEnd::List(_)
                            if plain.as_str()[range.clone()].ends_with('\n')
                                && !output.ends_with('\n') =>
                        {
                            output.push('\n');
                        }
                        _ => {}
                    }
                    Ok(())
                }
                Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                    emit(&text, style)
                }
                Event::Code(text) => emit(&text, style.fg(Color::Yellow)),
                Event::SoftBreak => {
                    let gap = plain.as_str().get(text_end..range.start).unwrap_or("");
                    if gap.chars().all(|character| matches!(character, ' ' | '\t')) {
                        emit(gap, style)?;
                    }
                    emit("\n", style)
                }
                Event::HardBreak => emit("\n", style),
                Event::Rule => {
                    let rule = if plain.as_str()[range.clone()].ends_with('\n') {
                        "────\n"
                    } else {
                        "────"
                    };
                    emit(rule, style.fg(Color::DarkGray))
                }
                _ => Ok(()),
            };
            text_end = range.end;
            if stack.len() == 1 {
                block_end = range.end;
                // The closing fence excludes its line ending; the code already supplies one.
                if closes_code && plain.as_str()[block_end..].starts_with('\n') {
                    block_end += 1;
                }
            }
            if result.is_err() {
                return Ok(plain);
            }
        }
        output.extend(
            plain.as_str()[block_end..]
                .chars()
                .filter(|character| *character == '\n'),
        );
        // Revalidate combined graphemes and bounds after parsing decoded entities.
        let Ok(mut rendered) = Self::from_untrusted(&output) else {
            return Ok(plain);
        };
        if rendered.as_str() != output {
            return Ok(plain);
        }
        rendered.styles = styles;
        Ok(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_unicode_and_fenced_block_spacing() {
        let input = "wide 界 e\u{301} 👩‍💻 \nline\n\nEND";
        assert_eq!(
            TranscriptText::from_markdown(input).unwrap().as_str(),
            input
        );
        for (input, expected) in [
            ("```bash\necho\n```\n\n- item", "echo\n\n• item"),
            ("```\necho\n```\nnext", "echo\nnext"),
            ("```\necho\n```", "echo\n"),
        ] {
            assert_eq!(
                TranscriptText::from_markdown(input).unwrap().as_str(),
                expected
            );
        }
    }

    #[test]
    fn block_boundaries_and_blank_input() {
        for (input, expected) in [
            ("", ""),
            ("\n\n", "\n\n"),
            ("---\nnext", "────\nnext"),
            ("---\n\nnext", "────\n\nnext"),
            ("---", "────"),
            ("> **quote**\n\nnext", "│ quote\n\nnext"),
            ("1. one\n2. two", "1. one\n2. two"),
            ("- first\n  - nested\n- last", "• first\n  • nested\n• last"),
            ("```\n```\nnext", "next"),
            (
                "- outer\n  - middle\n    - inner\n- end",
                "• outer\n  • middle\n    • inner\n• end",
            ),
            ("> > nested\n>\n> next", "│ │ nested\nnext"),
            ("~~~rust\n**literal**\n~~~\nafter", "**literal**\nafter"),
        ] {
            assert_eq!(
                TranscriptText::from_markdown(input).unwrap().as_str(),
                expected,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn streaming_prefixes_have_valid_nonoverlapping_styles() {
        let input = "> **bold *界 italic***\n\n- first\n  - nested\n\n```rust\nlet x = 1;\n```\n---\n[link](https://example.com) &#27;";
        for end in (0..=input.len()).filter(|end| input.is_char_boundary(*end)) {
            let text = TranscriptText::from_markdown(&input[..end]).unwrap();
            let mut previous_end = 0;
            for (range, _) in &text.styles {
                assert!(range.start >= previous_end);
                assert!(text.as_str().get(range.clone()).is_some());
                previous_end = range.end;
            }
            for part in text.parts() {
                for line in part.split('\n') {
                    let rendered = text.line(line);
                    let joined: String = rendered
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    assert_eq!(joined, line);
                }
            }
            assert!(!text.as_str().contains('\u{1b}'));
        }
    }

    #[test]
    fn markdown_styles_and_literal_code() {
        let text = TranscriptText::from_markdown(
            "# Title\n\n**bold** and *italic* `code`\n\n```bash\necho '**literal**'\n```\n\n- item",
        )
        .unwrap();
        assert_eq!(
            text.as_str(),
            "Title\n\nbold and italic code\n\necho '**literal**'\n\n• item"
        );
        for (word, modifier) in [("bold", Modifier::BOLD), ("italic", Modifier::ITALIC)] {
            assert!(
                text.styles
                    .iter()
                    .any(|(range, style)| &text.as_str()[range.clone()] == word
                        && style.add_modifier.contains(modifier))
            );
        }
    }

    #[test]
    fn unclosed_fences_entities_and_terminal_controls_are_safe() {
        let text = TranscriptText::from_markdown(
            "**safe** &#27; [link](https://example.com)\n\n```bash\n**literal**\u{1b}[31m",
        )
        .unwrap();
        assert!(!text.as_str().contains('\u{1b}'));
        assert!(text.as_str().contains("**literal**"));
    }
}
