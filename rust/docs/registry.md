# Registry, search and cluster

The A2X agent registry is four crates that fit together:

| Crate | Role |
|-------|------|
| `a2x-common` | Registry entry models, home and database path resolution, lease table, multi-provider LLM client, feature flags |
| `a2x-registry` | The backend: registration store, authentication, heartbeat leases, HTTP API, `a2x-registry` and `a2x-register` binaries |
| `a2x-search` | Taxonomy build, LLM-navigated hierarchical search, vector and traditional baselines, evaluation binaries |
| `a2x-cluster` | Replication between registry nodes: gossip, last-writer-wins envelopes, Merkle anti-entropy, membership |
| `a2x-registry-client` | Client SDK (async and blocking) and the `a2x-registry-client` binary |

## Running the backend

```bash
a2x-registry --host 0.0.0.0 --port 8080          # default http://127.0.0.1:8000
a2x-registry auth init                            # prints the admin token once on stderr
a2x-registry auth reset-admin --confirm
a2x-registry cluster init --node-id A             # feature `cluster`, on by default
a2x-registry cluster set add http://10.0.0.2:8000 --token T
a2x-registry cluster status
```

The data directory is `A2X_REGISTRY_HOME`, else the current directory when it contains
`database/`, else `~/.a2x_registry`; it holds `database/` and `auth_data/`. Searching and building need an LLM configuration in `llm_apikey.json`
and, for vector search, an embedding backend (`A2X_REGISTRY_EMBEDDING_BACKEND`). The React UI
from the original repository is served unchanged when `A2X_FRONTEND_DIST_DIR` points at its build.

Embedding the backend in your own program:

```rust,no_run
use a2x_registry::backend::startup::serve;
use a2x_registry::{AppConfig, AppState};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cfg = AppConfig::from_env();
    #[cfg(feature = "search")]
    let cfg = a2x_registry::backend::adapters::search::configure(cfg);
    serve(AppState::new(cfg), "127.0.0.1", 8000).await
}
```

`AppConfig::search_engine` and `AppConfig::build_engine` are pluggable; without the `search`
feature they answer with a structured 503. The cluster module starts during warmup when
`cluster_state.json` exists.

Library use without the server:

```rust,no_run
use a2x_registry::register::{RegisterGenericRequest, RegistryService};

# async fn demo() -> Result<(), a2x_registry::register::RegistryError> {
let registry = RegistryService::new("database", None);
registry.startup().await?;
registry.register_generic(&RegisterGenericRequest::new("default", "Calculator", "adds"), None)?;
# Ok(()) }
```

## Managing data offline

`a2x-register` operates on the database directory directly, without a running server, with the
same subcommands as the Python `python -m a2x_registry.register`:

```bash
a2x-register datasets
a2x-register create-dataset myDS --formats generic,a2a:v1.0
a2x-register register-generic myDS --name "API" --desc "..." --url https://api.example
a2x-register register-a2a myDS --url https://agent.example/.well-known/agent-card.json
a2x-register register-skill myDS skill.zip
a2x-register list myDS --mode admin
a2x-register update myDS generic_xxx --set status=offline
a2x-register deregister myDS generic_xxx
a2x-register delete-dataset old --confirm
```

`--json` prints machine-readable output; `--database-dir` and `--config` override the defaults.

## HTTP API

The backend serves the same routes as the Python backend: datasets, services, skills,
registration formats, heartbeat leases, reservations, authentication (`/api/auth/*`), search
(`/api/search`, WebSocket streaming), taxonomy builds (`/api/datasets/{ds}/build`, SSE logs) and
the cluster routes under `/api/cluster/*`. The route table with request and response shapes is in
the `a2x-registry` README under "HTTP endpoints".

## Search and taxonomy build

```rust
use std::sync::Arc;
use a2x_common::llm_client::LlmClientOptions;
use a2x_search::*;
use tokio_util::sync::CancellationToken;

# async fn run() -> a2x_search::Result<()> {
let llm: Arc<dyn LlmBackend> = Arc::new(LlmClient::new(None, LlmClientOptions::default())?);

// Build database/MyDS/taxonomy/{taxonomy,class,build_config}.json
let config = AutoHierarchicalConfig::new("database/MyDS/service.json");
let mut builder = TaxonomyBuilder::new(config, llm.clone());
let outcome = builder.build(ResumeMode::Yes, BuildSink::stdout(), CancellationToken::new()).await?;
println!("split {} nodes, {} categories", outcome.nodes_split, outcome.summary.total_categories);

// Search
let cfg = A2xSearchConfig::for_dataset_dir(std::path::Path::new("database/MyDS")).with_mode(SearchMode::GetImportant);
let searcher = A2xSearch::new(cfg, llm.clone())?;
let (results, stats) = searcher.search("book a flight to Tokyo").await;
println!("{} results after {} LLM calls", results.len(), stats.llm_calls);
# Ok(()) }
```

The binaries wrap the same API:

```bash
a2x-build --service-path database/ToolRet_clean/service.json [--resume yes|keyword|no]
a2x-search --query "I need to book a flight" --mode get_important --max-workers 20
a2x-evaluate-a2x --data-dir database/ToolRet_clean --max-queries 50 --mode get_all
a2x-evaluate-traditional --service-path database/publicMCP/service.json --query-file database/publicMCP/query/query_cn.json
a2x-evaluate-vector --max-queries 50 --top-k 10 --embedding-backend hashing
```

Vector search uses an `EmbeddingModel` trait with a hashing backend (offline, deterministic) and
an OpenAI-compatible HTTP backend, and a vector store persisted as JSON under
`database/chroma`. Tests script the LLM with `a2x_search::testing::FakeLlm`.

## Cluster replication

A node is a `ClusterStore` built from a `ClusterConfig`, a view of the local registry, a peer
authentication verifier and a transport. The backend does this during warmup; a standalone node
for experiments is the `a2x-cluster` binary:

```bash
a2x-cluster init --node-id A
A2X_REGISTRY_CLUSTER_ADVERTISE=http://127.0.0.1:8001 a2x-cluster serve --bind 127.0.0.1:8001
a2x-cluster set add http://127.0.0.1:8002 --server http://127.0.0.1:8001
a2x-cluster status --server http://127.0.0.1:8001
```

Membership is declarative (`set add`, `set remove`, `set show`): the node keeps a roster with
last-writer-wins records, gossips it to peers and reconciles connections. Records replicate as
envelopes with versions; periodic Merkle anti-entropy repairs missed updates. Configuration
knobs and their environment variables are listed in [Configuration](configuration.md).

## Client SDK

```rust,no_run
use a2x_registry_client::{A2xRegistryClient, ClientConfig, ListOptions, OwnershipFile, RegisterOptions};
use ap_support::json::object;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), a2x_registry_client::ClientError> {
    let client = A2xRegistryClient::new(
        ClientConfig::new().base_url("http://127.0.0.1:8000").ownership_file(OwnershipFile::Disabled),
    )?;
    let card = object(json!({
        "name": "EN-ZH Translator",
        "description": "Translate EN to simplified ZH.",
        "url": "https://translator-01.internal/a2a",
        "status": "online",
    }));
    let opts = RegisterOptions { lease_ttl: Some(60), auto_renew: true, ..Default::default() };
    let resp = client.register_agent("translators", &card, &opts).await?;
    let online = client.list_agents("translators", &ListOptions::new().filter("status", "online")).await?;
    println!("{} registered, {} online", resp.service_id, online.len());
    client.deregister_agent("translators", &resp.service_id).await?;
    client.close().await;
    Ok(())
}
```

Without an explicit base URL or API key, the client reads `~/.a2x_registry_client/cli_token.json`
written by `a2x-registry-client login`. Leases renew in the background while the client is open;
`OwnershipFile` records which services this process registered so a restart can clean up.
A blocking client (`a2x_registry_client::blocking`) wraps the same API for synchronous programs,
and `reserve_blank_agents_guarded` returns a guard that releases the reservation when dropped.

Errors are `ClientError` values with typed variants for HTTP status classes, validation, ownership
and connection failures; `is_validation()` and `is_connection()` classify them.

The `a2x-registry-client` binary handles login, logout, `whoami` and API keys; see
[Command-line tools](cli.md).
