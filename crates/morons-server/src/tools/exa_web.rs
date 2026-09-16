use super::{WebSearchResult, hosted_web::valid_url, valid_web_search_query};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExaWebResult {
    pub query: String,
    pub contract_revision: u16,
    pub results: Vec<WebSearchResult>,
}
impl ExaWebResult {
    pub(crate) fn is_valid(&self) -> bool {
        valid_web_search_query(&self.query)
            && self.contract_revision == 1
            && !self.results.is_empty()
            && self.results.len() <= super::MAX_WEB_SEARCH_RESULTS
            && self.results.iter().all(|result| {
                result.is_valid()
                    && valid_url(&result.url)
                    && !result.title.trim().is_empty()
                    && !result.snippet.trim().is_empty()
            })
    }
    pub(crate) fn summary(&self) -> String {
        let mut text =
            "Exa web · source excerpts (not independently verified; token usage not provided)"
                .to_owned();
        for result in &self.results {
            text.push_str(&format!(
                "\n{}\n{}\n{}",
                result.title, result.url, result.snippet
            ));
        }
        text
    }
}
