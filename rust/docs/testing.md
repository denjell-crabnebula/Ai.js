# Testing

## Running the suites

```bash
cargo test --workspace                 # everything, offline
cargo test -p a4p                      # one crate
cargo test -p mcp-sdk --test http_st   # one integration test binary
cargo test -p a2x-cluster -- membership # tests whose name contains "membership"
```

The suites start real servers on ephemeral ports inside the test process, drive stdio transports
through in-memory pipes, script LLM answers with `a2x_search::testing::FakeLlm`, and replace
environment variables with `ap_support::env::MapEnv`. Nothing needs network access or
credentials. Each crate README maps its test files to the original C++ or Python test files.

## The `TestResult` style

Tests return `TestResult` and use `?` on every fallible call. A failure prints the error with its
message instead of a panic backtrace, and no test contains `unwrap` or `expect`.

```rust
use ap_support::testing::{OptionExt, ResultExt, TestResult};
use ap_support::{err, some};

#[test]
fn parses_a_port() -> TestResult {
    let port: u16 = "8080".parse()?;                 // any std::error::Error propagates
    let items = vec![port];
    let first = items.first().required()?;           // Option -> Result with a generic message
    let first = some!(items.first());                // Option -> Result naming the expression
    assert_eq!(*first, 8080);
    let error = "x".parse::<u16>().err_or_fail()?;   // the Err of a Result, failing on Ok
    let error = err!("x".parse::<u16>());            // the same, naming the expression
    assert!(!error.to_string().is_empty());
    let value = Err::<u8, &str>("boom").or_fail();   // any Debug error becomes a failure
    assert!(value.is_err());
    Ok(())
}
```

| Helper | From | To |
|--------|------|----|
| `x?` | `Result<T, E: Error>` | `T` |
| `x.required()?`, `x.required_as("what")?` | `Option<T>` | `T` |
| `some!(x)` | `Option<T>` | `T`, message includes the expression text |
| `x.err_or_fail()?` | `Result<T: Debug, E>` | `E` |
| `err!(x)` | `Result<T: Debug, E>` | `E`, message includes the expression text |
| `x.or_fail()?` | `Result<T, E: Debug>` | `T`, for error types that are not `std::error::Error` |
| `TestFailure::new(msg)` | | An error value to return from a test |

Async tests work the same way with `#[tokio::test] async fn ... -> TestResult`. Helpers that
set up fixtures return `TestResult<Fixture>` and are called with `?`. Threads and spawned tasks
return `TestResult` too, and the test joins them with `handle.await??` or
`handle.join().map_err(...)?`.

Assertions stay `assert!`, `assert_eq!` and `matches!`; a match arm that must not be reached
returns `Err(TestFailure::new(...).into())`.

## Environment in tests

Never write to the process environment. Build a `MapEnv` and call the `_in(env)` variant of the
function under test:

```rust
use a4p::http_server::a4p_http_port_in;
use ap_support::env::MapEnv;

let env = MapEnv::from([("A4P_SERVER_PORT", "70000")]);
assert!(a4p_http_port_in(&env).is_err());
assert_eq!(a4p_http_port_in(&MapEnv::new()).map(|p| p), Ok(8961));
```

## Test servers and mocks

- MCP: `tests/common/mod.rs` starts Streamable HTTP servers on port 0 and stdio servers over
  `tokio::io::duplex`; `wait_for` polls a condition with a timeout and returns an error when it is
  not met.
- A2A: in-process HTTP servers plus a `MockTransport` that records requests and emits events.
- A4P: a software WebAuthn authenticator that produces real client data, attestations and
  assertions for P-256, Ed25519 and RSA keys.
- Registry: an axum test app over an in-memory registry; `AuthApp` helpers provision principals
  and tokens.
- Cluster: `a2x_cluster::testing` with an in-process transport, a fake registry and a manual clock,
  so many nodes run in one process without sockets.
- Client SDK: `MockServer::start(responder)` where the responder returns
  `TestResult<MockResponse>`; a responder error becomes a 500 response whose body carries the
  message, so the failing assertion shows it.

## Lints in tests

The same lints apply to tests as to library code: `cargo clippy --workspace --all-targets`
denies `unwrap`, `expect` and `panic`. When a mock must implement a trait method that cannot
return an error, it returns the trait's own error type or a documented default instead of
panicking.
