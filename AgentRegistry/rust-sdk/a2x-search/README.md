# a2x-search

Rust port of `AgentRegistry/a2x_registry/{a2x,vector,traditional}` from
[openJiuwen-ai/agent-protocol](https://github.com/openJiuwen-ai/agent-protocol):
the A2X hierarchical taxonomy **build** pipeline, the two-phase LLM-navigated
**search**, the **incremental** taxonomy updater, the **traditional**
(full-context, MCP style) and **vector** baselines, the **evaluators** with
error analysis, and the five CLI entry points.

Everything that talks to an LLM goes through `Arc<dyn a2x_common::LlmBackend>`,
so the registry backend can inject the shared `LlmClient` and tests can inject
the scripted `FakeLlm` shipped in `a2x_search::testing`.

> **Vector search substitution.** sentence-transformers and ChromaDB have no
> Rust equivalent. This crate replaces them with an `EmbeddingModel` trait
> (`OpenAiCompatibleEmbedding` over an HTTP `/embeddings` endpoint, plus the
> offline deterministic `HashingEmbedding`) and an in-memory cosine
> `VectorStore` persisted as one JSON file per collection. Collection naming,
> `stored_embedding_model`, `add / upsert / delete_ids / get_all_docs / clear`
> semantics and the `EMBEDDING_MODELS` table are preserved so the registry's
> `sync_vector`, `purge_dataset` and `GET /api/datasets/embedding-models`
> behave like the original. See [Vector search](#vector-search) below.

## Modules

| Module | Ports | Covers |
|--------|-------|--------|
| `taxonomy` | `common.models` (partial), file readers | `service.json`, `taxonomy.json`, `class.json`, `query.json` structs; `TreeIndex` (ancestors, LCA depth) |
| `search` | `a2x/search/*` | `A2xSearch` orchestrator, `CategoryNavigator` (Phase 1), `ServiceSelector` (Phase 2), prompts and `parse_selection`, streaming messages, `judge_relevance` (backend `POST /api/search/judge`) |
| `build` | `a2x/build/*` | `AutoHierarchicalConfig`, `TaxonomyBuilder` (BFS, checkpoints, resume), `NodeSplitter`, `CategoryDesigner`, `KeywordExtractor`, `CrossDomainAssigner`, prompts, `BuildSink` / `BuildEvent` progress |
| `incremental` | `a2x/incremental/incremental_builder.py` | `IncrementalBuilder` add / remove services in a built taxonomy |
| `traditional` | `traditional/search/traditional_search.py` | `TraditionalSearch` full-context baseline |
| `vector` | `vector/utils/*`, `vector/build/*`, `vector/search/*` | `EmbeddingModel`, `HashingEmbedding`, `OpenAiCompatibleEmbedding`, `embedding_models()`, `VectorStore`, `VectorIndexBuilder`, `VectorSearch`, ranking metrics |
| `evaluation` | `a2x/evaluation/*`, `traditional/evaluation/*`, `vector/evaluation/*` | `A2xEvaluator` (checkpointing), `TraditionalEvaluator`, `VectorEvaluator`, `error_analysis` |
| `testing` | (new) | `FakeLlm`: scripted `LlmBackend` for offline tests |
| `util` | `a2x/build/progress.py` (partial) | bounded task runner, JSON helpers, `llm_workers_from_env` |

## API mapping

| Python | Rust |
|--------|------|
| `a2x.search.A2XSearch(taxonomy_path, class_path, service_path, max_workers, parallel, mode)` | `A2xSearch::new(A2xSearchConfig, Arc<dyn LlmBackend>)` |
| `A2XSearch.search(query)` | `A2xSearch::search(&self, &str).await -> (Vec<SearchResult>, SearchStats)` |
| `A2XSearch.search(query, stream=True)` | `A2xSearch::search_streaming(self: &Arc<Self>, query) -> mpsc::UnboundedReceiver<StreamMessage>` |
| `A2XSearch._search_internal(query, step_callback)` | `A2xSearch::search_with_callback(&self, &str, Option<&StepCallback>)` |
| `search.models.SearchStats / NavigationStep / TerminalNode / ServiceGroup` | `search::{SearchStats, NavigationStep, TerminalNode, ServiceGroup}` |
| `search.prompts.build_category_prompt / build_service_prompt / parse_selection` | `search::prompts::{build_category_prompt, build_service_prompt, parse_selection}` |
| `search.navigator.CategoryNavigator` | `search::navigator::CategoryNavigator` |
| `search.selector.ServiceSelector` (`MIN_GROUP_SIZE = 30`) | `search::selector::ServiceSelector`, `selector::MIN_GROUP_SIZE` |
| `backend.services.search_service.SearchService.judge_services` | `search::judge_relevance(&dyn LlmBackend, &str, &[SearchResult]) -> Vec<(String, bool)>` |
| `build.config.AutoHierarchicalConfig` | `build::AutoHierarchicalConfig` (`new`, `build_params`, `matches_saved_config`, `save`, `dataset_name`) |
| `build.taxonomy_builder.TaxonomyBuilder(config, stop_event)` | `TaxonomyBuilder::new(config, llm)` + `build(resume, sink, cancel)` |
| `TaxonomyBuilder.build(resume="no"/"keyword"/"yes")` | `ResumeMode::{No, Keyword, Yes}` |
| `build.node_splitter.NodeSplitter` | `build::NodeSplitter` |
| `build.category_designer.CategoryDesigner` | `build::CategoryDesigner` |
| `build.keyword_extractor.KeywordExtractor` | `build::KeywordExtractor` |
| `build.cross_domain_assigner.CrossDomainAssigner` | `build::CrossDomainAssigner` |
| `build.prompts.*` templates, `ClassificationResult`, `NodeSplitResult` | `build::prompts::*` (same constant names), `build::{ClassificationResult, NodeSplitResult}` |
| `build.progress.progress_bar` + `logging` records | `BuildSink` emitting `BuildEvent::{Log, Progress, Phase}` |
| `threading.Event` stop event, `InterruptedError` | `tokio_util::sync::CancellationToken`, `Error::Cancelled` |
| `incremental.IncrementalBuilder` | `IncrementalBuilder` (`add_service`, `add_services_batch`, `remove_service`, `into_parts`) |
| `traditional.search.TraditionalSearch(service_path)` | `TraditionalSearch::new(&Path, llm)` / `from_services` |
| `vector.utils.embedding.EmbeddingModel(model_name)` | `vector::EmbeddingModel` trait; `default_embedding_model(name)` / `resolve_embedding_model(name, backend)` |
| `vector.utils.embedding_constants.{DEFAULT_EMBEDDING_MODEL, EMBEDDING_MODELS}` | `vector::{DEFAULT_EMBEDDING_MODEL, embedding_models()}` |
| `vector.utils.chroma_store.ChromaStore(collection, persist_dir, embedding_model)` | `VectorStore::open(collection, persist_dir, embedding_model)` |
| `vector.utils.metrics.*` | `vector::{precision_at_k, recall_at_k, hit_at_k, mrr, ndcg_at_k}` |
| `vector.build.index_builder.IndexBuilder` | `VectorIndexBuilder` |
| `vector.search.vector_search.VectorSearch` | `VectorSearch::new(VectorSearchConfig, Arc<dyn EmbeddingModel>).await` |
| `a2x.evaluation.a2x_evaluator.A2XEvaluator` | `A2xEvaluator` (`evaluate_single_query`, `evaluate_batch(&EvaluateOptions)`) |
| `a2x.evaluation.error_analysis.{generate_error_report, save_error_report}` | `evaluation::{generate_error_report, save_error_report}` |
| `traditional.evaluation.TraditionalEvaluator` | `TraditionalEvaluator` |
| `vector.evaluation.VectorEvaluator` | `VectorEvaluator` |
| `python -m a2x_registry.a2x.build` | `a2x-build` |
| `python -m a2x_registry.a2x.search` | `a2x-search` |
| `python -m a2x_registry.a2x.evaluation` | `a2x-evaluate-a2x` |
| `python -m a2x_registry.vector.evaluation` | `a2x-evaluate-vector` |
| `python -m a2x_registry.traditional.evaluation` | `a2x-evaluate-traditional` |
| `common.evaluation.compute_set_metrics`, `common.naming.generate_output_dir` | reused from `a2x_common` |

## Public API for the backend

```rust
use std::sync::Arc;
use a2x_search::*;

// Search
pub struct A2xSearchConfig { taxonomy_path, class_path, service_path: PathBuf, max_workers: usize, parallel: bool, mode: SearchMode }
impl A2xSearchConfig { fn for_dataset_dir(dir: &Path) -> Self; fn with_mode(self, SearchMode) -> Self; fn with_max_workers(self, usize) -> Self; fn with_parallel(self, bool) -> Self }
impl A2xSearch {
    pub fn new(config: A2xSearchConfig, llm: Arc<dyn LlmBackend>) -> Result<Self>;
    pub fn from_parts(config: A2xSearchConfig, taxonomy: TaxonomyFile, classes: ClassFile, services: ServicesIndex, llm: Arc<dyn LlmBackend>) -> Self;
    pub async fn search(&self, query: &str) -> (Vec<SearchResult>, SearchStats);
    pub async fn search_with_callback(&self, query: &str, on_step: Option<&StepCallback>) -> (Vec<SearchResult>, SearchStats);
    pub fn search_streaming(self: &Arc<Self>, query: impl Into<String>) -> tokio::sync::mpsc::UnboundedReceiver<StreamMessage>;
}
pub type StepCallback = dyn Fn(NavigationStep) + Send + Sync;
pub async fn judge_relevance(llm: &dyn LlmBackend, query: &str, services: &[SearchResult]) -> Vec<(String, bool)>;

// Build
impl TaxonomyBuilder {
    pub fn new(config: AutoHierarchicalConfig, llm: Arc<dyn LlmBackend>) -> Self;
    pub async fn build(&mut self, resume: ResumeMode, sink: BuildSink, cancel: CancellationToken) -> Result<BuildOutcome>;
    pub fn evaluate_resume(&self) -> ResumeAction;   // Skip | Resume | Rebuild
}
pub struct BuildOutcome { skipped: bool, nodes_split: usize, elapsed: Duration, output_dir: PathBuf, taxonomy: TaxonomyFile, class_data: ClassFile, summary: BuildSummary }
impl BuildSink { fn new(f: impl Fn(BuildEvent) + Send + Sync + 'static) -> Self; fn tracing() -> Self; fn stdout() -> Self; fn null() -> Self }
pub enum BuildEvent { Log { level: LogLevel, message: String }, Progress { done: usize, total: usize, message: String }, Phase { phase: BuildPhase, message: String } }
impl BuildEvent { fn message(&self) -> &str; fn timestamped(&self) -> String /* "HH:MM:SS  message" */ }
pub fn build::compute_service_hash<'a>(services: impl IntoIterator<Item = &'a ServiceRecord>) -> String;

// Incremental
impl IncrementalBuilder {
    pub fn new(taxonomy: TaxonomyFile, class_data: ClassFile, services_index: ServicesIndex, llm: Arc<dyn LlmBackend>, workers: usize) -> Self;
    pub async fn add_service(&self, service: ServiceRecord) -> Vec<String>;
    pub async fn add_services_batch(&self, services: Vec<ServiceRecord>) -> IndexMap<String, Vec<String>>;
    pub fn remove_service(&self, service_id: &str) -> bool;
    pub fn taxonomy(&self) -> TaxonomyFile; pub fn class_data(&self) -> ClassFile; pub fn services_index(&self) -> ServicesIndex;
    pub fn into_parts(self) -> (TaxonomyFile, ClassFile, ServicesIndex);
}

// Traditional
impl TraditionalSearch {
    pub fn new(service_path: &Path, llm: Arc<dyn LlmBackend>) -> Result<Self>;
    pub fn from_services(services: Vec<ServiceRecord>, llm: Arc<dyn LlmBackend>) -> Self;
    pub async fn search(&self, query: &str) -> (Vec<SearchResult>, TraditionalStats);
}

// Vector
#[async_trait] pub trait EmbeddingModel: Send + Sync { async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>; fn dim(&self) -> usize; fn name(&self) -> String; }
pub fn embedding_models() -> IndexMap<String, EmbeddingModelInfo { dim, language, description }>;
pub const DEFAULT_EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";
pub fn default_embedding_model(model_name: &str) -> Result<Arc<dyn EmbeddingModel>>;
pub fn resolve_embedding_model(model_name: &str, backend: EmbeddingBackend /* Auto | Hashing | OpenAi */) -> Result<Arc<dyn EmbeddingModel>>;
impl VectorStore {
    pub fn open(collection_name: &str, persist_dir: &Path, embedding_model: Option<&str>) -> Result<Self>;
    pub fn delete_collection(collection_name: &str, persist_dir: &Path) -> Result<bool>;
    pub fn stored_embedding_model(&self) -> Option<&str>;
    pub fn add(&mut self, ids: &[String], texts: &[String], embeddings: &[Vec<f32>]) -> Result<()>;
    pub fn upsert(&mut self, ids: &[String], texts: &[String], embeddings: &[Vec<f32>]) -> Result<()>;
    pub fn delete_ids(&mut self, ids: &[String]) -> Result<()>;
    pub fn query(&self, embedding: &[f32], top_k: usize) -> Vec<QueryHit { id, text, distance }>;
    pub fn count(&self) -> usize; pub fn get_all_docs(&self) -> IndexMap<String, String>; pub fn clear(&mut self) -> Result<()>;
}
pub fn collection_name_for_dataset(dataset: &str) -> String;   // lower-case, '-' -> '_'
impl VectorIndexBuilder { pub fn new(collection_name: &str, persist_dir: &Path, model_name: &str) -> Self; pub async fn build(&self, service_path: &Path, force_rebuild: bool, model: &dyn EmbeddingModel) -> Result<VectorStore>; }
impl VectorSearch {
    pub async fn new(config: VectorSearchConfig, model: Arc<dyn EmbeddingModel>) -> Result<Self>;
    pub async fn search(&self, query: &str, top_k: usize) -> Result<(Vec<SearchResult>, VectorStats)>;
}

// Evaluation
impl A2xEvaluator { pub fn new(config: A2xSearchConfig, llm: Arc<dyn LlmBackend>) -> Result<Self>; pub async fn evaluate_batch(&self, opts: &EvaluateOptions) -> Result<(Vec<A2xQueryMetrics>, A2xOverallMetrics)>; }
impl TraditionalEvaluator { pub fn new(service_path: &Path, max_workers: usize, llm: Arc<dyn LlmBackend>) -> Result<Self>; pub async fn evaluate_batch(&self, query_file: &Path, max_queries: Option<usize>, output_dir: Option<&Path>) -> Result<(Vec<TraditionalQueryMetrics>, TraditionalOverallMetrics)>; }
impl VectorEvaluator { pub async fn new(config: VectorSearchConfig, top_k: usize, top_k_list: Option<Vec<usize>>, model: Arc<dyn EmbeddingModel>) -> Result<Self>; pub async fn evaluate_batch(&self, query_file: &Path, max_queries: Option<usize>, output_dir: Option<&Path>) -> Result<(Vec<VectorQueryMetrics>, VectorOverallMetrics)>; }
pub fn evaluation::save_error_report(results_dir: &Path) -> Result<PathBuf>;

// Misc
pub fn llm_workers_from_env(default: usize) -> usize;   // A2X_REGISTRY_LLM_WORKERS
```

`SearchMode`, `ResumeMode` and `EmbeddingBackend` implement `FromStr` with
the Python string values (`get_all`, `get_one`, `get_important`; `no`,
`keyword`, `yes`; `auto`, `hashing`, `openai`).

### Streaming search over WebSocket

`search_streaming` spawns the search on a tokio task and yields
`StreamMessage`s that serialize exactly like the Python generator:

```json
{"type": "step", "parent_id": "root", "selected": ["cat_travel"], "pruned": ["cat_food"]}
{"type": "step", "parent_id": "__phase2__", "selected": [], "pruned": []}
{"type": "result", "results": [{"id": "...", "name": "...", "description": "..."}],
 "stats": {"llm_calls": 3, "total_tokens": 812, "visited_categories": 2, "pruned_categories": 3}}
```

`__phase2__` marks the start of service selection and `__fallback__` the
`get_one` fallback, as in Python.

### Streaming build logs over SSE

The Python backend captured `logging` records with a
`"%(asctime)s  %(message)s"` formatter. Every Python log line is emitted here
as `BuildEvent::Log`, every progress bar as `BuildEvent::Progress` whose
`message` is the exact bar text Python logged (for example
`██████░░░░░░░░░░░░░░░░░░░░░░░░  23.1% [425/1839] assigned`), and phase
changes as `BuildEvent::Phase`. `BuildEvent::timestamped()` produces the
`HH:MM:SS  message` line the SSE stream carried. Install a sink with
`BuildSink::new(move |event| tx.send(event.timestamped()))`.

Cancellation: pass a `CancellationToken`; `build` returns
`Err(Error::Cancelled)` at the next checkpoint (`err.is_cancelled()`).

## Quick start

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

Offline tests use `FakeLlm`:

```rust
use a2x_search::testing::FakeLlm;
let llm = FakeLlm::new()
    .on("Categories:", "1")                 // substring rule
    .on_fn(|prompt| prompt.contains("Services:").then(|| "1,2".to_string()))
    .fail_on("boom", "provider down");
```

## CLI

All binaries read the LLM configuration from `llm_apikey.json` (see
`a2x-common`) and honour `A2X_REGISTRY_HOME`. Flags mirror the Python argparse
entry points.

```bash
# Build (output-dir defaults to database/{dataset}/taxonomy)
a2x-build --service-path database/ToolRet_clean/service.json
a2x-build --service-path database/ToolRet_clean/service.json --resume yes      # smart resume
a2x-build --service-path database/ToolRet_clean/service.json --resume keyword  # reuse keywords.json
#   --output-dir, --keyword-batch-size 50, --keyword-threshold 500, --max-service-size 40,
#   --max-categories-size 20, --generic-ratio 0.333, --delete-threshold 2, --max-depth 3 (0 = unlimited),
#   --workers 20, --max-refine-iterations 3, --no-cross-domain
#   Ctrl-C cancels at the next checkpoint; the build can be resumed with --resume yes.

# Search (default dataset ToolRet_clean under the registry home)
a2x-search --query "I need to book a flight" --mode get_important --max-workers 20 --verbose
#   --max-workers defaults to A2X_REGISTRY_LLM_WORKERS when set, else 20; --parallel=false for sequential

# Evaluate
a2x-evaluate-a2x --data-dir database/ToolRet_clean --max-queries 50 --mode get_all
a2x-evaluate-a2x --data-dir database/publicMCP --query-file database/publicMCP/query/query_cn.json \
    --service-path database/publicMCP/service.json --mode get_one --workers 10 --notes "..."
a2x-evaluate-traditional --service-path database/publicMCP/service.json --query-file database/publicMCP/query/query_cn.json
a2x-evaluate-vector --max-queries 50 --top-k 10 --top-k-list 5,10 --embedding-backend hashing
#   --model-name is read from database/{ds}/vector_config.json when omitted; --force-rebuild;
#   --embedding-backend auto|hashing|openai (env A2X_REGISTRY_EMBEDDING_BACKEND); --persist-dir database/chroma
```

Output directories default to `results/{date}_{method}[-{mode}]_{dataset}[-{suffix}]_{count}`
through `a2x_common::generate_output_dir`. Evaluation logs go to stderr at
info level (`RUST_LOG` overrides).

## File formats

All files are UTF-8 JSON with 2-space indentation, written atomically.

- **`service.json`**: array of `{"id", "name", "description", ...}`. Extra keys
  such as `inputSchema` are preserved in `ServiceRecord::extra`.
- **`taxonomy.json`**: `{"version": "2.0-hierarchical", "root": "root",
  "categories": {id: {"children": [...], "services": [...]}}, "build_status": "bfs"|"cross_domain"|"complete"}`.
  Children and services lists are sorted like the Python builder writes them.
- **`class.json`**: `{"version": "2.0-hierarchical", "categories": {id: {"name", "description", "boundary"?, "decision_rule"?}}}`.
  Root is `All API Services`.
- **`keywords.json`**: `{keyword: count}` sorted by count, root only, kept by
  `--resume keyword|yes`, deleted by `--resume no`.
- **`build_config.json`**: every `AutoHierarchicalConfig` field plus
  `service_hash` (SHA256 of sorted `(name, description)` pairs encoded as
  Python `json.dumps(..., ensure_ascii=False)`; byte-compatible with the
  registry's `_compute_build_hash`). `matches_saved_config` ignores
  `output_dir`, `workers` and `service_hash`, compares `service_path` by
  dataset name and floats with a `1e-3` tolerance.
- **`assignments.json`**: `{service_id: {"category_ids": [...], "reasoning": "..."}}`.
- **`query.json`**: array of `{"id", "query", "correct_tools": [{"id", ...}]}`.
- **Evaluation output** (`config.json`, `evaluation_results.json`,
  `summary.json`, `partial_results.jsonl` with `_index`, `error_analysis.json`,
  `error_analysis.md`): same keys as the Python evaluators, including the
  Chinese headings of the markdown report.
- **Vector collection** (`<persist_dir>/<collection>.json`, new):
  `{"collection", "embedding_model", "docs": [{"id", "text", "embedding"}]}`.

## Vector search

`resolve_embedding_model(name, backend)` replaces `EmbeddingModel(name)`:

- `EmbeddingBackend::OpenAi` / `Auto`: reads the `"embedding"` object of
  `llm_apikey.json` (`base_url` of an OpenAI-compatible `/embeddings`
  endpoint, `model`, `api_keys` or `api_key`, optional `dim`, `batch_size`).
  Vectors are L2-normalised like `normalize_embeddings=True`.
- `EmbeddingBackend::Hashing`: `HashingEmbedding`, deterministic
  feature-hashing over lowercase word unigrams and bigrams (each CJK character
  is a token). No network or model files. Suitable for tests, demos and CI
  only; retrieval quality is far below a real model.
- `Auto` without an `embedding` section returns
  `A2xError::VectorSearchUnavailable` with setup instructions, like the
  Python `VectorSearchUnavailableError`.

`VectorStore::open` is `get_or_create_collection`: an existing file keeps its
recorded `embedding_model`; `clear()` drops the documents and the recorded
model so that the next `open` records the new one (the flow `sync_vector`
relies on). `query` returns cosine distance (`1 - cos`) like Chroma's cosine
space. The store is fully in memory; every mutation rewrites the file.

## Deviations from the original

- Vector stack substitution (above).
- `build` also checks the cancellation token between BFS node splits, not only
  at phase boundaries. State on disk is consistent at those points (a
  checkpoint is written after every split), so `--resume yes` continues
  correctly. Python only checked at phase boundaries.
- `SearchMode` is passed through the pipeline instead of mutating the searcher
  during the `get_one` fallback, so one `A2xSearch` can serve concurrent
  requests.
- `A2xSearch::search` never returns an error, matching Python where every LLM
  failure is logged and treated as an empty selection. Failed calls are still
  counted in `llm_calls`, as in Python.
- `tqdm` progress bars in evaluators are replaced by a single in-place stderr
  line. Build progress bars keep the exact Python text.
- `AutoHierarchicalConfig::max_depth` is `Option<u32>` (`None` = unlimited,
  Python `None`); `a2x-build --max-depth 0` maps to unlimited.
- `a2x-evaluate-vector` adds `--embedding-backend` and `--persist-dir`.
- Assignments are inserted in completion order (like Python's
  `as_completed`), so `assignments.json` key order varies between runs.
- Vector store files replace `database/chroma/` SQLite; the directory name is
  kept as the default `persist_dir`.

## Intentionally left out

- sentence-transformers model loading, HuggingFace cache lookup and the
  `HF_ENDPOINT` mirror hints; ChromaDB itself.
- `vector/build/index_builder.py`'s `__main__` block (index building happens
  through `VectorSearch` / `a2x-evaluate-vector`, as in the backend).
- `error_analysis.py`'s `__main__` "latest results directory" helper; call
  `save_error_report(dir)` instead.
- The `a2x/utils/llm_client.py` re-export shim (it only re-exported
  `common.llm_client`, which lives in `a2x-common`).
- `tests/query/*` in the original test the FastAPI app (lite/full gating,
  HTTP 503 bodies). Those belong to the `a2x-registry` crate; the parts that
  apply here (embedding constants table, judge behaviour) are covered by this
  crate's tests.

## Tests

```bash
CARGO_TARGET_DIR=target-search cargo test -p a2x-search
```

Unit tests live next to the code; `tests/` holds offline integration tests
driven by `FakeLlm`: navigation and selection in all three modes, streaming,
LCA group merging, a full synthetic taxonomy build (description-based and
keyword-based roots, root validation and redesign, refinement, tiny-category
deletion, cross-domain linking), cancellation, all three resume modes,
config-change detection, incremental add/remove, traditional search with the
regex fallback, vector search with the hashing embedding, store sync
semantics, and every evaluator's output files including the error report.
