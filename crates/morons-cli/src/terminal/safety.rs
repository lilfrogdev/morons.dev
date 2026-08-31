use std::fmt;

use morons_protocol::{MAX_OPENCODE_API_KEY_BYTES, OpenCodeApiKey, OpenCodeApiKeyError};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_PRESENTED_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_PRESENTED_TEXT_SCALARS: usize = 16 * 1024;
pub const MAX_PRESENTED_LINES: usize = 1024;
pub const MAX_PRESENTED_LINE_SCALARS: usize = 2048;
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
const TAB_WIDTH: usize = 4;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SafeText {
    text: String,
    truncated: bool,
}

impl SafeText {
    #[must_use]
    pub fn from_untrusted(input: &str) -> Self {
        let mut text = String::with_capacity(input.len().min(MAX_PRESENTED_TEXT_BYTES));
        let mut scalars = 0_usize;
        let mut line_scalars = 0_usize;
        let mut lines = 1_usize;
        let mut truncated = false;

        for character in input.chars() {
            if is_bidirectional_control(character) {
                continue;
            }
            if character == '\n' {
                if lines >= MAX_PRESENTED_LINES || !push_bounded(&mut text, character, &mut scalars)
                {
                    truncated = true;
                    break;
                }
                lines += 1;
                line_scalars = 0;
                continue;
            }
            if character == '\t' {
                for _ in 0..TAB_WIDTH {
                    if line_scalars >= MAX_PRESENTED_LINE_SCALARS
                        || !push_bounded(&mut text, ' ', &mut scalars)
                    {
                        truncated = true;
                        break;
                    }
                    line_scalars += 1;
                }
                if truncated {
                    break;
                }
                continue;
            }
            if character.is_control() {
                continue;
            }
            if line_scalars >= MAX_PRESENTED_LINE_SCALARS
                || !push_bounded(&mut text, character, &mut scalars)
            {
                truncated = true;
                break;
            }
            line_scalars += 1;
        }

        Self { text, truncated }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn first_line(&self) -> &str {
        self.text
            .split_once('\n')
            .map_or(&self.text, |(line, _)| line)
    }

    #[must_use]
    pub const fn was_truncated(&self) -> bool {
        self.truncated
    }
}

impl fmt::Debug for SafeText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SafeText")
            .field("text_bytes", &self.text.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Default, PartialEq, Eq)]
pub struct PromptBuffer {
    text: String,
}

impl PromptBuffer {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    #[cfg(test)]
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.text.len()
    }

    pub fn push_character(&mut self, character: char) -> bool {
        if character == '\t' {
            return (0..TAB_WIDTH).all(|_| self.push_visible(' '));
        }
        if character == '\n' {
            return self.push_visible(character);
        }
        if character.is_control() || is_bidirectional_control(character) {
            return false;
        }
        self.push_visible(character)
    }

    pub fn push_paste(&mut self, paste: &str) {
        for character in paste.chars() {
            let _ = self.push_character(character);
            if self.text.len() >= MAX_PROMPT_BYTES {
                break;
            }
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    fn push_visible(&mut self, character: char) -> bool {
        let next_length = self.text.len().checked_add(character.len_utf8());
        if next_length.is_none_or(|length| length > MAX_PROMPT_BYTES) {
            return false;
        }
        self.text.push(character);
        true
    }
}

impl fmt::Debug for PromptBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromptBuffer")
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

pub struct CredentialBuffer {
    bytes: Zeroizing<Vec<u8>>,
}

impl Default for CredentialBuffer {
    fn default() -> Self {
        Self {
            bytes: Zeroizing::new(Vec::with_capacity(MAX_OPENCODE_API_KEY_BYTES)),
        }
    }
}

impl CredentialBuffer {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn push_character(&mut self, character: char) -> bool {
        if !character.is_ascii() || !matches!(character as u8, 0x21..=0x7e) {
            return false;
        }
        if self.bytes.len() >= MAX_OPENCODE_API_KEY_BYTES {
            return false;
        }
        self.bytes.push(character as u8);
        true
    }

    pub fn push_paste(&mut self, paste: &str) -> bool {
        let bytes = paste.as_bytes();
        if !bytes.iter().all(|byte| matches!(byte, 0x21..=0x7e))
            || self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|length| length > MAX_OPENCODE_API_KEY_BYTES)
        {
            return false;
        }
        self.bytes.extend_from_slice(bytes);
        true
    }

    pub fn backspace(&mut self) {
        if let Some(byte) = self.bytes.last_mut() {
            byte.zeroize();
            self.bytes.pop();
        }
    }

    pub fn into_api_key(mut self) -> Result<OpenCodeApiKey, OpenCodeApiKeyError> {
        OpenCodeApiKey::new(std::mem::take(&mut *self.bytes))
    }

    #[cfg(test)]
    #[must_use]
    pub fn len_bytes(&self) -> usize {
        self.bytes.len()
    }
}

impl fmt::Debug for CredentialBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialBuffer")
            .field("secret_bytes", &"[REDACTED]")
            .finish()
    }
}

fn push_bounded(text: &mut String, character: char, scalars: &mut usize) -> bool {
    if *scalars >= MAX_PRESENTED_TEXT_SCALARS
        || text
            .len()
            .checked_add(character.len_utf8())
            .is_none_or(|length| length > MAX_PRESENTED_TEXT_BYTES)
    {
        return false;
    }
    text.push(character);
    *scalars += 1;
    true
}

const fn is_bidirectional_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_terminal_controls_and_bidi_controls_are_removed() {
        let input = "safe\u{1b}]52;c;clipboard\u{7}\u{202e}txt\u{2066}\nnext\tcolumn";
        let safe = SafeText::from_untrusted(input);

        assert_eq!(safe.as_str(), "safe]52;c;clipboardtxt\nnext    column");
        assert!(
            !safe
                .as_str()
                .chars()
                .any(|character| character != '\n' && character.is_control())
        );
        assert!(!safe.as_str().chars().any(is_bidirectional_control));
    }

    #[test]
    fn presentation_is_bounded_by_bytes_lines_and_line_length() {
        let bytes = SafeText::from_untrusted(&"é".repeat(MAX_PRESENTED_TEXT_BYTES));
        assert!(bytes.as_str().len() <= MAX_PRESENTED_TEXT_BYTES);
        assert!(bytes.was_truncated());

        let lines = SafeText::from_untrusted(&"x\n".repeat(MAX_PRESENTED_LINES + 1));
        assert!(lines.as_str().lines().count() <= MAX_PRESENTED_LINES);
        assert!(lines.was_truncated());

        let line = SafeText::from_untrusted(&"x".repeat(MAX_PRESENTED_LINE_SCALARS + 1));
        assert_eq!(line.as_str().chars().count(), MAX_PRESENTED_LINE_SCALARS);
        assert!(line.was_truncated());
    }

    #[test]
    fn single_line_presentation_never_forwards_a_layout_control() {
        let value = SafeText::from_untrusted("first\nsecond");
        assert_eq!(value.first_line(), "first");
        assert!(!value.first_line().contains('\n'));
    }

    #[test]
    fn safe_text_debug_does_not_expose_content() {
        let value = SafeText::from_untrusted("sensitive transcript text");
        let debug = format!("{value:?}");
        assert!(!debug.contains("sensitive transcript text"));
        assert!(debug.contains("text_bytes"));
    }

    #[test]
    fn prompt_rejects_controls_and_has_a_redacted_bound() {
        let mut prompt = PromptBuffer::default();
        prompt.push_paste("hello\u{1b}\u{202e}\nworld");
        assert_eq!(prompt.as_str(), "hello\nworld");
        assert!(!format!("{prompt:?}").contains("hello"));

        prompt.push_paste(&"x".repeat(MAX_PROMPT_BYTES + 1));
        assert_eq!(prompt.len_bytes(), MAX_PROMPT_BYTES);
        prompt.backspace();
        assert_eq!(prompt.len_bytes(), MAX_PROMPT_BYTES - 1);
        prompt.clear();
        assert!(prompt.is_empty());
    }

    #[test]
    fn credential_input_is_atomic_bounded_and_redacted() {
        let mut credential = CredentialBuffer::default();
        assert!(credential.push_paste("not-a-real-key"));
        assert!(!credential.push_paste(" invalid"));
        assert_eq!(credential.len_bytes(), 14);
        assert!(!format!("{credential:?}").contains("not-a-real-key"));

        credential.backspace();
        assert!(credential.push_character('y'));
        let key = credential
            .into_api_key()
            .expect("visible ASCII credential should be valid");
        assert_eq!(
            key,
            OpenCodeApiKey::new("not-a-real-key").expect("test credential should be valid")
        );

        let mut oversized = CredentialBuffer::default();
        assert!(!oversized.push_paste(&"x".repeat(MAX_OPENCODE_API_KEY_BYTES + 1)));
        assert!(oversized.is_empty());
    }
}
