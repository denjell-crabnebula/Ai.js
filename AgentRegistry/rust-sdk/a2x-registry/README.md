# a2x-registry

Rust port of `AgentRegistry/a2x_registry/{register,auth,heartbeat,backend}` from
[openJiuwen-ai/agent-protocol](https://github.com/openJiuwen-ai/agent-protocol),
plus the `a2x-registry` (backend server, auth and cluster admin) and
`a2x-register` (offline dataset and service management) command line tools.

The crate is the HTTP registry itself: dataset lifecycle, service registration
(generic, A2A agent card, skill folders), partial updates, reservation leases,
static API key auth with three roles and per namespace scoping, heartbeat
leases with a two stage sweeper, the taxonomy build job runner with its SSE log
stream, the search facade with the A2X WebSocket, LLM provider switching and
the startup warmup. Wire formats (paths, JSON bodies, headers, status codes)
and the files persisted under `database/` match the Python package.

## Modules

| Module | Ports | Covers |
|--------|-------|--------|
| `register::models` | `register/models.py` | `RegistryEntry`, `AgentCard` (extra fields preserved), `GenericServiceData`, `SkillData`, request and response models, `TaxonomyState`, `BuildRequest`; the three Pydantic dump projections (`Full`, `ExcludeNone`, `ExcludeDefaults`) |
| `register::validation` | `register/validation.py` | `FormatValidator` trait, `GenericValidator`, `SkillValidator`, `A2AValidator` (v0.0 and v1.0), `validate_service`, `normalize_format_config`, `validate_agent_card` shim |
| `register::store` | `register/store.py` | `RegistryStore`: `user_config.json`, `api_config.json` (omit-when-None `owner_id` / `lease_ttl`), `service.json`, `register_config.json`, `vector_config.json`, `auth_config.json`, `lease_config.json`, `skills/*/SKILL.md`, ZIP import and export, frontmatter rewrite, folder rename, `removed_skills/` |
| `register::service` | `register/service.py` | `RegistryService`: three phase startup, three source merge (`api_config > user_config > skill_folder`), register / update / deregister, reservation leases, taxonomy state tracking, dataset lifecycle, per namespace auth and lease config, `ServiceChangeListener`, `MutationHook`, `UnhealthyCheck` |
| `register::agent_card` | `register/agent_card.py` | `fetch_agent_card` (10 s timeout, `Accept` and `User-Agent` headers), `build_description` |
| `register::embedding` | `vector/utils/embedding_constants.py` | `DEFAULT_EMBEDDING_MODEL`, `EMBEDDING_MODELS` |
| `auth::{models,tokens,store}` | `auth/{models,tokens,store}.py` | `Principal`, `ApiKey`, `a2x_pat_` tokens, sha256 hashing, `AuthStore` (`bootstrap`, `load_or_none`, `authenticate`, principal and key CRUD, JSON lines audit log with Python separators) |
| `auth::extractors` | `auth/deps.py` | axum extractors `Authorize`, `RequirePrincipal`, `RequireAdmin`, `RequireAdminStrict`, `RequireAdminOrAnon` with the same 401 / 403 / 404 behaviour |
| `auth::router` | `auth/router.py` | `/api/auth/*` (404 for the whole group before `auth init`) |
| `auth::cli` | `auth/cli.py` | `a2x-registry auth init` and `reset-admin` with the stderr banner |
| `heartbeat::{store,sweeper,router,errors}` | `heartbeat/*` | `HeartbeatStore` on `a2x_common::LeaseTable`, four corner `validate`, `install`, `heartbeat`, `revoke`, `recover_from_persisted`; `HeartbeatSweeper` (tokio task, `sweep_once`); heartbeat endpoints; structured 400 bodies |
| `backend::app` | `backend/app.py` | Router assembly, CORS, `{"detail": ...}` 404 / 405 fallbacks, static front end from `A2X_FRONTEND_DIST_DIR`, `/api/warmup-status`, dynamic `/api/cluster/*` dispatch |
| `backend::startup` | `backend/startup.py` | `run_warmup` stages (registry, auth store, heartbeat recovery and sweeper, cluster, taxonomy caches, engine warmup), `serve` |
| `backend::routers::dataset` | `backend/routers/dataset.py` | Every `/api/datasets` endpoint including filters, pagination headers, reservations, skills, taxonomy, default queries, embedding models, vector / register / auth / lease config |
| `backend::routers::build` | `backend/routers/build.py` | Build trigger, status, cancel, SSE stream (`data:` events, `: keepalive`) |
| `backend::routers::search` | `backend/routers/search.py` | `POST /api/search`, `POST /api/search/judge`, WebSocket `/api/search/ws` |
| `backend::routers::provider` | `backend/routers/provider.py` | `GET /api/providers`, `POST /api/providers/{name}` |
| `backend::services::search_service` | `backend/services/search_service.py` | `SearchService` facade: taxonomy availability check, provider switching, LLM relevance judge, elapsed time |
| `backend::services::taxonomy_service` | `backend/services/taxonomy_service.py` | Cached D3 tree from `taxonomy.json` and `class.json` |
| `backend::default_queries` | `backend/default_queries.py` | `default_queries.json` with `$ref` redirect |
| `backend::build_jobs` | module globals in `routers/build.py` | Job bookkeeping keyed by dataset, cancellation tokens, SSE subscribers |
| `backend::engines` | (new) | `SearchEngine` and `BuildEngine` traits, `UnavailableEngine` (structured 503) |
| `backend::adapters::search` | `backend/services/search_service.py` engine cache, `sync_vector`, `purge_dataset` | `A2xEngine` over the `a2x-search` crate (feature `search`) |
| `backend::adapters::cluster` | `backend/startup.py` cluster wiring, `routers/dataset.py` foreign merge | `LocalRegistryView`, `PeerAuthVerifier`, replication hook, `/api/cluster/*` mounting, CLI (feature `cluster`) |
| `backend::workers` | `A2X_REGISTRY_*_WORKERS` guards | Semaphores with the documented defaults and warning-and-fallback |
| `util` | `json.dumps` layout, `str()` / `int()` / `bool()` | Python compatible JSON rendering for hashes and audit lines, value coercions used by filters and config loaders |

## API mapping

| Python | Rust |
|--------|------|
| `RegistryService(database_dir, global_config_path, allowed_a2a_versions)` | `RegistryService::new(database_dir, global_config_path)` + `with_allowed_a2a_versions` + `with_agent_card_workers` |
| `RegistryService.startup()` | `async fn startup() -> Result<BTreeMap<String, TaxonomyState>>` |
| `register_generic(req, caller)` | `register_generic(&RegisterGenericRequest, Option<&AuthContext>)` |
| `register_a2a(req, caller)` | `async fn register_a2a(...)` (fetches `agent_card_url`) / `register_a2a_resolved(req, card, url, caller)` |
| `register_batch(entries, dataset, persistent)` | `register_batch(&[RegistryEntry], &str, bool)` |
| `register_skill(dataset, zip_bytes, caller)` | `register_skill(&str, &[u8], Option<&AuthContext>)` |
| `deregister_skill` / `get_skill_zip` | same names |
| `update_service(dataset, sid, updates, caller)` | `update_service(&str, &str, &Map<String, Value>, Option<&AuthContext>)` |
| `deregister(dataset, sid, caller)` | `deregister(...)` |
| `list_services` / `list_entries` / `get_entry` / `get_status` / `list_datasets` / `list_datasets_with_counts` / `dataset_exists` | same names |
| `check_taxonomy_state` / `get_taxonomy_state` | same names |
| `reserve_services(dataset, filters, n, ttl, holder_id, caller)` | `reserve_services(...)` and `reserve_services_at(ReserveRequest, caller, now_mono, now_wall)` |
| `release_reservation` / `release_lease_by_sid` / `extend_reservation` / `is_leased` | same names (`extend_reservation_at` takes explicit clocks) |
| `create_dataset(name, embedding_model, formats, auth_required)` / `delete_dataset` | same names |
| `get_register_config` / `set_register_config` / `get_vector_config` / `set_vector_config` | same names |
| `is_auth_required` / `set_auth_config` / `get_lease_config` / `set_lease_config` | same names (`LeaseConfig` struct) |
| `set_on_service_changed(cb)` / `set_on_mutation(cb)` / `set_unhealthy_check(cb)` | `set_on_service_changed(Option<Arc<dyn ServiceChangeListener>>)` / `set_on_mutation(Option<Arc<dyn MutationHook>>)` / `set_unhealthy_check(Option<Arc<dyn UnhealthyCheck>>)`; closures implement the listener and check traits |
| `RegistryNotFoundError`, `ValueError`, `PermissionError`, `FileNotFoundError` | `RegistryError::{NotFound, Invalid, Permission, FileNotFound}` (404 / 400 / 403 / 404) |
| `RegistryStore(dataset_dir)` and its methods | `RegistryStore::new(dir)` with the same method names (`drop` is `drop_lease` on the heartbeat store) |
| `generate_service_id`, `parse_skill_md_content`, `_entry_to_config_dict` | `generate_service_id`, `parse_skill_md_content`, `entry_to_config_dict` |
| `validate_service`, `normalize_format_config`, `validate_agent_card`, `DEFAULT_FORMAT_CONFIG`, `SUPPORTED_SERVICE_TYPES` | same names in `register::validation` |
| `AuthStore.bootstrap(data_dir, admin_token, admin_handle)` | `AuthStore::bootstrap(Option<&Path>, Option<&str>, &str) -> (AuthStore, String)` |
| `AuthStore.load_or_none(data_dir)` | `AuthStore::load_or_none(Option<&Path>) -> Result<Option<AuthStore>>` |
| `authenticate`, `create_principal`, `list_principals`, `get_principal`, `update_principal`, `create_key`, `list_keys`, `revoke_key`, `audit` | same names (`update_principal` takes `NamespacesUpdate::{Unset, Set(..)}` for the sentinel) |
| `tokens.generate_token` / `hash_token` / `token_prefix` / `TOKEN_PREFIX` | same names |
| `deps.authorize` / `require_principal` / `require_admin` / `require_admin_strict` / `require_admin_or_anon` | extractors `Authorize` / `RequirePrincipal` / `RequireAdmin` / `RequireAdminStrict` / `RequireAdminOrAnon` |
| `get_auth_store` / `set_auth_store` | `AppState::auth_store()` / `set_auth_store()` |
| `HeartbeatStore(config_provider)` | `HeartbeatStore::new(Arc<dyn LeaseConfigProvider>)` (`RegistryService` and closures implement it) |
| `validate`, `install`, `grant`, `heartbeat`, `revoke`, `drop`, `is_unhealthy`, `get_lease`, `list_leases`, `sweep_tick`, `recover_from_persisted` | same names (`drop` is `drop_lease`; `heartbeat` returns `Option`) |
| `HeartbeatSweeper(registry, store, period)` / `start` / `stop` / `sweep_once` | `HeartbeatSweeper::new(Arc<dyn HardDeleter>, Arc<HeartbeatStore>, Duration)` / same names / `sweep_once_at(now)` |
| `SYSTEM_CTX` | `heartbeat::system_ctx()` |
| `HeartbeatNotSupportedError` / `TTLRequiredError` / `TTLOutOfRangeError` | `HeartbeatError` with `HeartbeatErrorCode::{NotSupported, TtlRequired, TtlOutOfRange}` |
| `get_heartbeat_store` / `set_heartbeat_store` | `AppState::heartbeat_store()` / `set_heartbeat_store()` |
| `backend.app.app` | `build_router(Arc<AppState>) -> axum::Router` |
| `backend.startup.run_warmup` / `warmup_state` | `startup::run_warmup(Arc<AppState>)` / `AppState::warmup` (`WarmupState`) |
| `search_service` singleton | `AppState::search` (`SearchService`) |
| `get_taxonomy_tree` | `TaxonomyService::get_taxonomy_tree` |
| `get_default_queries` | `default_queries::get_default_queries(home, database_dir, dataset)` |
| `python -m a2x_registry.backend` | `a2x-registry` |
| `python -m a2x_registry.register` | `a2x-register` |

## HTTP endpoints

All bodies, query parameters, headers and status codes follow
`docs/backend_api.md`, `docs/auth_design.md` and `docs/heartbeat_design.md` of
the original repository. Errors render as `{"detail": ...}`; 401 responses
carry `WWW-Authenticate: Bearer`.

| Method | Path | Notes |
|--------|------|-------|
| GET | `/api/warmup-status` | `{ready, stage, progress, error}` (keys starting with `_` are hidden) |
| GET | `/api/datasets` | `[{name, service_count, query_count}]` |
| POST | `/api/datasets` | `{name, embedding_model?, formats?, auth_required?, lease_config?}`; 409 `auth_not_initialized`, 401 / 403 for `auth_required=true` |
| DELETE | `/api/datasets/{ds}` | admin-or-anon |
| GET / POST | `/api/datasets/{ds}/auth-config` | POST is strict admin (404 before bootstrap) |
| GET / POST | `/api/datasets/{ds}/lease-config` | POST is admin-or-anon; bounds validated (400) |
| GET / POST | `/api/datasets/{ds}/register-config` | POST is admin-or-anon; empty normalized formats is 400 |
| GET | `/api/datasets/{ds}/services` | `fields=brief|detail`, `size`, `page`, `include_leased`, `include_unhealthy`, any other key is an AND filter with the `status=online` default-online rule; headers `X-Total-Count`, `X-Page`, `X-Total-Pages`, `X-Page-Size` when `size > 0` |
| GET | `/api/datasets/{ds}/services/{sid}` | wrapped entry; skills return `application/zip`; namespaced ids resolve to cluster replicas |
| POST | `/api/datasets/{ds}/services/generic` | `{name, description, service_id?, url?, inputSchema?, persistent?, lease_ttl?}` -> `{service_id, dataset, status, lease_ttl, lease_expires_at}` |
| POST | `/api/datasets/{ds}/services/a2a` | `{agent_card | agent_card_url, service_id?, persistent?, lease_ttl?}` |
| PUT | `/api/datasets/{ds}/services/{sid}` | top level upsert; `owner_id`, `service_id`, `type`, `source` are stripped |
| DELETE | `/api/datasets/{ds}/services/{sid}` | `{service_id, status: "deregistered"}`; 400 for `user_config` / `skill_folder` sources |
| DELETE | `/api/datasets/{ds}/services/{sid}/lease` | teammate-self release `{released, prev_holder_id}` |
| POST | `/api/datasets/{ds}/reservations` | `{filters, n, ttl_seconds, holder_id?}`; holder forced to the caller on auth namespaces |
| DELETE | `/api/datasets/{ds}/reservations/{holder}` | bulk release `{released: [...]}` |
| DELETE | `/api/datasets/{ds}/reservations/{holder}/{sid}` | per sid release; 403 when held by another holder |
| POST | `/api/datasets/{ds}/reservations/{holder}/extend` | `{ttl_seconds}` -> `{expires_at_unix}`; 404 without live leases |
| POST | `/api/datasets/{ds}/services/{sid}/heartbeat` | `{status?}` -> `{service_id, dataset, state, ttl_seconds, expires_at}`; 404 without lease |
| DELETE | `/api/datasets/{ds}/services/{sid}/heartbeat` | `{permanent?}` soft revoke or hard delete |
| POST | `/api/datasets/{ds}/skills` | multipart field `file` (ZIP with `SKILL.md`) |
| DELETE | `/api/datasets/{ds}/skills/{name}` | `{name, dataset, service_id, status: deleted|not_found}` |
| GET | `/api/datasets/{ds}/skills/{name}/download` | ZIP |
| GET | `/api/datasets/{ds}/taxonomy` | D3 tree; 404 when not built |
| GET | `/api/datasets/{ds}/default-queries` | `{source, queries: [{query, query_en}]}` |
| GET | `/api/datasets/embedding-models` | `{models: {...}}` |
| GET / POST | `/api/datasets/{ds}/vector-config` | POST is admin-or-anon and schedules a vector sync |
| POST | `/api/datasets/{ds}/build` | `BuildRequest`; 409 while running |
| GET | `/api/datasets/{ds}/build/status` | `{dataset, status, message, started_at, finished_at, logs}` or `{dataset, status: "idle"}` |
| DELETE | `/api/datasets/{ds}/build` | cancel; 409 without a running build |
| GET | `/api/datasets/{ds}/build/stream` | SSE: replay of logs, live `log` and `status` events, `: keepalive` comments |
| POST | `/api/search` | `{query, method, dataset?, top_k?}` -> `{results, stats, elapsed_time}`; 503 `{feature, extras, detail}` when a feature is missing |
| POST | `/api/search/judge` | `{query, services}` -> `{results: [{service_id, relevant}]}`; 503 `{reason: "llm_not_configured", detail}` without `llm_apikey.json` |
| WS | `/api/search/ws` | `{type: "step", data}` then `{type: "result", data}`, or `{type: "error", message}` |
| GET | `/api/providers` | `{providers: [{name, model}], current}` |
| POST | `/api/providers/{name}` | reorders `llm_apikey.json`; `{status: "ok", current}` or `{error, valid}` |
| GET | `/api/auth/whoami` | any authenticated principal |
| GET / POST | `/api/auth/principals` | admin; POST returns 201 with the plaintext `token` once |
| GET / PATCH | `/api/auth/principals/{id}` | admin |
| GET / POST | `/api/auth/keys` | own keys (admin sees all, `?principal_id=`); POST returns 201 with the plaintext `token` once |
| DELETE | `/api/auth/keys/{key_id}` | own key or admin |
| * | `/api/cluster/*` | served by the `a2x-cluster` router when `cluster_state.json` exists, otherwise 404 with the dormant message |

## CLI

```bash
# Backend server (default http://127.0.0.1:8000)
a2x-registry --host 0.0.0.0 --port 8080

# Auth bootstrap (prints the admin token once on stderr)
a2x-registry auth init [--handle root] [--admin-token a2x_pat_...] [--data-dir DIR]
a2x-registry auth reset-admin --confirm

# Cluster administration (feature `cluster`, on by default)
a2x-registry cluster init [--node-id ID]
a2x-registry cluster status [--server URL]
a2x-registry cluster set add http://10.0.0.2:8000 [--token T]
a2x-registry cluster set remove <node_id>
a2x-registry cluster set show

# Offline dataset and service management (same subcommands as
# `python -m a2x_registry.register`)
a2x-register [--database-dir DIR] [--config FILE] [--json] [-v] status [--dataset DS]
a2x-register datasets
a2x-register create-dataset myDS [--embedding-model M] [--formats generic,a2a:v1.0]
a2x-register list myDS [--mode browse|admin]
a2x-register get myDS generic_xxx
a2x-register register-generic myDS --name "API" --desc "..." [--url U] [--input-schema FILE] [--service-id ID]
a2x-register register-a2a myDS (--url https://... | --card-file card.json)
a2x-register register-skill myDS skill.zip
a2x-register update myDS generic_xxx [--json FILE] [--set k=v ...] [--name N] [--desc D] [--url U] [--license L]
a2x-register deregister myDS generic_xxx
a2x-register deregister-skill myDS my-skill
a2x-register get-register-config myDS
a2x-register set-register-config myDS --formats a2a:v1.0
a2x-register delete-dataset old --confirm
a2x-register --status            # legacy form
```

Environment variables follow `docs/environment.md`: `A2X_REGISTRY_HOME`,
`A2X_REGISTRY_AUTH_DATA`, `A2X_REGISTRY_CLUSTER_*`,
`A2X_REGISTRY_SEARCH_WORKERS` (4), `A2X_REGISTRY_DATASET_WORKERS` (2),
`A2X_REGISTRY_LLM_WORKERS` (20), `A2X_REGISTRY_AGENT_CARD_WORKERS` (10),
`A2X_FRONTEND_DIST_DIR`, `A2X_REGISTRY_DISABLED_FEATURES` (from `a2x-common`)
and `A2X_REGISTRY_EMBEDDING_BACKEND` (`auto`, `hashing`, `openai`; from the
search adapter).

## Quick start

```rust,no_run
use std::sync::Arc;
use a2x_registry::backend::startup::{run_warmup, serve};
use a2x_registry::register::{RegisterGenericRequest, RegistryService};
use a2x_registry::{build_router, AppConfig, AppState};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Library use: the registry alone.
    let registry = RegistryService::new("database", None);
    registry.startup().await.expect("startup");
    registry
        .register_generic(&RegisterGenericRequest::new("default", "Calculator", "adds"), None)
        .expect("register");

    // Server use: state from the environment, real engines when compiled in.
    let cfg = AppConfig::from_env();
    #[cfg(feature = "search")]
    let cfg = a2x_registry::backend::adapters::search::configure(cfg);
    let state = AppState::new(cfg);
    // `serve` runs the warmup in the background; for tests call
    // `run_warmup(state.clone()).await` and `build_router(state)` directly.
    let _ = (run_warmup, build_router);
    serve(state, "127.0.0.1", 8000).await
}
```

Engines are pluggable: `AppConfig::search_engine` and `AppConfig::build_engine`
default to `UnavailableEngine`, which answers with the structured 503 body.
`backend::adapters::search::configure` installs `A2xEngine` from the
`a2x-search` crate. The cluster module initializes itself during warmup when
`cluster_state.json` exists (`backend::adapters::cluster::init`).

## Tests

```bash
CARGO_TARGET_DIR=target-registry cargo test -p a2x-registry
```

Unit tests live next to the code (models, validation, store, service, auth
store and CLI, heartbeat store and sweeper, build jobs and SSE, search
service, adapters). `tests/` ports the intent of every original suite:
`registered`, `deregistered`, `update`, `reserve`, `query` (including the
WebSocket error hint over a real socket), `auth` (all fifteen files), `heartbeat`
(all eleven files, driven through `sweep_tick(now)` instead of sleeping) and
`backend` (front end contract, providers, warmup status, skills). Tests run
offline against a fresh temporary home per test.

## Deviations from the original

- The FastAPI dependency layer became axum extractors; module level
  singletons (`get_auth_store`, `get_heartbeat_store`) live on `AppState`.
- Blocking registry calls run on `spawn_blocking` behind a semaphore sized by
  `A2X_REGISTRY_DATASET_WORKERS`; `/api/search` uses a semaphore sized by
  `A2X_REGISTRY_SEARCH_WORKERS`. The sweeper and build jobs are tokio tasks.
- The SSE stream subscribes atomically with the log replay, so a log line
  emitted between replay and subscription is neither lost nor duplicated.
- Request validation failures answer `422 {"detail": ...}`; the heartbeat
  endpoints accept an empty body as the default request.
- `size=0` on the list endpoint yields an empty page with `X-Total-Pages: 1`
  instead of the original division by zero.
- Unknown search methods and taxonomy state rejections answer
  `500 {"detail": ...}` (the original raised an unhandled `ValueError`).
- `vector_N` is accepted as a search method alias for `vector` with
  `top_k = N`, in addition to the wire form the front end sends.
- The vector stack is the in-memory store of `a2x-search` (see its README),
  so `purge_dataset` deletes a JSON collection instead of a ChromaDB
  collection.
- Renaming a skill keeps its original service id, so `DELETE /skills/{name}`
  must use the original name. This matches the Python behaviour and is
  documented here as a known limitation.
- Reading configuration of a nonexistent dataset (`GET .../register-config`)
  creates its directory, as `RegistryStore.__init__` did.

## Intentionally left out

- `--reload` on `a2x-registry` (accepted, ignored).
- The interactive OpenAPI docs page (`/docs`).
- The `A2X_REGISTRY_MODE` / `A2X_REGISTRY_BIND` appliance variables (not in
  the original branch either).
- `tests/cluster` (owned by the `a2x-cluster` crate) and the Python import
  time lite / full extras probing tests; feature availability is a runtime
  flag in `a2x_common::feature_flags`.
- The uvicorn access log filter for `/api/warmup-status`.
