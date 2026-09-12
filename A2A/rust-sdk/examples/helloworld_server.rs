// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Hello world A2A server, the port of `examples/helloworld_server.cpp`.
//!
//! Run: `cargo run -p a2a-sdk --example helloworld_server -- -i 127.0.0.1 -p 8080`

use std::sync::Arc;
use std::time::Duration;

use a2a_sdk::A2aServerError;
use a2a_sdk::server::{AgentExecutor, HttpConfig, HttpServerBuilder, RequestContext, TaskUpdater};
use a2a_sdk::types::{AgentCapabilities, AgentCard, AgentInterface, Message, Part, Role};
use async_trait::async_trait;
use clap::Parser;

const SIMULATED_SERVER_LATENCY_MS: u64 = 300;

/// Command line options.
#[derive(Parser, Debug)]
#[command(about = "A2A Hello World server")]
struct Args {
    /// Server IP address to bind to
    #[arg(short = 'i', long = "ip")]
    ip: String,
    /// Server port to listen on (1-65535)
    #[arg(short = 'p', long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

/// Minimal executor answering with `Processed: <input>`.
struct MyAgentExecutor;

#[async_trait]
impl AgentExecutor for MyAgentExecutor {
    async fn execute(
        &self,
        context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        println!("AgentExecutor::execute() called");
        task_updater.start_work(None);
        tokio::time::sleep(Duration::from_millis(SIMULATED_SERVER_LATENCY_MS)).await;

        let mut response = Message {
            role: Role::Agent,
            ..Default::default()
        };
        let mut user_input = "Hello World!".to_string();
        if let Some(msg) = context.get_message() {
            response.message_id = msg.message_id.clone();
            response.context_id = Some(msg.context_id.clone().unwrap_or_default());
            if let Some(text) = msg.parts.iter().find_map(|p| p.text.clone()) {
                user_input = text;
            }
        }
        response
            .parts
            .push(Part::text(format!("Processed: {user_input}")).with_media_type("text/plain"));
        task_updater.send_response_message(&response);
        Ok(())
    }

    async fn cancel(
        &self,
        _context: Arc<RequestContext>,
        task_updater: Arc<dyn TaskUpdater>,
    ) -> Result<(), A2aServerError> {
        println!("AgentExecutor::cancel() called");
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
        name: "ExampleAgent".into(),
        description: "A2A Hello World Example".into(),
        version: "1.0.0".into(),
        default_input_modes: vec!["text".into()],
        default_output_modes: vec!["text".into()],
        capabilities: AgentCapabilities {
            streaming: Some(false),
            ..Default::default()
        },
        supported_interfaces: vec![AgentInterface::jsonrpc(format!(
            "http://{}:{}/jsonrpc",
            args.ip, args.port
        ))],
        ..Default::default()
    };
    println!("--Built agent card");

    let config = HttpConfig::new(args.ip.clone(), args.port);
    let server = HttpServerBuilder::build(
        &config,
        &agent_card,
        &AgentCard::default(),
        Arc::new(MyAgentExecutor),
        None,
    )?;

    println!("\nStarting A2A Hello World server...");
    server.start().await?;
    println!("Hello World server running on {}:{}", args.ip, args.port);
    println!("Press Ctrl+C to stop");

    tokio::signal::ctrl_c().await?;
    println!("\nReceived signal. Shutting down...");
    server.stop().await;
    println!("Server stopped.");
    Ok(())
}
