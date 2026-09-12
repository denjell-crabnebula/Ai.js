// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `a2x-register`: manage datasets, services and skills directly on disk.
//!
//! Mirrors `python -m a2x_registry.register`. Global options: `--database-dir`,
//! `--config`, `--json`, `-v`. Legacy `--status` / `--config` invocations
//! without a subcommand are still supported.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::{Map, Value, json};

use a2x_registry::register::embedding::DEFAULT_EMBEDDING_MODEL;
use a2x_registry::register::{
    AgentCard, RegisterA2ARequest, RegisterGenericRequest, RegistryEntry, RegistryError, RegistryService,
};

#[derive(Parser)]
#[command(
    name = "a2x-register",
    about = "A2X Registry CLI - manage datasets, services, and skills"
)]
struct Cli {
    /// Logging options (see --help for levels and formats).
    #[command(flatten)]
    logging: ap_support::logging::LoggingArgs,
    /// Environment lockdown options (see --help).
    #[command(flatten)]
    env: ap_support::env::EnvArgs,
    /// Path to the database directory
    #[arg(long)]
    database_dir: Option<PathBuf>,
    /// Path to a global config file (user_config.json)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    json_output: bool,
    /// Legacy: show registry status
    #[arg(long)]
    status: bool,
    /// Legacy: dataset filter for --status
    #[arg(long)]
    dataset: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show registry status
    Status {
        #[arg(long)]
        dataset: Option<String>,
    },
    /// List all datasets
    Datasets,
    /// Create a new dataset
    CreateDataset {
        name: String,
        #[arg(long, default_value = DEFAULT_EMBEDDING_MODEL)]
        embedding_model: String,
        /// Comma-separated allowed formats, e.g. 'generic,a2a:v1.0'
        #[arg(long)]
        formats: Option<String>,
    },
    /// Show allowed registration formats
    GetRegisterConfig { dataset: String },
    /// Replace allowed registration formats
    SetRegisterConfig {
        dataset: String,
        #[arg(long)]
        formats: String,
    },
    /// Delete a dataset
    DeleteDataset {
        name: String,
        #[arg(long)]
        confirm: bool,
    },
    /// List services in a dataset
    List {
        dataset: String,
        /// browse: lightweight; admin: with type/source
        #[arg(long, default_value = "admin", value_parser = ["browse", "admin"])]
        mode: String,
    },
    /// Get a single service entry
    Get { dataset: String, service_id: String },
    /// Register a generic service
    RegisterGeneric {
        dataset: String,
        #[arg(long)]
        name: String,
        #[arg(long, alias = "desc")]
        description: String,
        #[arg(long, default_value = "")]
        url: String,
        /// Path to JSON file with inputSchema
        #[arg(long)]
        input_schema: Option<PathBuf>,
        #[arg(long)]
        service_id: Option<String>,
    },
    /// Register an A2A agent
    RegisterA2a {
        dataset: String,
        /// URL to fetch the agent card from
        #[arg(long = "url", conflicts_with = "card_file")]
        agent_card_url: Option<String>,
        /// Path to a local agent card JSON file
        #[arg(long, required_unless_present = "agent_card_url")]
        card_file: Option<PathBuf>,
        #[arg(long)]
        service_id: Option<String>,
    },
    /// Upload a skill ZIP
    RegisterSkill { dataset: String, zip_file: PathBuf },
    /// Partially update a service by ID
    Update {
        dataset: String,
        service_id: String,
        /// Path to JSON file containing the updates dict
        #[arg(long = "json")]
        json_file: Option<PathBuf>,
        /// Set a single top-level field (repeatable)
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, alias = "desc")]
        description: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        license: Option<String>,
    },
    /// Deregister a service by ID
    Deregister { dataset: String, service_id: String },
    /// Remove a skill by name
    DeregisterSkill { dataset: String, name: String },
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        return;
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let fmt = |cells: Vec<String>| {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{:<w$}", c, w = widths[i]))
            .collect();
        format!("  {}", parts.join("  "))
    };
    println!("{}", fmt(headers.iter().map(|h| h.to_string()).collect()));
    for row in rows {
        println!("{}", fmt(row.clone()));
    }
}

fn print_kv(pairs: &[(&str, String)]) {
    if pairs.is_empty() {
        return;
    }
    let max_key = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in pairs {
        println!("  {:<w$} {}", format!("{k}:"), v, w = max_key + 2);
    }
}

fn truncate(text: &str, max_len: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= max_len {
        return text;
    }
    let cut: String = text.chars().take(max_len.saturating_sub(3)).collect();
    format!("{cut}...")
}

fn parse_formats_spec(spec: &str) -> Value {
    let mut out = Map::new();
    for piece in spec.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        match piece.split_once(':') {
            Some((t, v)) => out.insert(t.trim().into(), Value::String(v.trim().into())),
            None => out.insert(piece.into(), Value::String("v0.0".into())),
        };
    }
    Value::Object(out)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(1)
}

fn read_json_file(path: &PathBuf) -> Value {
    if !path.exists() {
        fail(format!("File not found: {}", path.display()));
    }
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(e));
    serde_json::from_str(&text).unwrap_or_else(|e| fail(e))
}

fn entry_json(e: &RegistryEntry) -> Value {
    Value::Object(e.to_json_exclude_none())
}

async fn dispatch(service: &RegistryService, cmd: Command, json_output: bool) -> Result<(), RegistryError> {
    match cmd {
        Command::Status { dataset } => {
            let status = service.get_status(dataset.as_deref());
            if json_output {
                print_json(&serde_json::to_value(&status).unwrap_or(Value::Null));
                return Ok(());
            }
            println!("Registry Status");
            print_kv(&[
                ("Total services", status.total_services.to_string()),
                (
                    "Datasets",
                    if status.datasets.is_empty() {
                        "(none)".into()
                    } else {
                        status.datasets.join(", ")
                    },
                ),
            ]);
            if !status.by_source.is_empty() {
                println!("\n  By source:");
                for (src, count) in &status.by_source {
                    println!("    {src:<16} {count}");
                }
            }
        }
        Command::Datasets => {
            let ds_list = service.list_datasets();
            if json_output {
                print_json(&json!(ds_list));
                return Ok(());
            }
            if ds_list.is_empty() {
                println!("No datasets found.");
                return Ok(());
            }
            println!("Datasets:");
            for name in ds_list {
                println!("  {} ({} services)", name, service.list_services(&name).len());
            }
        }
        Command::CreateDataset {
            name,
            embedding_model,
            formats,
        } => {
            let formats = formats.map(|f| parse_formats_spec(&f));
            service.create_dataset(&name, Some(&embedding_model), formats.as_ref(), false)?;
            let effective: BTreeMap<String, String> =
                service.get_register_config(&name)?.into_iter().collect();
            if json_output {
                print_json(
                    &json!({"dataset": name, "embedding_model": embedding_model, "formats": effective, "status": "created"}),
                );
                return Ok(());
            }
            println!("Created dataset '{name}' (embedding: {embedding_model})");
            println!("  Allowed formats:");
            for (t, v) in effective {
                println!("    {t:<8} min_version={v}");
            }
        }
        Command::GetRegisterConfig { dataset } => {
            let cfg: BTreeMap<String, String> = service.get_register_config(&dataset)?.into_iter().collect();
            if json_output {
                print_json(&json!({"dataset": dataset, "formats": cfg}));
                return Ok(());
            }
            println!("Register config for '{dataset}':");
            for (t, v) in cfg {
                println!("  {t:<8} min_version={v}");
            }
        }
        Command::SetRegisterConfig { dataset, formats } => {
            let cfg: BTreeMap<String, String> = service
                .set_register_config(&dataset, &parse_formats_spec(&formats))?
                .into_iter()
                .collect();
            if json_output {
                print_json(&json!({"dataset": dataset, "formats": cfg}));
                return Ok(());
            }
            println!("Updated register config for '{dataset}':");
            for (t, v) in cfg {
                println!("  {t:<8} min_version={v}");
            }
        }
        Command::DeleteDataset { name, confirm } => {
            if !confirm {
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() {
                    eprintln!("Error: --confirm required for non-interactive use");
                    std::process::exit(1);
                }
                print!("Delete dataset '{name}' and all its data? [y/N] ");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                if answer.trim().to_lowercase() != "y" {
                    println!("Aborted.");
                    return Ok(());
                }
            }
            service.delete_dataset(&name)?;
            if json_output {
                print_json(&json!({"dataset": name, "status": "deleted"}));
                return Ok(());
            }
            println!("Deleted dataset '{name}'");
        }
        Command::List { dataset, mode } => {
            if mode == "browse" {
                let services = service.list_services(&dataset);
                if json_output {
                    print_json(&Value::Array(services));
                    return Ok(());
                }
                if services.is_empty() {
                    println!("No services in '{dataset}'.");
                    return Ok(());
                }
                println!("Services in '{dataset}' ({} total)\n", services.len());
                let rows: Vec<Vec<String>> = services
                    .iter()
                    .map(|s| {
                        vec![
                            s["id"].as_str().unwrap_or("").into(),
                            s["name"].as_str().unwrap_or("").into(),
                            truncate(s["description"].as_str().unwrap_or(""), 60),
                        ]
                    })
                    .collect();
                print_table(&["ID", "NAME", "DESCRIPTION"], &rows);
            } else {
                let mut entries = service.list_entries(&dataset);
                entries.sort_by(|a, b| a.service_id.cmp(&b.service_id));
                if json_output {
                    print_json(&Value::Array(entries.iter().map(entry_json).collect()));
                    return Ok(());
                }
                if entries.is_empty() {
                    println!("No services in '{dataset}'.");
                    return Ok(());
                }
                println!("Services in '{dataset}' ({} total)\n", entries.len());
                let rows: Vec<Vec<String>> = entries
                    .iter()
                    .map(|e| {
                        vec![
                            e.service_id.clone(),
                            e.r#type.to_string(),
                            e.source.to_string(),
                            e.display_name(),
                        ]
                    })
                    .collect();
                print_table(&["ID", "TYPE", "SOURCE", "NAME"], &rows);
            }
        }
        Command::Get { dataset, service_id } => {
            let Some(entry) = service.get_entry(&dataset, &service_id) else {
                eprintln!("Service '{service_id}' not found in '{dataset}'.");
                std::process::exit(1);
            };
            if json_output {
                print_json(&entry_json(&entry));
                return Ok(());
            }
            println!("Service: {}", entry.service_id);
            print_kv(&[
                ("Type", entry.r#type.to_string()),
                ("Source", entry.source.to_string()),
                ("Name", entry.display_name()),
                ("Description", entry.display_description()),
            ]);
            if let Some(sd) = &entry.service_data {
                if let Some(url) = sd.url.as_ref().filter(|u| !u.is_empty()) {
                    print_kv(&[("URL", url.clone())]);
                }
                if !sd.input_schema.is_empty() {
                    println!(
                        "\n  inputSchema:\n{}",
                        serde_json::to_string_pretty(&sd.input_schema).unwrap_or_default()
                    );
                }
            }
            if let Some(card) = &entry.agent_card {
                if !card.url.is_empty() {
                    print_kv(&[("Agent URL", card.url.clone())]);
                }
                if let Some(u) = &entry.agent_card_url {
                    print_kv(&[("Card URL", u.clone())]);
                }
            }
            if let Some(sk) = &entry.skill_data {
                print_kv(&[
                    ("Skill path", sk.skill_path.clone()),
                    (
                        "Files",
                        if sk.files.is_empty() {
                            "(none)".into()
                        } else {
                            sk.files.join(", ")
                        },
                    ),
                ]);
            }
        }
        Command::RegisterGeneric {
            dataset,
            name,
            description,
            url,
            input_schema,
            service_id,
        } => {
            let schema = match input_schema {
                Some(p) => match read_json_file(&p) {
                    Value::Object(m) => m,
                    _ => fail("inputSchema file must contain a JSON object"),
                },
                None => Map::new(),
            };
            let mut req = RegisterGenericRequest::new(dataset, name, description);
            req.url = url;
            req.input_schema = schema;
            req.service_id = service_id;
            let resp = service.register_generic(&req, None)?;
            if json_output {
                print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                return Ok(());
            }
            println!("Registered generic service");
            print_kv(&[
                ("ID", resp.service_id),
                ("Dataset", resp.dataset),
                ("Status", resp.status),
            ]);
        }
        Command::RegisterA2a {
            dataset,
            agent_card_url,
            card_file,
            service_id,
        } => {
            let mut req = match (card_file, agent_card_url) {
                (Some(p), _) => {
                    let card = AgentCard::from_value(read_json_file(&p)).unwrap_or_else(|e| fail(e));
                    RegisterA2ARequest::with_card(dataset, card)
                }
                (None, Some(url)) => RegisterA2ARequest::with_url(dataset, url),
                (None, None) => fail("one of --url or --card-file is required"),
            };
            req.service_id = service_id;
            let resp = service.register_a2a(&req, None).await?;
            if json_output {
                print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                return Ok(());
            }
            println!("Registered A2A agent");
            print_kv(&[
                ("ID", resp.service_id),
                ("Dataset", resp.dataset),
                ("Status", resp.status),
            ]);
        }
        Command::RegisterSkill { dataset, zip_file } => {
            if !zip_file.exists() {
                fail(format!("File not found: {}", zip_file.display()));
            }
            let bytes = std::fs::read(&zip_file).unwrap_or_else(|e| fail(e));
            let resp = service.register_skill(&dataset, &bytes, None)?;
            if json_output {
                print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                return Ok(());
            }
            println!("Registered skill");
            print_kv(&[
                ("Name", resp.name),
                ("ID", resp.service_id),
                ("Dataset", resp.dataset),
                ("Status", resp.status),
            ]);
        }
        Command::Update {
            dataset,
            service_id,
            json_file,
            set,
            name,
            description,
            url,
            license,
        } => {
            let mut updates = Map::new();
            if let Some(p) = json_file {
                match read_json_file(&p) {
                    Value::Object(m) => updates.extend(m),
                    _ => fail("--json file must contain a JSON object"),
                }
            }
            for kv in set {
                let Some((k, v)) = kv.split_once('=') else {
                    fail(format!("--set expects key=value, got '{kv}'"));
                };
                updates.insert(k.trim().into(), Value::String(v.into()));
            }
            if let Some(v) = name {
                updates.insert("name".into(), Value::String(v));
            }
            if let Some(v) = description {
                updates.insert("description".into(), Value::String(v));
            }
            if let Some(v) = url {
                updates.insert("url".into(), Value::String(v));
            }
            if let Some(v) = license {
                updates.insert("license".into(), Value::String(v));
            }
            if updates.is_empty() {
                fail("no updates provided (use --json, --set, or shortcut flags)");
            }
            let resp = service.update_service(&dataset, &service_id, &updates, None)?;
            if json_output {
                print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                return Ok(());
            }
            println!("Updated service '{}' in '{}'", resp.service_id, resp.dataset);
            print_kv(&[
                (
                    "Changed fields",
                    if resp.changed_fields.is_empty() {
                        "(none - no-op)".into()
                    } else {
                        resp.changed_fields.join(", ")
                    },
                ),
                (
                    "Taxonomy stale",
                    if resp.taxonomy_affected {
                        "yes".into()
                    } else {
                        "no".into()
                    },
                ),
            ]);
        }
        Command::Deregister { dataset, service_id } => {
            match service.deregister(&dataset, &service_id, None) {
                Ok(resp) => {
                    if json_output {
                        print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                        return Ok(());
                    }
                    println!("Deregistered service");
                    print_kv(&[("ID", resp.service_id), ("Status", resp.status)]);
                }
                Err(RegistryError::NotFound(e)) => {
                    if json_output {
                        print_json(&json!({"service_id": service_id, "status": "not_found"}));
                    } else {
                        eprintln!("{e}");
                    }
                    std::process::exit(1);
                }
                Err(e) => return Err(e),
            }
        }
        Command::DeregisterSkill { dataset, name } => {
            let resp = service.deregister_skill(&dataset, &name, None)?;
            if json_output {
                print_json(&serde_json::to_value(&resp).unwrap_or(Value::Null));
                return Ok(());
            }
            if resp.status == "not_found" {
                eprintln!("Skill '{name}' not found in '{dataset}'.");
                std::process::exit(1);
            }
            println!("Deleted skill");
            print_kv(&[
                ("Name", resp.name),
                ("ID", resp.service_id),
                ("Dataset", resp.dataset),
                ("Status", resp.status),
            ]);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli.logging.init() {
        eprintln!("{e}");
    }
    cli.env.apply_or_exit(a2x_registry::env_policy());
    let database_dir = cli
        .database_dir
        .clone()
        .unwrap_or_else(a2x_common::paths::database_dir);
    let database_dir = std::fs::canonicalize(&database_dir).unwrap_or(database_dir);
    let service = RegistryService::new(database_dir, cli.config.clone());
    let changes = match service.startup().await {
        Ok(c) => c,
        Err(e) => fail(e),
    };
    let Some(command) = cli.command else {
        if cli.status {
            let status = service.get_status(cli.dataset.as_deref());
            print_json(&serde_json::to_value(&status).unwrap_or(Value::Null));
            return;
        }
        if cli.config.is_some() {
            println!("\nRegistry startup complete:");
            for (ds, state) in &changes {
                println!(
                    "  {}: {} services, taxonomy={}",
                    ds,
                    service.list_services(ds).len(),
                    state
                );
            }
            let total = service.get_status(None);
            println!(
                "\nTotal: {} services across {} datasets",
                total.total_services,
                total.datasets.len()
            );
            println!(
                "By source: {}",
                serde_json::to_string(&total.by_source).unwrap_or_default()
            );
            return;
        }
        use clap::CommandFactory;
        Cli::command().print_help().ok();
        println!();
        std::process::exit(1);
    };
    if let Err(e) = dispatch(&service, command, cli.json_output).await {
        fail(e);
    }
}
