// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Hello world A2A client, the port of `examples/helloworld_client.cpp`.
//!
//! Run: `cargo run -p a2a-sdk --example helloworld_client -- -i 127.0.0.1 -p 8080`

use std::collections::BTreeMap;

use a2a_sdk::client::{ClientConfig, ClientEvent, ClientFactory, HttpCardResolverBuilder};
use a2a_sdk::types::{AgentCapabilities, AgentCard, AgentInterface, Message, Part, Role};
use clap::Parser;

/// Command line options.
#[derive(Parser, Debug)]
#[command(about = "A2A Hello World client")]
struct Args {
    /// Server IP address to connect to
    #[arg(short = 'i', long = "ip")]
    ip: String,
    /// Server port to connect to (1-65535)
    #[arg(short = 'p', long = "port", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!(
        "Client configuration:\n  Connecting to: {}:{}\n",
        args.ip, args.port
    );
    let base_url = format!("http://{}:{}", args.ip, args.port);

    let card = AgentCard {
        name: "ExampleAgent".into(),
        description: "A2A Hello World Example".into(),
        version: "1.0.0".into(),
        default_input_modes: vec!["text".into()],
        default_output_modes: vec!["text".into()],
        capabilities: AgentCapabilities {
            streaming: Some(false),
            ..Default::default()
        },
        supported_interfaces: vec![AgentInterface::jsonrpc(format!("{base_url}/jsonrpc"))],
        ..Default::default()
    };

    let resolver =
        HttpCardResolverBuilder::build(&base_url, "/.well-known/agent-card.json", &BTreeMap::new())
            .ok_or("failed to build resolver")?;
    println!("Test: GetAgentCard");
    match resolver.get_agent_card(None).await {
        Ok(remote) => {
            println!("Success: {}", remote.name);
            println!(
                "AgentCard details:\n  Name: {}\n  Description: {}\n  URL: {}\n  Version: {}",
                remote.name,
                remote.description,
                remote
                    .supported_interfaces
                    .first()
                    .map(|i| i.url.as_str())
                    .unwrap_or(""),
                remote.version
            );
        }
        Err(e) => {
            eprintln!("Failed: {e}");
            std::process::exit(1);
        }
    }

    let cfg = ClientConfig {
        streaming: false,
        supported_transports: vec!["JSONRPC".into()],
        ..Default::default()
    };
    let client =
        ClientFactory::create(&card, &cfg, Vec::new(), Vec::new()).ok_or("Failed to create client")?;

    let msg = Message {
        role: Role::User,
        message_id: "123".into(),
        parts: vec![
            Part::text("hello remote server").with_media_type("text/plain"),
            Part::data(serde_json::json!({"key": "value", "number": 1})).with_media_type("application/json"),
        ],
        ..Default::default()
    };

    println!("--> Sending to {}", card.supported_interfaces[0].url);
    let result = client
        .send_message(
            &msg,
            None,
            Box::new(|ev: &ClientEvent, _card: &AgentCard| match ev {
                ClientEvent::Message(m) => {
                    println!(
                        "<-- Response:\n<---- Role: {:?}\n<---- messageId: {}",
                        m.role, m.message_id
                    );
                    for p in &m.parts {
                        if let Some(t) = &p.text {
                            println!("<---- text: {t}");
                        }
                    }
                }
                ClientEvent::Error(e) => {
                    println!(
                        "<-- Error:\n<---- code: {}\n<---- message: {}",
                        e.code,
                        e.message.as_deref().unwrap_or("")
                    );
                }
                ClientEvent::TaskUpdate(t, _) => {
                    println!(
                        "<-- Task:\n<---- contextId: {}\n<---- id: {}\n",
                        t.context_id, t.id
                    );
                }
            }),
            0,
        )
        .await;
    if let Err(e) = result {
        eprintln!("SendMessage failed: {e}");
    }
    client.close();
    Ok(())
}
