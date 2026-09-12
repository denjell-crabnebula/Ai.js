# Logging

All crates log through [`tracing`](https://docs.rs/tracing). Libraries emit events and never
install a subscriber; the executable decides how events are filtered and rendered. The
`ap-support` crate provides that setup so every binary behaves the same way.

## Levels

| Level | Use in the SDKs |
|-------|-----------------|
| `error` | A request or background task failed and nothing recovered it |
| `warn` | Recovered problems: a rejected value replaced by a default, a retry, an ignored malformed record |
| `info` | Lifecycle and normal operation: listening addresses, sessions opened and closed, builds started and finished |
| `debug` | Protocol detail: method names, request ids, state transitions, lease renewals |
| `trace` | Everything, including message bodies and per-item progress |
| `off` | Nothing |

The MCP and A2A SDKs keep their original log callback APIs (`mcp_log!`, `a2a_log!`,
`set_log_callback`) and also emit each line as a `tracing` event, so both mechanisms see the same
messages.

## Targets

Events carry the module path of the crate that emitted them, so filters can single out one
component:

| Target prefix | Crate |
|---------------|-------|
| `mcp_sdk` | MCP client and server |
| `a2a_sdk` | A2A client and server |
| `a4p` | A4P facade, HTTP server, client |
| `a2x_registry` | Registry backend and CLIs |
| `a2x_search` | Taxonomy build and search |
| `a2x_cluster` | Cluster replication |
| `a2x_registry_client` | Client SDK |
| `a2x_common` | Shared registry code, LLM client |

## In binaries

Every binary takes the shared `--log-level`, `--log-format`, `--log-filter`, `-v`,
`--quiet` and `--log-target` flags (see [Command-line tools](cli.md)). Resolution order:

1. `--log-filter` or `RUST_LOG` directives, when present, decide per target.
2. Otherwise `--log-level` (or `AP_LOG_LEVEL`), adjusted one step by each `-v` or `--quiet`.
3. Otherwise `info`.

Examples:

```bash
a2x-registry --log-level debug
a2x-registry --log-format json --log-target            # one JSON object per line, with module paths
RUST_LOG=warn,a2x_cluster=trace a2x-registry
a2x-build --service-path db/s.json -vv                 # trace
a2x-registry-client whoami --quiet                     # warnings and errors only
```

Logs always go to stderr.

## In your own program

Enable the `logging` feature of `ap-support` (and `cli` for the `clap` argument group):

```toml
[dependencies]
ap-support = { path = "../agent-protocol/rust/ap-support", features = ["cli"] }
```

Configure and install a subscriber once at startup:

```rust,no_run
use ap_support::logging::{LogConfig, LogFormat, LogLevel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ap_support::logging::init(
        &LogConfig::new(LogLevel::Debug)
            .format(LogFormat::Compact)
            .filter("debug,hyper=warn")
            .with_target(true),
    )?;
    tracing::info!("ready");
    Ok(())
}
```

`ap_support::logging::init_from_env()` reads `AP_LOG_LEVEL`, `AP_LOG_FORMAT` and `RUST_LOG`
and never fails the caller; a bad value is reported on stderr and the defaults apply. The A4P
examples use it.

With `clap`, flatten `LoggingArgs` into your parser and call `init()` on it:

```rust,no_run
use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli.logging.init() {
        eprintln!("{e}");
    }
}
```

`init` fails only when a global subscriber is already installed; the binaries print that and
continue.

## In libraries and tests

Libraries call `tracing::{error, warn, info, debug, trace}!` with structured fields
(`tracing::warn!(error = %e, "lease renewal failed")`) and never call `init`. The
`try_or_log!` macro from `ap-support` logs a recoverable error at `warn` and continues with a
fallback value; see [Error handling](error-handling.md).

Tests do not install a subscriber. To see a test's log output, set `RUST_LOG` and install one at
the top of the test with `ap_support::logging::init_from_env()`; a second test in the same
process that calls it again is a no-op.
