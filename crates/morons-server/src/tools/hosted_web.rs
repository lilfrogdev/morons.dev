//! Canonical web results; independent of provider wire types and terminal rendering.
use super::SubagentUsage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebCitation {
    pub title: String,
    pub url: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebReceipt {
    pub model_id: String,
    pub contract_revision: u16,
    pub search_calls: u16,
    pub open_page_calls: u16,
    pub find_in_page_calls: u16,
    pub usage: SubagentUsage,
}
impl WebReceipt {
    pub(crate) fn is_valid(&self) -> bool {
        let u = self.usage;
        self.model_id == "gpt-5.5"
            && self.contract_revision == 1
            && self.search_calls > 0
            && u32::from(self.search_calls)
                + u32::from(self.open_page_calls)
                + u32::from(self.find_in_page_calls)
                <= 8
            && u.input_tokens <= 96_000
            && u.output_tokens <= 32_000
            && u.cached_input_tokens <= u.input_tokens
            && u.cache_write_input_tokens <= u.input_tokens
            && u.cached_input_tokens
                .checked_add(u.cache_write_input_tokens)
                .is_some_and(|sum| sum <= u.input_tokens)
            && u.reasoning_output_tokens <= u.output_tokens
            && u.input_tokens.checked_add(u.output_tokens) == Some(u.total_tokens)
    }
    pub(crate) fn summary(&self) -> String {
        format!(
            "OpenAI web · {} · {} search / {} open / {} find · {} input / {} output tokens (separate from coding usage)",
            self.model_id,
            self.search_calls,
            self.open_page_calls,
            self.find_in_page_calls,
            self.usage.input_tokens,
            self.usage.output_tokens
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostedWebResult {
    pub query: String,
    pub answer: String,
    pub citations: Vec<WebCitation>,
    pub receipt: WebReceipt,
}
impl HostedWebResult {
    pub(crate) fn is_valid(&self) -> bool {
        super::valid_web_search_query(&self.query)
            && !self.answer.trim().is_empty()
            && self.answer.len() <= 16 * 1024
            && self.receipt.is_valid()
            && !self.citations.is_empty()
            && self.citations.len() <= 10
            && self
                .citations
                .iter()
                .all(|c| c.title.len() <= 512 && valid_url(&c.url))
    }
    pub(crate) fn summary(&self) -> String {
        let mut text = format!("{}\n{}\nSources:", self.receipt.summary(), self.answer);
        for (i, c) in self.citations.iter().enumerate() {
            text.push_str(&format!("\n[{}] {}\n{}", i + 1, c.title, c.url));
        }
        text
    }
}
fn valid_url(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value.is_ascii()
        && !value
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        && value.parse::<http::Uri>().is_ok_and(|u| {
            matches!(u.scheme_str(), Some("http" | "https"))
                && u.authority().is_some_and(|a| !a.as_str().contains('@'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolInput, ToolOutput, ToolResult, validate_canonical_result_for_input};

    fn result() -> HostedWebResult {
        HostedWebResult {
            query: "public query".into(),
            answer: "Answer".into(),
            citations: vec![WebCitation {
                title: "Source".into(),
                url: "https://example.com/source".into(),
            }],
            receipt: WebReceipt {
                model_id: "gpt-5.5".into(),
                contract_revision: 1,
                search_calls: 1,
                open_page_calls: 0,
                find_in_page_calls: 0,
                usage: SubagentUsage {
                    input_tokens: 12,
                    cached_input_tokens: 2,
                    cache_write_input_tokens: 0,
                    output_tokens: 4,
                    reasoning_output_tokens: 1,
                    total_tokens: 16,
                },
            },
        }
    }
    #[test]
    fn canonical_hosted_web_results_bound_citations_identity_and_separate_usage() {
        let original = result();
        assert!(original.is_valid());
        let summary = original.summary();
        for text in [
            "gpt-5.5",
            "separate from coding usage",
            "Answer",
            "https://example.com/source",
        ] {
            assert!(summary.contains(text));
        }
        for change in 0..8 {
            let mut result = original.clone();
            match change {
                0 => result.receipt.model_id = "gpt-6-astra".into(),
                1 => result.receipt.search_calls = 0,
                2 => result.receipt.open_page_calls = 8,
                3 => result.receipt.usage.cached_input_tokens = 13,
                4 => result.answer = "x".repeat(16 * 1024 + 1),
                5 => result.citations[0].url = "https://user:pass@example.com".into(),
                6 => result.citations[0].url = "file:///private/source".into(),
                _ => result.citations = vec![result.citations[0].clone(); 11],
            }
            assert!(!result.is_valid());
        }
        let output = ToolResult::Ok {
            output: ToolOutput::OpenAiWeb { result: original },
        };
        assert!(!validate_canonical_result_for_input(
            &ToolInput::WebSearch {
                query: "foreign".into()
            },
            &output
        ));
    }
    #[test]
    fn historical_brave_payload_remains_exact_read_compatibility_not_native_receipt() {
        let bytes = r#"{"status":"ok","output":{"output":"web_search","query":"old query","results":[{"title":"Old source","url":"https://example.com/old","snippet":"Old snippet"}],"truncated":false}}"#;
        let result: ToolResult = serde_json::from_str(bytes).unwrap();
        assert_eq!(result.provider_output().unwrap(), bytes);
        assert!(matches!(
            result,
            ToolResult::Ok {
                output: ToolOutput::WebSearch { .. }
            }
        ));
        assert!(!result.summary().contains("OpenAI"));
    }
}
