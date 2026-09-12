// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Prompt templates and response parsing for A2X search.
//!
//! The prompt text is the algorithm and is kept byte-for-byte identical to
//! the Python source.

use super::models::SearchMode;

/// A category line for the category prompt.
#[derive(Clone, Copy, Debug)]
pub struct CategoryEntry<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub boundary: &'a str,
}

/// A service line for the service prompt.
#[derive(Clone, Copy, Debug)]
pub struct ServiceEntry<'a> {
    pub name: &'a str,
    pub description: &'a str,
}

/// Build the LLM prompt for category selection.
pub fn build_category_prompt(
    mode: SearchMode,
    query: &str,
    categories: &[CategoryEntry<'_>],
    parent_path: &str,
) -> String {
    let mut lines = Vec::with_capacity(categories.len());
    for (i, c) in categories.iter().enumerate() {
        if mode == SearchMode::GetOne && !c.boundary.is_empty() {
            lines.push(format!(
                "{}. {}: {} [{}]",
                i + 1,
                c.name,
                c.description,
                c.boundary
            ));
        } else {
            lines.push(format!("{}. {}: {}", i + 1, c.name, c.description));
        }
    }
    let categories_text = lines.join("\n");

    match mode {
        SearchMode::GetOne => {
            let path_hint = if parent_path.is_empty() {
                String::new()
            } else {
                format!("\nCurrent path: {parent_path}")
            };
            format!(
                "Think about what specific service or tool the user needs, then select the ONE category where such a service would be classified. Match by service function, not query topic.
{path_hint}

Query: {query}

Categories:
{categories_text}

Return ONLY one number (e.g. \"3\"), or \"NONE\" if no category is relevant."
            )
        }
        SearchMode::GetImportant => format!(
            "Analyze the user's request step by step: identify each distinct action or need, then select the categories that would contain services for those actions. Include a category if any part of the request could require it. Exclude categories that have no functional connection to any part of the request.

Query: {query}

Categories:
{categories_text}

Return ONLY the numbers separated by commas (e.g. \"1,3,5\"), or \"NONE\"."
        ),
        SearchMode::GetAll => format!(
            "Select ALL categories that could contain relevant services for the query. Think about what actions and entities the user needs, then match to categories by keyword and semantic similarity. Include ALL potentially relevant categories — when in doubt, include it.

Query: {query}

Categories:
{categories_text}

Return ONLY the numbers separated by commas (e.g. \"1,3,5\"), or \"NONE\"."
        ),
    }
}

/// Build the LLM prompt for service selection.
pub fn build_service_prompt(mode: SearchMode, query: &str, services: &[ServiceEntry<'_>]) -> String {
    let services_text = services
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}: {}", i + 1, s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");

    match mode {
        SearchMode::GetOne => format!(
            "Select the ONE service that most directly fulfills the user's primary need. Match the specific action described in the query to the service's described functionality.

Query: {query}

Services:
{services_text}

Return ONLY one number (e.g. \"3\"), or \"NONE\" if no service is relevant."
        ),
        SearchMode::GetImportant => format!(
            "Select services the user clearly needs to fulfill their request. A service should be included if the query explicitly requires its functionality. If two services do the same thing, include only the better match. Exclude services that are not directly requested.

Query: {query}

Services:
{services_text}

Return ONLY the numbers separated by commas (e.g. \"1,3,5\"), or \"NONE\"."
        ),
        SearchMode::GetAll => format!(
            "Select ALL services that could help fulfill the query. Include related and prerequisite services. When uncertain, include it.

Query: {query}

Services:
{services_text}

Return ONLY the numbers separated by commas (e.g. \"1,3,5\"), or \"NONE\"."
        ),
    }
}

/// Parse an LLM selection response into 0-based indices.
///
/// Accepts numbers separated by commas or whitespace, ignores tokens that
/// are not numbers or are out of range, and keeps only the first index in
/// `get_one` mode.
pub fn parse_selection(response: &str, max_index: usize, mode: SearchMode) -> Vec<usize> {
    let response = response.trim().to_uppercase();
    if response == "NONE" || response.is_empty() {
        return Vec::new();
    }
    let mut indices = Vec::new();
    for part in response.replace(',', " ").split_whitespace() {
        if let Ok(num) = part.trim().parse::<i64>() {
            if num >= 1 && (num as usize) <= max_index {
                indices.push(num as usize - 1);
            }
        }
    }
    if mode == SearchMode::GetOne && indices.len() > 1 {
        indices.truncate(1);
    }
    indices
}

/// Parse a plain number list without the `get_one` truncation. Used by the
/// judge endpoint and the incremental builder.
pub fn parse_number_list(response: &str, max_index: usize) -> Vec<usize> {
    parse_selection(response, max_index, SearchMode::GetAll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn selection_parsing() -> TestResult {
        assert_eq!(parse_selection("1,3,5", 5, SearchMode::GetAll), vec![0, 2, 4]);
        assert_eq!(
            parse_selection(" none ", 5, SearchMode::GetAll),
            Vec::<usize>::new()
        );
        assert_eq!(parse_selection("", 5, SearchMode::GetAll), Vec::<usize>::new());
        assert_eq!(parse_selection("2, 9, x, 3", 5, SearchMode::GetAll), vec![1, 2]);
        assert_eq!(parse_selection("The answer is 3", 5, SearchMode::GetAll), vec![2]);
        assert_eq!(parse_selection("1 2", 5, SearchMode::GetOne), vec![0]);
        assert_eq!(parse_selection("0", 5, SearchMode::GetAll), Vec::<usize>::new());
        assert_eq!(
            parse_selection("Categories: 2 and 4.", 5, SearchMode::GetAll),
            vec![1]
        );
        Ok(())
    }

    #[test]
    fn category_prompt_modes() -> TestResult {
        let cats = [
            CategoryEntry {
                name: "Travel",
                description: "Trips",
                boundary: "Not finance",
            },
            CategoryEntry {
                name: "Food",
                description: "Meals",
                boundary: "",
            },
        ];
        let p = build_category_prompt(SearchMode::GetOne, "book a flight", &cats, "Root");
        assert!(p.contains("1. Travel: Trips [Not finance]"));
        assert!(p.contains("2. Food: Meals\n"));
        assert!(p.contains("\nCurrent path: Root"));
        assert!(p.starts_with("Think about what specific service"));
        let p = build_category_prompt(SearchMode::GetAll, "q", &cats, "");
        assert!(p.contains("1. Travel: Trips\n"));
        assert!(p.starts_with("Select ALL categories"));
        let p = build_category_prompt(SearchMode::GetImportant, "q", &cats, "");
        assert!(p.starts_with("Analyze the user's request step by step"));
        Ok(())
    }

    #[test]
    fn service_prompt_modes() -> TestResult {
        let svcs = [ServiceEntry {
            name: "A",
            description: "does a",
        }];
        assert!(build_service_prompt(SearchMode::GetOne, "q", &svcs).starts_with("Select the ONE service"));
        assert!(
            build_service_prompt(SearchMode::GetImportant, "q", &svcs)
                .starts_with("Select services the user clearly")
        );
        let p = build_service_prompt(SearchMode::GetAll, "q", &svcs);
        assert!(p.starts_with("Select ALL services"));
        assert!(p.contains("1. A: does a"));
        Ok(())
    }
}
