// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Ranking metrics for vector search evaluation.

use std::collections::HashSet;

fn hits_in_top_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> usize {
    let top: HashSet<&String> = retrieved.iter().take(k).collect();
    top.iter().filter(|id| relevant.contains(**id)).count()
}

/// `P@K = |retrieved[:k] ∩ relevant| / k`.
pub fn precision_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    hits_in_top_k(retrieved, relevant, k) as f64 / k as f64
}

/// `R@K = |retrieved[:k] ∩ relevant| / |relevant|`.
pub fn recall_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    hits_in_top_k(retrieved, relevant, k) as f64 / relevant.len() as f64
}

/// 1.0 when any relevant document is in the top k.
pub fn hit_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if hits_in_top_k(retrieved, relevant, k) > 0 {
        1.0
    } else {
        0.0
    }
}

/// Reciprocal rank of the first relevant document.
pub fn mrr(retrieved: &[String], relevant: &HashSet<String>) -> f64 {
    for (i, id) in retrieved.iter().enumerate() {
        if relevant.contains(id) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Normalized discounted cumulative gain at k with binary relevance.
pub fn ndcg_at_k(retrieved: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    let dcg: f64 = retrieved
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, id)| relevant.contains(*id))
        .map(|(i, _)| 1.0 / ((i as f64 + 2.0).log2()))
        .sum();
    let n_relevant = k.min(relevant.len());
    let idcg: f64 = (1..=n_relevant).map(|i| 1.0 / ((i as f64 + 1.0).log2())).sum();
    if idcg > 0.0 { dcg / idcg } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    #[test]
    fn metrics_values() -> TestResult {
        let retrieved: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let relevant: HashSet<String> = ["b", "z"].iter().map(|s| s.to_string()).collect();
        assert!((precision_at_k(&retrieved, &relevant, 2) - 0.5).abs() < 1e-9);
        assert!((recall_at_k(&retrieved, &relevant, 4) - 0.5).abs() < 1e-9);
        assert_eq!(hit_at_k(&retrieved, &relevant, 1), 0.0);
        assert_eq!(hit_at_k(&retrieved, &relevant, 2), 1.0);
        assert!((mrr(&retrieved, &relevant) - 0.5).abs() < 1e-9);
        // dcg = 1/log2(3); idcg = 1/log2(2) + 1/log2(3)
        let expected = (1.0 / 3f64.log2()) / (1.0 + 1.0 / 3f64.log2());
        assert!((ndcg_at_k(&retrieved, &relevant, 4) - expected).abs() < 1e-9);
        assert_eq!(ndcg_at_k(&retrieved, &HashSet::new(), 4), 0.0);
        assert_eq!(recall_at_k(&retrieved, &HashSet::new(), 4), 0.0);
        Ok(())
    }
}
