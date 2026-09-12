// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Backend startup: the warmup sequence and the server entry point.
//!
//! `run_warmup` loads the registry, the auth store, the heartbeat module,
//! the optional cluster module and warms the search engine. Progress is
//! published on `warmup.stage` for `/api/warmup-status`.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use super::state::AppState;
use crate::auth::AuthStore;
use crate::heartbeat::{HeartbeatStore, HeartbeatSweeper};

/// Execute the full startup sequence.
pub async fn run_warmup(state: Arc<AppState>) {
    let t0 = Instant::now();
    let warm = state.warmup.clone();
    let result: Result<(), String> = async {
        // 1. Registry
        warm.stage("注册服务加载...", 5);
        let registry = state.registry.clone();
        let registry_states = registry.startup().await.map_err(|e| e.to_string())?;
        for (ds, st) in &registry_states {
            tracing::info!(
                "  Registry {}: {} services, taxonomy={}",
                ds,
                registry.list_services(ds).len(),
                st
            );
        }

        // 1b. Auth store
        match AuthStore::load_or_none(Some(&state.auth_data_dir)) {
            Ok(Some(store)) => {
                tracing::info!(
                    "  Auth store loaded ({} principals, {} keys) at {}",
                    store.list_principals().len(),
                    store.list_keys(None).len(),
                    store.data_dir().display()
                );
                state.set_auth_store(Some(Arc::new(store)));
            }
            Ok(None) => {
                tracing::info!("  Auth not initialized - registry runs in anonymous mode");
                state.set_auth_store(None);
            }
            Err(e) => {
                tracing::error!("  Auth store load failed: {}", e);
                state.set_auth_store(None);
            }
        }

        // 1c. Heartbeat module (always loaded; per namespace opt-in)
        let hb_store = Arc::new(HeartbeatStore::new(registry.clone()));
        registry.set_unhealthy_check(Some(hb_store.clone()));
        let recovered: Vec<(String, String, i64)> = registry
            .list_datasets()
            .iter()
            .flat_map(|ds| {
                registry
                    .list_entries(ds)
                    .into_iter()
                    .filter_map(|e| e.lease_ttl.map(|t| (ds.clone(), e.service_id, t)))
                    .collect::<Vec<_>>()
            })
            .collect();
        if !recovered.is_empty() {
            hb_store.recover_from_persisted(&recovered);
        }
        state.set_heartbeat_store(Some(hb_store.clone()));
        let sweeper = Arc::new(HeartbeatSweeper::new(
            registry.clone(),
            hb_store,
            state.sweeper_period,
        ));
        sweeper.start();
        state.set_sweeper(Some(sweeper));
        tracing::info!(
            "  Heartbeat store loaded (recovered {} leases into grace window)",
            recovered.len()
        );

        // 1d. Cluster module (opt-in through the adapter)
        #[cfg(feature = "cluster")]
        {
            super::adapters::cluster::init(&state).await;
        }

        // Vector sync side effects only when the feature is available.
        if a2x_common::feature_flags::has(a2x_common::feature_flags::Feature::Vector) {
            let search = state.search.clone();
            registry.set_on_service_changed(Some(Arc::new(move |ds: &str| search.schedule_vector_sync(ds))));
        }
        tracing::info!("Warmup: registry done ({:.1}s)", t0.elapsed().as_secs_f64());

        // 2. Taxonomy caches
        warm.stage("加载分类树...", 20);
        for ds in state.search.discover_datasets() {
            match state.taxonomy.get_taxonomy_tree(&ds) {
                Ok(_) => tracing::info!("  Taxonomy cached: {}", ds),
                Err(e) => tracing::warn!("  Taxonomy failed for {}: {}", ds, e),
            }
        }

        // 3. Search engines
        warm.stage("加载 A2X 搜索引擎...", 35);
        let paths: Vec<_> = state
            .search
            .discover_datasets()
            .iter()
            .map(|ds| state.search.paths(ds))
            .collect();
        state.search.engine().warmup(&paths).await;
        tracing::info!("Warmup: engines done ({:.1}s)", t0.elapsed().as_secs_f64());
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            warm.stage("完成", 100);
            warm.set("ready", Value::Bool(true));
            tracing::info!(
                "Warmup [100%] complete - total {:.1}s",
                t0.elapsed().as_secs_f64()
            );
        }
        Err(e) => {
            warm.set("error", Value::String(e.clone()));
            warm.set("ready", Value::Bool(true));
            tracing::error!("Warmup error: {}", e);
        }
    }
}

/// Bind `host:port`, run the warmup in the background and serve until
/// shutdown (Ctrl-C).
pub async fn serve(state: Arc<AppState>, host: &str, port: u16) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    let router = super::app::build_router(state.clone());
    let warm_state = state.clone();
    tokio::spawn(async move { run_warmup(warm_state).await });
    let shutdown_state = state.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown_state.shutdown();
        })
        .await
}
