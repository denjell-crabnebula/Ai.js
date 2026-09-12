# a2x-registry-client

Rust port of `AgentRegistry/client` (the `a2x_registry_client` Python package): the client SDK
and CLI for the A2X agent service registry. It talks to the registry over HTTP only and
interoperates with the original FastAPI backend and with the Rust `a2x-registry` crate.

The crate ships:

- `A2xRegistryClient`, an async client (reqwest + tokio) with one method per Python method.
- `blocking::A2xRegistryClient`, a synchronous wrapper that owns a private tokio runtime.
- `ClientError`, one enum mirroring the Python exception hierarchy.
- Credential helpers for `~/.a2x_registry_client/cli_token.json`.
- `OwnershipStore`, the local record of which service ids this client registered.
- `HeartbeatRenewer` / `HeartbeatRegistry`, background lease renewal.
- The `a2x-registry-client` binary with the same subcommands as `cli.py`.

## API mapping

### Clients

| Python | Rust |
|--------|------|
| `A2XRegistryClient(base_url, timeout, api_key, ownership_file)` | `blocking::A2xRegistryClient::new(ClientConfig)` |
| `AsyncA2XRegistryClient(...)` | `A2xRegistryClient::new(ClientConfig)` |
| `ownership_file=None` / `False` / `Path` | `OwnershipFile::Default` / `Disabled` / `Path(p)` |
| `client.base_url` / `timeout` / `api_key` | `base_url()` / `timeout()` / `api_key()` |
| `close()` / `aclose()` / context manager | `close().await` (async), `close()` and `Drop` (blocking) |
| `create_dataset(name, embedding_model, formats, auth_required, lease_config)` | `create_dataset(name, &CreateDatasetOptions)` |
| `formats=UNSET` / `formats=None` / dict | `Formats::Unset` / `Formats::Omit` / `Formats::Explicit(map)` |
| `create_principal(handle, role, namespaces, note)` | `create_principal(handle, role, namespaces, note)` |
| `delete_dataset(name)` | `delete_dataset(name)` |
| `register_agent(dataset, agent_card, service_id, persistent, lease_ttl, auto_renew)` | `register_agent(dataset, &card, &RegisterOptions)` |
| `heartbeat(dataset, sid, status)` | `heartbeat(dataset, sid, status)` returns `serde_json::Value` |
| `drain(dataset, sid, reason=)` | `drain(dataset, sid)` |
| `shutdown(sids, dataset, permanent, reason, timeout, raise_on_error)` | `shutdown(&ShutdownOptions)` returns `ShutdownReport` |
| `update_agent(dataset, sid, fields)` | `update_agent(dataset, sid, &fields)` |
| `set_status(dataset, sid, status)` | `set_status(dataset, sid, status)` |
| `list_agents(dataset, page=, size=, **filters)` | `list_agents(dataset, &ListOptions)` returns `Vec<JsonObject>` |
| `get_agent(dataset, sid)` | `get_agent(dataset, sid)` returns `AgentDetail` |
| `deregister_agent(dataset, sid)` | `deregister_agent(dataset, sid)` |
| `register_blank_agent(dataset, endpoint, service_id, persistent)` | `register_blank_agent(dataset, endpoint, service_id, persistent)` |
| `list_idle_blank_agents(dataset, n)` | `list_idle_blank_agents(dataset, n)` |
| `replace_agent_card(dataset, sid, agent_card, release_lease)` | `replace_agent_card(dataset, sid, &card, release_lease)` |
| `restore_to_blank(dataset, sid)` | `restore_to_blank(dataset, sid)` |
| `reserve_blank_agents(dataset, n, ttl_seconds, holder_id, extra_filters)` | `reserve_blank_agents(dataset, &ReserveOptions)` returns `Reservation` |
| `with client.reserve_blank_agents(...) as r:` | blocking: `reserve_blank_agents_guarded(...)` returns `ReservationGuard` (releases on drop) |
| `release_reservation(reservation, service_ids)` | `release_reservation(&mut reservation, service_ids)` |
| `extend_reservation(reservation, ttl_seconds)` | `extend_reservation(&mut reservation, ttl_seconds)` |
| `release_my_lease(dataset, sid)` | `release_my_lease(dataset, sid)` |
| `client._transport.request("GET", "/api/auth/whoami")` (CLI) | `whoami()`, `list_keys()`, `create_key(name)`, `revoke_key(key_id)`, `request_json(method, path, body)` |

### Models

| Python | Rust |
|--------|------|
| `DatasetCreateResponse` | `DatasetCreateResponse` |
| `DatasetDeleteResponse` | `DatasetDeleteResponse` |
| `PrincipalCreateResponse` | `PrincipalCreateResponse` |
| `RegisterResponse` (`lease_ttl`, `lease_expires_at`) | `RegisterResponse` |
| `PatchResponse` | `PatchResponse` |
| `DeregisterResponse` | `DeregisterResponse` |
| `AgentDetail` (`id, type, name, description, metadata, raw`) | `AgentDetail` (`r#type` for `type`) |
| `Reservation` | `Reservation` (`released` flag, `is_released()`) |
| `list[dict]` flat entries | `Vec<JsonObject>` (`serde_json::Map<String, Value>`) |

### Errors

| Python | Rust `ClientError` variant | Predicate |
|--------|----------------------------|-----------|
| `A2XError` | the enum itself | |
| `A2XConnectionError` | `Connection`, `Timeout` | `is_connection()` |
| `A2XHTTPError` | `Http` (other 4xx) and every HTTP variant below | `is_http()`, `status_code()`, `payload()` |
| `A2XAuthenticationError` (401) | `Authentication` | |
| `A2XAuthorizationError` (403) | `Authorization` | |
| `NotFoundError` (404) | `NotFound` | `is_not_found()` |
| `ValidationError` (400 / 422) | `Validation` | `is_validation()` |
| `UserConfigServiceImmutableError` | `UserConfigServiceImmutable` | `is_validation()` |
| `A2XHeartbeatNotSupportedError` | `HeartbeatNotSupported` | `is_validation()` |
| `A2XTTLRequiredError` (`min_ttl`, `max_ttl`) | `TtlRequired` | `min_ttl()`, `max_ttl()` |
| `A2XTTLOutOfRangeError` | `TtlOutOfRange` | `min_ttl()`, `max_ttl()` |
| `UnexpectedServiceTypeError` | `UnexpectedServiceType` | |
| `ServerError` (5xx) | `Server` | |
| `NotOwnedError` | `NotOwned` | `is_not_owned()` |
| `ValueError` (local validation) | `InvalidArgument` | |
| `json.JSONDecodeError` / `TypeError` on bad bodies | `Decode` | |
| `OSError` | `Io` | |

Status code mapping is identical to `transport._wrap_http_error`: 401, 403, 404, 400/422 (with
the `detail.code` dispatch for heartbeat errors and the `user_config` substring rule), 5xx,
and a generic `Http` for anything else (409 included).

### Credentials, ownership, heartbeat

| Python | Rust |
|--------|------|
| `DEFAULT_BASE_URL` | `DEFAULT_BASE_URL` |
| `DEFAULT_CONFIG_PATH` | `default_config_path()` |
| `read_cli_token(path)` | `read_cli_token(Option<&Path>)` returns `Option<CliToken>` |
| `write_cli_token(api_key, base_url, path)` | `write_cli_token(api_key, base_url, Option<&Path>)` |
| `remove_cli_token(path)` | `remove_cli_token(Option<&Path>)` |
| `resolve_credentials(api_key, base_url, config_path)` | `resolve_credentials(api_key, base_url, config_path)` |
| `OwnershipStore(file_path, base_url)` | `OwnershipStore::new(Option<PathBuf>, base_url)` |
| `HeartbeatRenewer(ds, sid, ttl_seconds, heartbeat_fn, period=)` | `HeartbeatRenewer::new(ds, sid, ttl, HeartbeatFn, Option<Duration>)` |
| `HeartbeatRegistry` / `AsyncHeartbeatRegistry` | `HeartbeatRegistry` |
| `renewer.start()` / `stop()` | `start()` / `stop().await` / `signal_stop()` |
| daemon thread | tokio task, aborted when the last handle is dropped |

## Quick start

### Async

```rust,no_run
use a2x_registry_client::{A2xRegistryClient, ClientConfig, ListOptions, OwnershipFile, RegisterOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), a2x_registry_client::ClientError> {
    // No base_url or api_key: both come from ~/.a2x_registry_client/cli_token.json.
    let client = A2xRegistryClient::new(
        ClientConfig::new()
            .base_url("http://127.0.0.1:8000")
            .ownership_file(OwnershipFile::Disabled),
    )?;

    let card = json!({
        "name": "EN-ZH Translator",
        "description": "Translate EN to simplified ZH.",
        "url": "https://translator-01.internal/a2a",
        "status": "online",
    });
    let opts = RegisterOptions { lease_ttl: Some(60), auto_renew: true, ..Default::default() };
    let resp = client.register_agent("translators", card.as_object().unwrap(), &opts).await?;
    println!("registered {} (lease expires at {:?})", resp.service_id, resp.lease_expires_at);

    let online = client
        .list_agents("translators", &ListOptions::new().filter("status", "online"))
        .await?;
    for agent in &online {
        println!("{} {}", agent["id"], agent["name"]);
    }

    client.set_status("translators", &resp.service_id, "busy").await?;
    client.deregister_agent("translators", &resp.service_id).await?;
    client.close().await; // stops the background heartbeat renewer
    Ok(())
}
```

### Blocking

```rust,no_run
use a2x_registry_client::blocking::A2xRegistryClient;
use a2x_registry_client::{ClientConfig, OwnershipFile, ReserveOptions};

fn main() -> Result<(), a2x_registry_client::ClientError> {
    let leader = A2xRegistryClient::new(
        ClientConfig::new().base_url("http://127.0.0.1:8000").ownership_file(OwnershipFile::Disabled),
    )?;
    let opts = ReserveOptions { n: 1, ttl_seconds: 30, ..Default::default() };
    {
        let reservation = leader.reserve_blank_agents_guarded("team_pool", &opts)?;
        if let Some(teammate) = reservation.agents.first() {
            println!("locked {} at {}", teammate["id"], teammate["endpoint"]);
            // ... P2P negotiation with the teammate ...
        }
        // Leaving the scope releases the lease (best-effort), like Python's `with`.
    }
    Ok(())
}
```

Teammate side (`register_blank_agent`, `replace_agent_card`, `restore_to_blank`) works the same
way through either client; see the Python `README_agentteam.md` for the flow.

### Error handling

```rust,no_run
use a2x_registry_client::ClientError;

fn describe(err: &ClientError) -> String {
    match err {
        ClientError::NotOwned { dataset, service_id } => format!("not ours: {dataset}/{service_id}"),
        ClientError::TtlOutOfRange { min_ttl, max_ttl, .. } => format!("ttl must be in [{min_ttl:?}, {max_ttl:?}]"),
        e if e.is_validation() => format!("rejected: {e}"),
        e if e.is_connection() => format!("network: {e}"),
        e => e.to_string(),
    }
}
```

## CLI

The binary is `a2x-registry-client`, with the same subcommands and flags as the Python
`cli.py`:

```text
a2x-registry-client [--base-url URL] login [--token TOKEN]   prompt for URL and token, save cli_token.json
a2x-registry-client logout                                   remove cli_token.json
a2x-registry-client [--base-url URL] whoami                  GET /api/auth/whoami
a2x-registry-client [--base-url URL] keys list               GET /api/auth/keys
a2x-registry-client [--base-url URL] keys create --name X    POST /api/auth/keys (prints the token once)
a2x-registry-client [--base-url URL] keys revoke KEY_ID      DELETE /api/auth/keys/{key_id}
```

```bash
cargo run -p a2x-registry-client -- --base-url http://127.0.0.1:8000 login --token a2x_pat_...
cargo run -p a2x-registry-client -- whoami
cargo run -p a2x-registry-client -- keys create --name laptop
```

`login` validates the `a2x_pat_` prefix (exit 2), writes the token file with mode 0600, then
pokes `/api/auth/whoami`: a 401 exits 1, any other failure keeps the token and exits 0.
`keys revoke` exits 3 on a 403. `whoami` and `keys *` print the server JSON on stdout; errors go
to stderr with exit 1. Warnings from the SDK are logged through `tracing` to stderr; set
`RUST_LOG=debug` for more.

## Tests

```bash
cargo test -p a2x-registry-client
```

Every original test file is ported (`test_admin_methods`, `test_cli_token_io`,
`test_error_mapping`, `test_error_subclasses`, `test_heartbeat_renewer`,
`test_resolve_credentials`, plus `conftest`'s isolated home as explicit temp paths). The Python
`httpx.MockTransport` handlers are replaced by an in-process axum server on `127.0.0.1:0`
(`tests/common/mod.rs`) that records requests and replies with the JSON shapes from
`docs/backend_api.md`. Additional tests cover every client method, ownership persistence, the
lease-release hook, the reservation flow, the blocking wrapper and the CLI.

## Deviations from the original

- One `ClientError` enum replaces the exception class hierarchy; `is_http()`, `is_validation()`
  and friends replace `isinstance` checks. Timeouts are a separate `Timeout` variant (Python
  folds them into `A2XConnectionError`); `is_connection()` covers both.
- `Reservation` is not a context manager on the async client (Rust has no async drop). Call
  `release_reservation` explicitly. The blocking client offers `ReservationGuard`.
- `ClientConfig::heartbeat_period` is a test hook that overrides the `max(1s, ttl / 3)` renewal
  period; the Python client has no such knob.
- `ClientConfig::config_path` lets a client read a `cli_token.json` from a custom location; the
  Python client always uses `~/.a2x_registry_client/cli_token.json`.
- `shutdown` applies `timeout` to each call (the Python docstring promises it but the code never
  uses the argument). `reason` is kept but unused on both sides.
- The renewer task is aborted when the last handle is dropped; Python relies on daemon threads.
- Warnings (`warnings.warn`, `logger.warning`) are `tracing::warn!` events.
- The CLI prints server JSON for `keys list` / `keys create` / `keys revoke` instead of the
  Python text lines; `login` / `logout` keep the same messages. `login` reads the token from
  stdin without turning echo off (Python uses `getpass`). `--base-url` given after `login`
  is honoured (argparse silently dropped the global one in that case).
- The CLI disables ownership persistence; the Python CLI loaded the default `owned.json` but
  never wrote to it.
- Redirects are not followed, matching `httpx` defaults.

## Intentionally left out

- Environment variables: the Python SDK reads none (by design, see `docs/auth_design.md`), so
  neither does this crate. There is also no retry logic, because the original has none.
- `UserConfigDeregisterForbiddenError`, the deprecated alias of
  `UserConfigServiceImmutableError`.
- `HTTPTransport` / `AsyncHTTPTransport` as two types: there is one async `Transport`; the
  blocking client wraps it.
- `lease_config` GET / POST helpers: the Python client only defines the path helper
  (`internal::lease_config_path` is kept), no method.
- `register_generic` for `POST /services/generic`: not part of the Python client.
