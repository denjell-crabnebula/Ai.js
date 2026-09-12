// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Agent card URL fetching and description building.

use std::time::Duration;

use super::errors::RegistryError;
use super::models::AgentCard;

/// Fetch an A2A agent card from `url` and parse it.
///
/// Sends `Accept: application/json` and `User-Agent: A2XRegistry/1.0` with a
/// ten second timeout like the Python `urlopen` call.
pub async fn fetch_agent_card(url: &str) -> Result<AgentCard, RegistryError> {
    fetch_agent_card_with(url, Duration::from_secs(10)).await
}

/// [`fetch_agent_card`] with an explicit timeout.
pub async fn fetch_agent_card_with(url: &str, timeout: Duration) -> Result<AgentCard, RegistryError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| RegistryError::Fetch(format!("cannot build HTTP client: {e}")))?;
    let resp = client
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "A2XRegistry/1.0")
        .send()
        .await
        .map_err(|e| RegistryError::Fetch(format!("failed to fetch agent card from {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(RegistryError::Fetch(format!(
            "HTTP Error {}: {}",
            status.as_u16(),
            url
        )));
    }
    let raw = resp
        .bytes()
        .await
        .map_err(|e| RegistryError::Fetch(format!("failed to read agent card from {url}: {e}")))?;
    let text = String::from_utf8_lossy(&raw);
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| RegistryError::Fetch(format!("invalid agent card JSON at {url}: {e}")))?;
    AgentCard::from_value(value)
        .map_err(|e| RegistryError::Fetch(format!("invalid agent card at {url}: {e}")))
}

/// Build a rich description by aggregating the card description and skills.
///
/// Example: `Provides weather info. Skills: [get_forecast] Get weather
/// forecast for a city; [get_alerts] Get weather alerts`.
pub fn build_description(card: &AgentCard) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !card.description.is_empty() {
        parts.push(format!("{}.", card.description.trim_end_matches('.')));
    }
    if !card.skills.is_empty() {
        let skill_strs: Vec<String> = card
            .skills
            .iter()
            .map(|s| {
                let label = if s.name.is_empty() {
                    String::new()
                } else {
                    format!("[{}]", s.name)
                };
                format!("{label} {}", s.description).trim().to_string()
            })
            .collect();
        if !skill_strs.is_empty() {
            parts.push(format!("Skills: {}", skill_strs.join("; ")));
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;
    use serde_json::json;

    #[test]
    fn description_aggregates_skills() -> TestResult {
        let card = AgentCard::from_value(json!({
            "name": "w", "description": "Provides weather info...",
            "skills": [
                {"name": "get_forecast", "description": "Get forecast"},
                {"name": "", "description": "anon"}
            ]
        }))?;
        assert_eq!(
            build_description(&card),
            "Provides weather info. Skills: [get_forecast] Get forecast; anon"
        );
        let empty = AgentCard::from_value(json!({"name": "w", "description": ""}))?;
        assert_eq!(build_description(&empty), "");
        Ok(())
    }

    #[tokio::test]
    async fn fetch_from_local_server() -> TestResult {
        use axum::{Router, routing::get};
        let app = Router::new()
            .route(
                "/card",
                get(|| async { axum::Json(json!({"name": "n", "description": "d"})) }),
            )
            .route("/bad", get(|| async { "not json" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let card = fetch_agent_card(&format!("http://{addr}/card")).await?;
        assert_eq!(card.name, "n");
        assert!(fetch_agent_card(&format!("http://{addr}/bad")).await.is_err());
        assert!(fetch_agent_card(&format!("http://{addr}/missing")).await.is_err());
        Ok(())
    }
}
