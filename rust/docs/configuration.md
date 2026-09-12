# Configuration

Every setting has a default; environment variables override it. Binaries read the process
environment. Library functions that read the environment also have an `_in(env)` variant taking
an `ap_support::env::EnvSource`, so a host program can pass a `MapEnv` instead; see
[Error handling](error-handling.md).

## Lockdown

Every binary checks its environment against a policy before it starts. The policy lists each
variable the linked crates read, its type, its default and the values a safe deployment accepts.
`--env-lockdown` (or `AP_ENV_LOCKDOWN`) selects how strictly the policy is enforced:

| Level | Behaviour |
|-------|-----------|
| `open` | No checks; every value is used as given |
| `restricted` (default) | A value that breaks a rule is refused: the variable reads as unset, the default applies, and a warning names the variable and the reason |
| `locked` | As `restricted`, and any violation stops the binary with exit status 2 and a report. Unknown variables in the `AP_`, `A4P_` and `A2X_` namespaces are violations (typo protection), `trace` logging is refused, deployment-tier variables must say `production`, and the A4P signing keys must be set |

The rules that classify a value as dangerous:

| Rule | Applies to | Refuses |
|------|------------|---------|
| Loopback only | bind addresses (`A4P_SERVER_HOST`) | `0.0.0.0`, `::` or any non-loopback address, unless `--allow-public-bind` is given |
| HTTPS for remote hosts | client and advertise URLs | `http://` to a host that is not loopback (advertise URLs only under `locked`) |
| Safe path | home, data, state and UI directories | `/`, `/etc`, `/proc`, `/sys`, `/dev`, `/boot`, `/usr`, `/bin`, `/root` and paths containing `..` |
| Bounded numbers | worker counts, ports, timeouts, intervals, bucket counts | values outside the documented range, including 0 workers |
| Allowed values | log level and format, embedding backend, lockdown level, feature lists | anything not in the list |
| No development key | `INTENT_SERVER_ED25519_PRIVATE_KEY`, `OPERATION_SERVER_ED25519_PRIVATE_KEY` | the built-in development seed labels |
| Production tier | `A4P_ENV`, `APP_ENV`, `ENV`, `PYTHON_ENV` | any value other than `prod` or `production` under `locked` |

Secrets are redacted in reports. `--env-report` prints the variables the policy saw, so a
deployment can be audited without exposing key material.

```bash
a2x-registry --env-lockdown locked                 # refuse to start on any violation
A4P_SERVER_HOST=0.0.0.0 a2x-registry --allow-public-bind
a2x-registry --env-report --env-lockdown restricted
```

In your own program, compose a policy from the crates you use and install it once at startup;
every library read then goes through the checked environment:

```rust,no_run
use ap_support::env::{lockdown, EnvPolicy, Lockdown};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = EnvPolicy::base().merge(a4p::env_policy());
    let report = lockdown(policy, Lockdown::Locked)?;   // Err carries the report
    println!("{} variables checked", report.seen.len());
    Ok(())
}
```

`EnvPolicy::markdown_table()` renders a policy as a Markdown table for documentation. A crate
declares its own variables with `VarSpec` and `Rule`; see `ap_support::env` and the
`env_policy()` function of each crate.

## Logging (all binaries)

| Variable | Default | Meaning |
|----------|---------|---------|
| `AP_LOG_LEVEL` | `info` | `off`, `error`, `warn`, `info`, `debug`, `trace` |
| `AP_LOG_FORMAT` | `text` | `text`, `compact`, `pretty`, `json` |
| `RUST_LOG` | unset | Per-target filter directives, override the level |
| `AP_ENV_LOCKDOWN` | `restricted` | `open`, `restricted` or `locked`; see Lockdown above |

## A4P

| Variable | Default | Meaning |
|----------|---------|---------|
| `A4P_SERVER_HOST` | `127.0.0.1` | Bind address of `A4PHTTPServer` |
| `A4P_SERVER_PORT` | `8961` | Bind port; must be 1 to 65535 |
| `A4P_SERVER_BASE_URL` | `http://127.0.0.1:8961` | Base URL used by `A4PClient::new()` |
| `A4P_HTTP_TIMEOUT_S` | `300` | Client request timeout in seconds, minimum 1 |
| `A4P_USAGE_DB_PATH` | `.a4p/intent_token_usage.sqlite3` | SQLite intent-token usage store |
| `A4P_USER_AUTHORIZER_BASE_URL` | example default | Where the agent simulator example forwards requests |
| `INTENT_SERVER_ED25519_PRIVATE_KEY` | development key | Intent Server signing key: PKCS8 PEM or a 32-byte seed as base64url, `base64url:`, `base64:` or `hex:` |
| `OPERATION_SERVER_ED25519_PRIVATE_KEY` | development key | Operation Server signing key, same formats |
| `A4P_ENV`, `APP_ENV`, `ENV`, `PYTHON_ENV` | unset | `prod` or `production` refuses the built-in development keys |

## Registry

| Variable | Default | Meaning |
|----------|---------|---------|
| `A2X_REGISTRY_HOME` | see below | Home directory holding `database/` and `auth_data/` |
| `A2X_REGISTRY_AUTH_DATA` | `<home>/auth_data` | Authentication data directory |
| `A2X_FRONTEND_DIST_DIR` | unset | Built React UI to serve at `/` |
| `A2X_REGISTRY_DISABLED_FEATURES` | unset | Comma-separated features to disable: `vector`, `evaluation` |
| `A2X_REGISTRY_SEARCH_WORKERS` | `4` | Concurrent `/api/search` requests |
| `A2X_REGISTRY_DATASET_WORKERS` | `2` | Concurrent dataset operations and builds |
| `A2X_REGISTRY_LLM_WORKERS` | `20` | Concurrent LLM calls |
| `A2X_REGISTRY_AGENT_CARD_WORKERS` | `10` | Concurrent agent-card fetches |
| `A2X_REGISTRY_EMBEDDING_BACKEND` | `auto` | `auto`, `hashing` or `openai` |

The home is `A2X_REGISTRY_HOME` when set, else the current directory when it contains `database/`,
else `~/.a2x_registry`. Worker counts must be positive integers; an invalid value logs a warning
and keeps the default.

## Cluster

Prefix `A2X_REGISTRY_CLUSTER_`; invalid values log a warning and keep the default. Integer knobs
accept `10` or `10.0`.

| Variable | Default | Meaning |
|----------|---------|---------|
| `..._ADVERTISE` | empty | URL peers use to reach this node |
| `..._STATE` | `<home>/cluster_state.json` | Path of the persisted node state |
| `..._KEEPALIVE_INTERVAL` | `10` s | Gossip keepalive period |
| `..._HOLD_TIMEOUT` | `30` s | Time before an unresponsive peer is dropped |
| `..._ANTI_ENTROPY_INTERVAL` | `20` s | Merkle reconciliation period |
| `..._HTTP_TIMEOUT` | `5` s | Peer request timeout |
| `..._BROADCAST_WORKERS` | `32` | Parallel broadcast fan-out |
| `..._MERKLE_BUCKETS` | `256` | Merkle tree buckets; must match across the cluster |

Derived: tombstone retention is `hold_timeout + keepalive_interval`.

## Search

| Variable | Default | Meaning |
|----------|---------|---------|
| `A2X_REGISTRY_HOME` | `~/.a2x_registry` | Where `database/{dataset}` lives |
| `A2X_REGISTRY_LLM_WORKERS` | `20` | Default for `--max-workers` and `--workers` |
| `A2X_REGISTRY_EMBEDDING_BACKEND` | `auto` | Vector search backend |

LLM provider credentials come from `llm_apikey.json`, not from the environment.

## Client SDK

The client reads no environment variables. Base URL and API key come from `ClientConfig`, then
from `~/.a2x_registry_client/cli_token.json`; the ownership file is
`~/.a2x_registry_client/owned.json` unless `ClientConfig::ownership_file` says otherwise.

## Files

| File | Written by | Content |
|------|-----------|---------|
| `llm_apikey.json` | you | LLM provider, model, API key and endpoint for search and build |
| `database/{ds}/service.json` | registry | Registered services of a dataset |
| `database/{ds}/taxonomy/{taxonomy,class,build_config}.json` | `a2x-build` | The taxonomy and its build hash |
| `database/chroma/{ds}.json` | vector search | Persisted vector store |
| `cluster_state.json` | `cluster init` | Node id, cluster id, roster |
| `.a4p/ed25519_trusted_server_keys.json` | A4P example server | Trust anchors for the User Authorizer |
| `~/.a2x_registry_client/cli_token.json` | `a2x-registry-client login` | Base URL and API token, mode 0600 |
