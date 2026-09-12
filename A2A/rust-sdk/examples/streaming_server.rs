// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Streaming orchestrator A2A server, the port of `examples/streaming_server.cpp`.
//!
//! Run: `cargo run -p a2a-sdk --example streaming_server -- -i 127.0.0.1 -p 8080`

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use a2a_sdk::A2aServerError;
use a2a_sdk::server::{
    AgentExecutor, HttpConfig, HttpServerBuilder, RequestContext, TaskArtifactParam, TaskUpdater,
};
use a2a_sdk::types::{AgentCapabilities, AgentCard, AgentInterface, Message, Part, Role};
use async_trait::async_trait;
use clap::Parser;
use serde_json::{Value, json};

/// Command line options.
#[derive(Parser, Debug)]
#[command(about = "A2A streaming orchestrator server")]
struct Args {
    /// Server IP address to bind to
    #[arg(short = 'i', long = "ip")]
    ip: String,
    /// Server port to listen on (1-65535)
    #[arg(short = 'p', long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

/// Executor that streams flight and weather artifacts, then completes.
#[derive(Default)]
struct OrchestratorAgentExecutor {
    counter: AtomicU64,
}

impl OrchestratorAgentExecutor {
    fn new_agent_parts_message(&self, parts: Vec<Part>) -> Message {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        Message {
            role: Role::Agent,
            parts,
            message_id: format!("message-{n}"),
            ..Default::default()
        }
    }

    fn make_combined_part(&self, tag: &str, payload: Value) -> Message {
        self.new_agent_parts_message(vec![
            Part::data(json!({"tag": tag, "payload": payload})).with_media_type("application/json"),
        ])
    }

    fn make_error_part(&self, msg: &str) -> Message {
        self.new_agent_parts_message(vec![
            Part::data(json!({"error": msg})).with_media_type("application/json"),
        ])
    }

    fn destination_and_date(input: &Message) -> (String, String) {
        let mut destination = "Unknown".to_string();
        let mut date = "anytime".to_string();
        for part in &input.parts {
            let Some(data) = &part.data else {
                continue;
            };
            let parsed: Option<Value> = match data {
                Value::String(s) => serde_json::from_str(s).ok(),
                other => Some(other.clone()),
            };
            if let Some(j) = parsed {
                if let Some(d) = j.get("destination").and_then(Value::as_str) {
                    destination = d.to_string();
                }
                if let Some(d) = j.get("date").and_then(Value::as_str) {
                    date = d.to_string();
                }
            }
        }
        (destination, date)
    }

    fn make_combined_message(&self, input: &Message) -> Message {
        let (destination, date) = Self::destination_and_date(input);
        let combined = json!({
            "action": "tripPlan", "destination": destination, "date": date, "summary": "Trip planned."
        });
        self.new_agent_parts_message(vec![Part::data(combined).with_media_type("application/json")])
    }
}

#[async_trait]
impl AgentExecutor for OrchestratorAgentExecutor {
    async fn execute(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        println!("OrchestratorAgentExecutor::execute() called");
        let Some(req) = context.get_message().cloned() else {
            task_updater.failed(Some(self.make_error_part("No message in request")));
            return Ok(());
        };
        let (destination, _date) = Self::destination_and_date(&req);

        task_updater.start_work(Some(
            self.make_combined_part("flight-progress", json!({"stage": "calling-flight"})),
        ));
        let flight_data = json!({
            "action": "flights/info", "airline": "Air France", "destination": destination, "price": "750 USD"
        });
        task_updater.add_artifact(&TaskArtifactParam {
            parts: vec![Part::text(flight_data.to_string()).with_media_type("text/plain")],
            ..Default::default()
        });

        task_updater.start_work(Some(
            self.make_combined_part("weather-progress", json!({"stage": "calling-weather"})),
        ));
        let weather_data = json!({
            "action": "weather/forecast", "city": destination, "temperature": "-2C", "conditions": "Snowy"
        });
        task_updater.add_artifact(&TaskArtifactParam {
            parts: vec![Part::text(weather_data.to_string()).with_media_type("text/plain")],
            ..Default::default()
        });

        task_updater.start_work(Some(
            self.make_combined_part("done", json!({"stage": "aggregate"})),
        ));
        task_updater.complete(Some(self.make_combined_message(&req)));
        Ok(())
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        println!("OrchestratorAgentExecutor::cancel() called");
        task_updater.cancel(None);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!(
        "Server configuration:\n  IP: {}\n  Port: {}\n",
        args.ip, args.port
    );

    let agent_card = AgentCard {
        name: "OrchestratorAgent".into(),
        description: "A2A Orchestrator Example".into(),
        version: "1.0.0".into(),
        default_input_modes: vec!["text".into()],
        default_output_modes: vec!["text".into()],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            ..Default::default()
        },
        supported_interfaces: vec![AgentInterface::jsonrpc(format!(
            "http://{}:{}/jsonrpc",
            args.ip, args.port
        ))],
        ..Default::default()
    };

    let config = HttpConfig::new(args.ip.clone(), args.port);
    let server = HttpServerBuilder::build(
        &config,
        &agent_card,
        &AgentCard::default(),
        Arc::new(OrchestratorAgentExecutor::default()),
        None,
    )?;

    println!("\nStarting A2A streaming server...");
    server.start().await?;
    println!("Streaming server running on {}:{}", args.ip, args.port);
    println!("Press Ctrl+C to stop");

    tokio::signal::ctrl_c().await?;
    println!("\nReceived signal. Shutting down...");
    server.stop().await;
    println!("Server stopped.");
    Ok(())
}
