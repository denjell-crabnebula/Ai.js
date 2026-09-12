// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! LLM relevance judge used by `POST /api/search/judge`.

use a2x_common::{ChatMessage, LlmBackend, SearchResult};

use super::prompts::parse_number_list;

/// Build the judge prompt. Descriptions are cut to 200 characters.
pub fn build_judge_prompt(query: &str, services: &[SearchResult]) -> String {
    let svc_list = services
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let desc: String = s.description.chars().take(200).collect();
            format!("{}. {}: {}", i + 1, s.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Given the user query, judge which services are relevant (can help fulfill any part of the query) and which are irrelevant.\n\nQuery: {query}\n\nServices:\n{svc_list}\n\nReturn ONLY the numbers of RELEVANT services, separated by commas (e.g. \"1,3,5\"). Return \"NONE\" if no service is relevant."
    )
}

/// Ask the LLM which services are relevant to `query`.
///
/// Returns `(service_id, relevant)` for every input service, in input
/// order. A failed LLM call marks every service irrelevant.
pub async fn judge_relevance(
    llm: &dyn LlmBackend,
    query: &str,
    services: &[SearchResult],
) -> Vec<(String, bool)> {
    let prompt = build_judge_prompt(query, services);
    let resp = llm.call(&[ChatMessage::user(prompt)], 0.0, Some(200)).await;
    let relevant: Vec<String> = parse_number_list(&resp.content, services.len())
        .into_iter()
        .map(|i| services[i].id.clone())
        .collect();
    services
        .iter()
        .map(|s| (s.id.clone(), relevant.contains(&s.id)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeLlm;
    use ap_support::testing::TestResult;

    #[tokio::test]
    async fn judge_marks_selected_services() -> TestResult {
        let llm = FakeLlm::new().on("judge which services are relevant", "2, 3");
        let services = vec![
            SearchResult::new("a", "A", "x"),
            SearchResult::new("b", "B", "y"),
            SearchResult::new("c", "C", "z"),
        ];
        let out = judge_relevance(&llm, "q", &services).await;
        assert_eq!(
            out,
            vec![
                ("a".to_string(), false),
                ("b".to_string(), true),
                ("c".to_string(), true)
            ]
        );
        let prompt = &llm.prompts()[0];
        assert!(prompt.contains("1. A: x\n2. B: y\n3. C: z"));
        Ok(())
    }

    #[tokio::test]
    async fn judge_none_and_failure() -> TestResult {
        let llm = FakeLlm::new().fail_on("Query: fail", "boom").with_default("NONE");
        let services = vec![SearchResult::new("a", "A", "x")];
        assert_eq!(
            judge_relevance(&llm, "fail", &services).await,
            vec![("a".to_string(), false)]
        );
        assert_eq!(
            judge_relevance(&llm, "q", &services).await,
            vec![("a".to_string(), false)]
        );
        Ok(())
    }
}
