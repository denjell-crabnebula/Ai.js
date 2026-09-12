# Error handling

## Policy

The workspace has no `unwrap()`, `expect()`, `panic!`, `unreachable!` or `unsafe` in any
Rust source, tests included. `Cargo.toml` denies `clippy::unwrap_used`, `clippy::expect_used`
and `clippy::panic` for every crate and forbids unsafe code, so `cargo clippy --all-targets`
fails on a new occurrence. Assertions in tests (`assert!`, `assert_eq!`) are the one accepted
way to stop on a failed expectation.

What that means in practice:

- A library function that can fail returns `Result<T, E>` with the crate's error enum
  (`McpError`, `A2aServerError`, `A2aClientError`, `A4PError`, `RegistryError`, `ClusterError`,
  `ClientError`, `A2xError`, `IntentTokenUsageStoreError`). Callers use `?`.
- A lookup that can legitimately miss returns `Option`; callers use `let Some(x) = ... else`
  or `ok_or_else` to turn a miss into an error with context.
- Values that were "checked above" are re-checked with `let ... else` rather than unwrapped.
- Locks are `parking_lot` mutexes, whose guards cannot be poisoned, so `lock()` returns the guard
  directly.
- Arithmetic and conversions that can overflow or fail use `checked_*`, `try_into()` with
  `map_err`, and `Option` chains.
- Regular expressions were replaced by small string scanners where the pattern was static, so no
  `Regex::new(...)` needs unwrapping at startup.
- Binaries convert an unusable argument, a missing feature or a runtime that cannot start into a
  message on stderr and a non-zero exit code.
- Best-effort paths (audit logs, background renewals, test servers shutting down) log the error
  and continue instead of propagating it.

## Error types

Each crate defines its errors with `thiserror`. The variants carry the information a caller needs
to decide what to do: an HTTP status class, a protocol code, a path, the underlying I/O or JSON
error as `source`. Examples:

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

Protocol-level rejections that are part of the wire contract (an invalid mandate in A4P, a
JSON-RPC error response in MCP and A2A) are not Rust errors: they are successful responses whose
payload says `approved: false` or carries an error object with a stable code, exactly as the
original implementations return them.

## The `ap-support` macros

`ap-support` provides five macros for writing fallible code without `unwrap`:

| Macro | Expands to |
|-------|------------|
| `bail!(err)` | `return Err(err.into())` |
| `ensure!(cond, err)` | `if !cond { bail!(err) }` |
| `some_or_bail!(option, err)` | The value, or `bail!(err)` when `None` |
| `ok_or_bail!(result, \|e\| err)` | The value, or `bail!(err)` built from the original error |
| `try_or_log!(result, "context", fallback)` | The value, or a `warn` log with the error and the fallback |

```rust
use ap_support::{bail, ensure, ok_or_bail, some_or_bail, try_or_log};

fn port_from(text: Option<&str>) -> Result<u16, String> {
    let text = some_or_bail!(text, "no port given");
    ensure!(!text.is_empty(), "port is empty");
    let port = ok_or_bail!(text.parse::<u16>(), |e| format!("bad port {text:?}: {e}"));
    if port == 0 {
        bail!("port 0 is reserved");
    }
    Ok(port)
}

fn workers(text: &str) -> usize {
    try_or_log!(text.parse::<usize>(), "A2X_REGISTRY_LLM_WORKERS is not a number", 20)
}
```

`bail!` works with any error type that implements `From` for the expression you pass, including
`String` into `Box<dyn Error>` and the crate error enums that implement `From<&str>` or have
constructors.

## Environment sources

Configuration that comes from environment variables is read through
`ap_support::env::EnvSource`, so the same code serves production (`ProcessEnv`) and tests or
embedders (`MapEnv`) without writing to the process environment:

```rust
use ap_support::env::{parse_var, EnvSource, MapEnv, ProcessEnv};

fn port(env: &dyn EnvSource) -> Result<u16, String> {
    Ok(parse_var::<u16>(env, "MY_PORT").map_err(|e| e.to_string())?.unwrap_or(8080))
}

assert_eq!(port(&MapEnv::from([("MY_PORT", "9000")])), Ok(9000));
assert_eq!(port(&MapEnv::new()), Ok(8080));
assert!(port(&MapEnv::from([("MY_PORT", "x")])).is_err());
let _ = port(&ProcessEnv);
```

Library code never reads `std::env` directly. It reads `ap_support::env::current()`, which is
the process environment until a binary installs a policy-checked view with
`ap_support::env::lockdown`; see [Configuration](configuration.md). The SDK functions that read
configuration all have an `_in(env)` variant:
`A4PClient::with_env`, `a4p_http_port_in`, `is_production_environment_in`,
`intent_server_signing_key_in`, `ClusterConfig::from_env_source`, `env_workers_in`,
`llm_workers_from_env_in`, `embedding_backend_from`.

## JSON values

`ap_support::json::object(value)` returns the map inside a JSON object value and an empty map for
anything else, which replaces the `json!({...}).as_object().unwrap()` idiom for literals that
are known to be objects.
