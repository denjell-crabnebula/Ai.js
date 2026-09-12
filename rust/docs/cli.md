# Command-line tools

All binaries are built by `cargo build --workspace` into `target/debug/`, or run with
`cargo run -p <crate> --bin <name> -- <args>`.

| Binary | Crate | Purpose |
|--------|-------|---------|
| `a2x-registry` | `a2x-registry` | Backend API server; `auth` and `cluster` administration subcommands |
| `a2x-register` | `a2x-registry` | Offline dataset, service and skill management |
| `a2x-registry-client` | `a2x-registry-client` | Login, `whoami` and API key management against a server |
| `a2x-cluster` | `a2x-cluster` | Standalone cluster node for experiments |
| `a2x-build` | `a2x-search` | Build a dataset's taxonomy with an LLM |
| `a2x-search` | `a2x-search` | Search a dataset from the command line |
| `a2x-evaluate-a2x` | `a2x-search` | Evaluate hierarchical search on a query file |
| `a2x-evaluate-traditional` | `a2x-search` | Evaluate the traditional (full-context) baseline |
| `a2x-evaluate-vector` | `a2x-search` | Evaluate vector search |

`--help` on any binary prints its options; `<binary> <subcommand> --help` prints a subcommand's.

## Shared logging flags

Every binary takes the same logging options, listed under the `Logging` heading of `--help`:

| Flag | Environment variable | Default | Meaning |
|------|----------------------|---------|---------|
| `--log-level <LEVEL>` | `AP_LOG_LEVEL` | `info` | `off`, `error`, `warn`, `info`, `debug` or `trace` |
| `--log-format <FORMAT>` | `AP_LOG_FORMAT` | `text` | `text`, `compact`, `pretty` or `json` |
| `--log-filter <DIRECTIVES>` | `RUST_LOG` | unset | Per-target directives, for example `info,a2x_cluster=trace` |
| `-v`, `--verbose` | | | One step louder per occurrence (`-vv` from `info` is `trace`) |
| `--quiet` | | | One step quieter per occurrence |
| `--log-target` | | off | Show the module path of each event |

Logs always go to stderr, so stdout stays machine-readable (`a2x-register --json`,
`a2x-registry-client whoami`). See [Logging](logging.md).

## Shared environment flags

| Flag | Environment variable | Default | Meaning |
|------|----------------------|---------|---------|
| `--env-lockdown <LEVEL>` | `AP_ENV_LOCKDOWN` | `restricted` | `open`, `restricted` or `locked` |
| `--allow-public-bind` | | off | Permit bind addresses other than loopback |
| `--env-report` | | off | Print the variables the lockdown policy saw, secrets redacted |

Under `locked`, a violation prints the report and exits with status 2 before anything else runs.
See [Configuration](configuration.md) for the rules.

## `a2x-registry`

```text
a2x-registry [--host 127.0.0.1] [--port 8000] [--reload]
a2x-registry auth init [--handle root] [--admin-token a2x_pat_...] [--data-dir DIR]
a2x-registry auth reset-admin --confirm
a2x-registry cluster init [--node-id ID]
a2x-registry cluster status [--server URL]
a2x-registry cluster add-peer <address> [--namespaces a,b] [--token T] [--server URL]
a2x-registry cluster rm-peer <node_id> [--server URL]
a2x-registry cluster set add <address>... [--token T] [--server URL]
a2x-registry cluster set remove <node_id>... [--server URL]
a2x-registry cluster set show [--server URL]
```

`auth init` prints the admin token once on stderr. `--server` defaults to
`http://127.0.0.1:8000`. Exit codes: 0 on success, 1 on error or an uninitialised cluster, 2 for
a missing feature.

## `a2x-register`

```text
a2x-register [--database-dir DIR] [--config FILE] [--json] <command>
  status [--dataset DS]      datasets
  create-dataset <name> [--embedding-model M] [--formats generic,a2a:v1.0]
  get-register-config <ds>   set-register-config <ds> --formats a2a:v1.0
  delete-dataset <ds> --confirm
  list <ds> [--mode browse|admin]
  get <ds> <service_id>
  register-generic <ds> --name N --desc D [--url U] [--input-schema FILE] [--service-id ID]
  register-a2a <ds> (--url URL | --card-file card.json)
  register-skill <ds> <skill.zip>
  update <ds> <service_id> [--json FILE] [--set k=v ...] [--name N] [--desc D] [--url U] [--license L]
  deregister <ds> <service_id>
  deregister-skill <ds> <skill_name>
```

## `a2x-registry-client`

```text
a2x-registry-client [--base-url URL] login [--token TOKEN]
a2x-registry-client logout
a2x-registry-client [--base-url URL] whoami
a2x-registry-client [--base-url URL] keys list
a2x-registry-client [--base-url URL] keys create --name X
a2x-registry-client [--base-url URL] keys revoke KEY_ID
```

`login` validates the `a2x_pat_` prefix (exit 2), writes `~/.a2x_registry_client/cli_token.json`
with mode 0600 and checks `/api/auth/whoami` (a 401 exits 1). `keys revoke` exits 3 on a 403.
Results print as JSON on stdout; errors go to stderr with exit 1.

## `a2x-cluster`

```text
a2x-cluster init [--node-id ID]
a2x-cluster serve --bind HOST:PORT
a2x-cluster status [--server URL]
a2x-cluster add-peer <address> [--server URL]
a2x-cluster rm-peer <node_id> [--server URL]
a2x-cluster set add|remove|show ...
```

`serve` runs an in-memory node with no local records; set `A2X_REGISTRY_CLUSTER_ADVERTISE` to the
URL peers should use.

## Search binaries

```text
a2x-build --service-path FILE [--output-dir DIR] [--resume no|keyword|yes] [--keyword-batch-size 50]
          [--keyword-threshold 500] [--max-service-size 40] [--max-categories-size 20]
          [--generic-ratio 0.333] [--delete-threshold 2] [--max-depth 3] [--workers 20]
          [--max-refine-iterations 3] [--no-cross-domain]
a2x-search [--query TEXT] [--mode get_all|get_important|get_one] [--parallel true|false]
           [--max-workers N] [--dataset DS]
a2x-evaluate-a2x --data-dir DIR [--query-file FILE] [--service-path FILE] [--max-queries N]
                 [--mode MODE] [--workers N] [--notes TEXT]
a2x-evaluate-traditional --service-path FILE --query-file FILE [--max-queries N]
a2x-evaluate-vector [--max-queries N] [--top-k K] [--top-k-list 5,10] [--embedding-backend auto|hashing|openai]
                    [--model-name M] [--force-rebuild] [--persist-dir DIR]
```

`Ctrl-C` during a build cancels at the next checkpoint; `--resume yes` continues it. Output
directories default to `results/{date}_{method}[-{mode}]_{dataset}[-{suffix}]_{count}`. All
search binaries read the LLM configuration from `llm_apikey.json` and honour
`A2X_REGISTRY_HOME`.

## Exit behaviour

Binaries never panic on bad input or a failed dependency. A configuration problem (an unusable
argument value, a missing feature, a runtime that cannot start) is printed to stderr and the
process exits with a non-zero status: 1 for runtime errors, 2 for usage and feature errors, and
the documented codes above for the client tool.
