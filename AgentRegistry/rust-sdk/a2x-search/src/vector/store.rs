// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! In-memory cosine vector store persisted as JSON.
//!
//! Replaces `ChromaStore`. One collection is one file,
//! `<persist_dir>/<collection_name>.json`, holding the recorded embedding
//! model and every document with its vector. Every mutation is written
//! back atomically.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::read_json;

/// A stored document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredDoc {
    pub id: String,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// A query result.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryHit {
    pub id: String,
    pub text: String,
    /// Cosine distance (`1 - cosine similarity`), like Chroma's cosine space.
    pub distance: f32,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    collection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedding_model: Option<String>,
    #[serde(default)]
    docs: Vec<StoredDoc>,
}

/// Chroma-style collection name for a dataset: lowercase, `-` to `_`.
pub fn collection_name_for_dataset(dataset: &str) -> String {
    dataset.to_lowercase().replace('-', "_")
}

/// Cosine similarity store for one collection.
#[derive(Clone, Debug)]
pub struct VectorStore {
    collection_name: String,
    path: Option<PathBuf>,
    embedding_model: Option<String>,
    docs: IndexMap<String, StoredDoc>,
}

impl VectorStore {
    /// File that backs `collection_name` under `persist_dir`.
    pub fn collection_file(persist_dir: &Path, collection_name: &str) -> PathBuf {
        persist_dir.join(format!("{collection_name}.json"))
    }

    /// Get or create a persisted collection. An existing file keeps its
    /// recorded embedding model; a new one records `embedding_model`.
    pub fn open(collection_name: &str, persist_dir: &Path, embedding_model: Option<&str>) -> Result<Self> {
        let path = Self::collection_file(persist_dir, collection_name);
        if path.exists() {
            let file: StoreFile = read_json(&path)?;
            let mut store = Self {
                collection_name: collection_name.to_string(),
                path: Some(path),
                embedding_model: file.embedding_model,
                docs: file.docs.into_iter().map(|d| (d.id.clone(), d)).collect(),
            };
            if store.embedding_model.is_none() && embedding_model.is_some() {
                store.embedding_model = embedding_model.map(str::to_string);
                store.save()?;
            }
            Ok(store)
        } else {
            let store = Self {
                collection_name: collection_name.to_string(),
                path: Some(path),
                embedding_model: embedding_model.map(str::to_string),
                docs: IndexMap::new(),
            };
            store.save()?;
            Ok(store)
        }
    }

    /// A collection that is never written to disk.
    pub fn in_memory(collection_name: &str, embedding_model: Option<&str>) -> Self {
        Self {
            collection_name: collection_name.to_string(),
            path: None,
            embedding_model: embedding_model.map(str::to_string),
            docs: IndexMap::new(),
        }
    }

    /// Delete the collection file. Returns true when it existed.
    pub fn delete_collection(collection_name: &str, persist_dir: &Path) -> Result<bool> {
        let path = Self::collection_file(persist_dir, collection_name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn collection_name(&self) -> &str {
        &self.collection_name
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The embedding model recorded for the collection, if any.
    pub fn stored_embedding_model(&self) -> Option<&str> {
        self.embedding_model.as_deref()
    }

    pub fn set_embedding_model(&mut self, model: Option<&str>) -> Result<()> {
        self.embedding_model = model.map(str::to_string);
        self.save()
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = StoreFile {
            collection: self.collection_name.clone(),
            embedding_model: self.embedding_model.clone(),
            docs: self.docs.values().cloned().collect(),
        };
        // Compact JSON: a pretty-printed vector per line would bloat the file.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
        }
        let bytes = serde_json::to_vec(&file).map_err(|e| Error::Other(e.to_string()))?;
        a2x_common::atomic::atomic_write_bytes(path, &bytes).map_err(|e| Error::io(path, e))
    }

    fn check_lengths(ids: &[String], texts: &[String], embeddings: &[Vec<f32>]) -> Result<()> {
        if ids.len() != texts.len() || ids.len() != embeddings.len() {
            return Err(Error::Invalid(format!(
                "ids ({}), texts ({}) and embeddings ({}) must have the same length",
                ids.len(),
                texts.len(),
                embeddings.len()
            )));
        }
        Ok(())
    }

    /// Add documents. Fails on an id that already exists.
    pub fn add(&mut self, ids: &[String], texts: &[String], embeddings: &[Vec<f32>]) -> Result<()> {
        Self::check_lengths(ids, texts, embeddings)?;
        if let Some(dup) = ids.iter().find(|id| self.docs.contains_key(*id)) {
            return Err(Error::Invalid(format!("document id already exists: {dup}")));
        }
        self.upsert(ids, texts, embeddings)
    }

    /// Insert or replace documents.
    pub fn upsert(&mut self, ids: &[String], texts: &[String], embeddings: &[Vec<f32>]) -> Result<()> {
        Self::check_lengths(ids, texts, embeddings)?;
        for ((id, text), embedding) in ids.iter().zip(texts).zip(embeddings) {
            self.docs.insert(
                id.clone(),
                StoredDoc {
                    id: id.clone(),
                    text: text.clone(),
                    embedding: embedding.clone(),
                },
            );
        }
        self.save()
    }

    /// Delete documents by id (unknown ids are ignored).
    pub fn delete_ids(&mut self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for id in ids {
            self.docs.shift_remove(id);
        }
        self.save()
    }

    /// The `top_k` nearest documents by cosine distance.
    pub fn query(&self, embedding: &[f32], top_k: usize) -> Vec<QueryHit> {
        let mut hits: Vec<QueryHit> = self
            .docs
            .values()
            .map(|d| QueryHit {
                id: d.id.clone(),
                text: d.text.clone(),
                distance: 1.0 - cosine_similarity(embedding, &d.embedding),
            })
            .collect();
        hits.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);
        hits
    }

    pub fn count(&self) -> usize {
        self.docs.len()
    }

    /// `id -> text` for every document.
    pub fn get_all_docs(&self) -> IndexMap<String, String> {
        self.docs
            .iter()
            .map(|(id, d)| (id.clone(), d.text.clone()))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&StoredDoc> {
        self.docs.get(id)
    }

    /// Remove every document and forget the recorded embedding model, so
    /// the next `open` with a model records the new one.
    pub fn clear(&mut self) -> Result<()> {
        self.docs.clear();
        self.embedding_model = None;
        self.save()
    }
}

/// Cosine similarity; zero when either vector is empty or zero length.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::TestResult;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn persistence_and_semantics() -> TestResult {
        let dir = tempfile::tempdir()?;
        let name = collection_name_for_dataset("My-Data");
        assert_eq!(name, "my_data");
        let mut store = VectorStore::open(&name, dir.path(), Some("m1"))?;
        store.add(
            &s(&["a", "b"]),
            &s(&["ta", "tb"]),
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        )?;
        assert!(store.add(&s(&["a"]), &s(&["x"]), &[vec![1.0, 0.0]]).is_err());
        store.upsert(&s(&["a"]), &s(&["ta2"]), &[vec![1.0, 0.1]])?;
        assert_eq!(store.count(), 2);

        let reopened = VectorStore::open(&name, dir.path(), Some("m2"))?;
        assert_eq!(reopened.stored_embedding_model(), Some("m1"));
        assert_eq!(reopened.get_all_docs()["a"], "ta2");
        let hits = reopened.query(&[1.0, 0.0], 5);
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits.len(), 2);
        assert_eq!(reopened.query(&[1.0, 0.0], 1).len(), 1);

        let mut store = reopened;
        store.delete_ids(&s(&["b", "missing"]))?;
        assert_eq!(store.count(), 1);
        store.clear()?;
        assert_eq!(store.count(), 0);
        assert_eq!(store.stored_embedding_model(), None);
        let again = VectorStore::open(&name, dir.path(), Some("m2"))?;
        assert_eq!(again.stored_embedding_model(), Some("m2"));
        assert!(VectorStore::delete_collection(&name, dir.path())?);
        assert!(!VectorStore::delete_collection(&name, dir.path())?);
        assert_eq!(cosine_similarity(&[], &[1.0]), 0.0);
        Ok(())
    }
}
