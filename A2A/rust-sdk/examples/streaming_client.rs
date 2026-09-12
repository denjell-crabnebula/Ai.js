// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Streaming A2A client, the port of `examples/streaming_client.cpp`.
//!
//! Run: `cargo run -p a2a-sdk --example streaming_client -- -i 127.0.0.1 -p 8080`

use std::sync::Arc;

use a2a_sdk::client::{ClientConfig, ClientEvent, ClientFactory, UpdateEvent};
use a2a_sdk::types::{AgentCapabilities, AgentCard, AgentInterface, ClientCallContext, Message, Part, Role};
use clap::Parser;
use serde_json::json;

/// Command line options.
#[derive(Parser, Debug)]
#[command(about = "A2A streaming client")]
struct Args {
    /// Server IP address to connect to
    #[arg(short = 'i', long = "ip")]
    ip: String,
    /// Server port to connect to (1-65535)
    #[arg(short = 'p', long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

fn print_event(ev: &ClientEvent) {
    match ev {
        ClientEvent::Message(m) => {
            println!(
                "<-- Response:\n<---- Role: {:?}\n<---- messageId: {}",
                m.role, m.message_id
            );
        }
        ClientEvent::Error(e) => {
            println!(
                "<-- Error:\n<---- code: {}\n<---- message: {}",
                e.code,
                e.message.as_deref().unwrap_or("")
            );
        }
        ClientEvent::TaskUpdate(t, upd) => {
            println!("<-- Task:\n<---- contextId: {}\n<---- id: {}", t.context_id, t.id);
            match upd {
                UpdateEvent::Status(u) => {
                    println!(
                        "<-- Status:\n<---- status: {:?}\n<---- taskId: {}",
                        u.status.state, u.task_id
                    );
                }
                UpdateEvent::Artifact(u) => {
                    println!(
                        "<-- Artifact:\n<---- contextId: {}\n<---- taskId: {}\n<---- artifactId: {}",
                        u.context_id, u.task_id, u.artifact.artifact_id
                    );
                }
                UpdateEvent::None => {}
            }
            println!("\n");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!(
        "Client configuration:\n  Connecting to: {}:{}\n",
        args.ip, args.port
    );
    println!("--- Streaming Client Demo ---");
    let session_id = "demo-session";

    println!("[1] Configuring and creating client factory...");
    let card = AgentCard {
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
    let cfg = ClientConfig {
        streaming: true,
        supported_transports: vec!["JSONRPC".into()],
        ..Default::default()
    };
    let client =
        ClientFactory::create(&card, &cfg, Vec::new(), Vec::new()).ok_or("Failed to create client")?;
    println!("Client created for agent: {}", card.name);

    println!("[2] Adding a global event consumer...");
    client.add_event_consumer(Arc::new(|ev: &ClientEvent, card: &AgentCard| {
        println!("[consumer] Event for {}:", card.name);
        print_event(ev);
    }));

    println!("[3] Building request message...");
    let msg = Message {
        role: Role::User,
        message_id: "77777".into(),
        parts: vec![
            Part::data(json!({"action": "planTrip", "destination": "Paris", "date": "2025-12-25"}))
                .with_media_type("application/json"),
        ],
        ..Default::default()
    };

    println!("[4] Preparing call context with sessionId...");
    let ctx = ClientCallContext {
        state: json!({"sessionId": session_id}).to_string(),
        headers: String::new(),
    };

    println!("\n--> [5] Sending streaming request to Orchestrator...");
    let result = client
        .send_message(
            &msg,
            Some(&ctx),
            Box::new(|ev: &ClientEvent, _card: &AgentCard| {
                println!("[callback] Received event:");
                print_event(ev);
            }),
            0,
        )
        .await;
    if let Err(e) = result {
        eprintln!("SendMessage failed: {e}");
    }
    println!("\nDone.");
    client.close();
    Ok(())
}
