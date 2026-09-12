// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Stdio transport, port of `src/transport/stdio_transport.*`.
//!
//! Messages are newline delimited JSON. The client spawns a child process
//! and talks to its stdin and stdout; the server reads its own stdin and
//! writes its own stdout. Nothing in this module ever logs to stdout.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use ap_jsonrpc::Message;
use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{CallbackHandle, ClientTransport, RequestContext, ServerTransport};
use crate::error::McpError;
use crate::protocol::{parse_message, serialize_message};
use crate::types::StdioClientConfig;

/// Boxed async reader.
pub type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;
/// Boxed async writer.
pub type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Split a chunk of received bytes into complete messages, keeping the
/// remainder in `buffer`. Empty lines are skipped and a trailing `\r` removed.
pub fn extract_lines(buffer: &mut String) -> Vec<String> {
    let mut messages = Vec::new();
    while let Some(pos) = buffer.find('\n') {
        let mut line: String = buffer[..pos].to_string();
        buffer.drain(..=pos);
        if line.ends_with('\r') {
            line.pop();
        }
        if !line.is_empty() {
            messages.push(line);
        }
    }
    messages
}

/// Frame a message for the wire: the compact JSON plus a newline.
pub fn frame_message(message: &Message) -> String {
    let mut data = serialize_message(message);
    data.push('\n');
    data
}

/// True when an environment key or value would corrupt the environment block.
pub fn contains_invalid_env_chars(value: &str) -> bool {
    value.contains('=') || value.contains('\n') || value.contains('\r')
}

struct Connection {
    writer: Arc<Mutex<BoxedWriter>>,
    reader_task: JoinHandle<()>,
    child: Option<Child>,
}

fn spawn_reader(
    reader: BoxedReader,
    callback: Arc<RwLock<Option<CallbackHandle>>>,
    name: &'static str,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let reason: String;
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line = line.trim_end_matches('\r');
                    if line.is_empty() {
                        continue;
                    }
                    tracing::debug!("{name} received message: {line}");
                    let cb = callback.read().as_ref().and_then(Weak::upgrade);
                    let Some(cb) = cb else {
                        continue;
                    };
                    match parse_message(line) {
                        Ok(message) => {
                            let ctx = RequestContext {
                                connection_id: 0,
                                ..Default::default()
                            };
                            // Responses only complete a pending request, so they are
                            // delivered inline. This keeps them ahead of a following EOF.
                            // Requests and notifications may run long handlers and are
                            // dispatched on their own task.
                            if matches!(message, Message::Response(_)) {
                                cb.on_message_received(message, ctx).await;
                            } else {
                                tokio::spawn(async move {
                                    cb.on_message_received(message, ctx).await;
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!("Invalid JSON-RPC message: {e}, Message: {line}");
                        }
                    }
                }
                Ok(None) => {
                    reason = "Connection closed by peer".to_string();
                    break;
                }
                Err(e) => {
                    reason = format!("Read error: {e}");
                    break;
                }
            }
        }
        tracing::info!("{name} disconnected: {reason}");
        let cb = callback.read().as_ref().and_then(Weak::upgrade);
        if let Some(cb) = cb {
            cb.on_disconnected(reason).await;
        }
    })
}

async fn write_framed(writer: &Arc<Mutex<BoxedWriter>>, message: &Message) -> Result<(), McpError> {
    let data = frame_message(message);
    tracing::debug!("stdio sending message: {}", data.trim_end());
    let mut guard = writer.lock().await;
    guard
        .write_all(data.as_bytes())
        .await
        .map_err(|e| McpError::Transport(format!("Failed to write message: {e}")))?;
    guard
        .flush()
        .await
        .map_err(|e| McpError::Transport(format!("Failed to flush message: {e}")))
}

async fn stop_connection(conn: Connection) {
    conn.reader_task.abort();
    // Closing stdin asks the child to exit.
    drop(conn.writer);
    if let Some(mut child) = conn.child {
        match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = child.kill().await;
            }
        }
        tracing::info!("Subprocess stopped");
    }
}

/// Client side stdio transport (`StdioClientTransport`).
pub struct StdioClientTransport {
    config: StdioClientConfig,
    callback: Arc<RwLock<Option<CallbackHandle>>>,
    running: AtomicBool,
    connection: Mutex<Option<Connection>>,
    preset: parking_lot::Mutex<Option<(BoxedReader, BoxedWriter)>>,
}

impl StdioClientTransport {
    /// Create a transport. With an empty `command` the process's own stdio is used.
    pub fn new(config: StdioClientConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            callback: Arc::new(RwLock::new(None)),
            running: AtomicBool::new(false),
            connection: Mutex::new(None),
            preset: parking_lot::Mutex::new(None),
        })
    }

    /// Create a transport over explicit streams. Useful for tests.
    pub fn with_streams<R, W>(reader: R, writer: W) -> Arc<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let transport = Self::new(StdioClientConfig::default());
        *transport.preset.lock() = Some((Box::new(reader), Box::new(writer)));
        transport
    }

    /// True after a successful [`ClientTransport::connect`].
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn spawn_child(&self) -> Result<(Child, BoxedReader, BoxedWriter), McpError> {
        let mut command = Command::new(&self.config.command);
        command.args(&self.config.args);
        for (k, v) in &self.config.env {
            if contains_invalid_env_chars(k) || contains_invalid_env_chars(v) {
                continue;
            }
            command.env(k, v);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command.kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| McpError::Transport(format!("Failed to start subprocess: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("Failed to open subprocess stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("Failed to open subprocess stdout".into()))?;
        tracing::info!("Subprocess started with PID: {:?}", child.id());
        Ok((child, Box::new(stdout), Box::new(stdin)))
    }
}

#[async_trait]
impl ClientTransport for StdioClientTransport {
    async fn connect(&self) -> Result<(), McpError> {
        let mut slot = self.connection.lock().await;
        if slot.is_some() {
            tracing::warn!("StdioClientTransport already connected");
            return Ok(());
        }
        let preset = self.preset.lock().take();
        let (child, reader, writer): (Option<Child>, BoxedReader, BoxedWriter) = if let Some((r, w)) = preset
        {
            (None, r, w)
        } else if !self.config.command.is_empty() {
            let (child, r, w) = self.spawn_child()?;
            (Some(child), r, w)
        } else {
            tracing::info!("Using direct stdio communication");
            (None, Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
        };
        let reader_task = spawn_reader(reader, self.callback.clone(), "StdioClientTransport");
        *slot = Some(Connection {
            writer: Arc::new(Mutex::new(writer)),
            reader_task,
            child,
        });
        self.running.store(true, Ordering::SeqCst);
        tracing::info!("Client StdioClientTransport connected successfully");
        Ok(())
    }

    async fn terminate(&self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        let conn = self.connection.lock().await.take();
        if let Some(conn) = conn {
            stop_connection(conn).await;
        }
        tracing::info!("ClientStdioTransport terminated");
    }

    async fn send_message(&self, message: Message) -> Result<(), McpError> {
        let writer = {
            let guard = self.connection.lock().await;
            guard.as_ref().map(|c| c.writer.clone())
        };
        let Some(writer) = writer else {
            return Err(McpError::Transport(
                "Cannot send message: no connection available".into(),
            ));
        };
        write_framed(&writer, &message).await
    }

    fn set_callback(&self, callback: Option<CallbackHandle>) {
        *self.callback.write() = callback;
    }
}

/// Server side stdio transport (`StdioServerTransport`).
pub struct StdioServerTransport {
    callback: Arc<RwLock<Option<CallbackHandle>>>,
    running: AtomicBool,
    connection: Mutex<Option<Connection>>,
    preset: parking_lot::Mutex<Option<(BoxedReader, BoxedWriter)>>,
}

impl Default for StdioServerTransport {
    fn default() -> Self {
        Self {
            callback: Arc::new(RwLock::new(None)),
            running: AtomicBool::new(false),
            connection: Mutex::new(None),
            preset: parking_lot::Mutex::new(None),
        }
    }
}

impl StdioServerTransport {
    /// Create a transport over the process's stdin and stdout.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Create a transport over explicit streams. Useful for tests.
    pub fn with_streams<R, W>(reader: R, writer: W) -> Arc<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let transport = Self::new();
        *transport.preset.lock() = Some((Box::new(reader), Box::new(writer)));
        transport
    }

    /// True after a successful [`ServerTransport::listen`].
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ServerTransport for StdioServerTransport {
    async fn listen(&self) -> Result<(), McpError> {
        let mut slot = self.connection.lock().await;
        if slot.is_some() {
            tracing::warn!("StdioServerTransport already listening");
            return Ok(());
        }
        let preset = self.preset.lock().take();
        let (reader, writer): (BoxedReader, BoxedWriter) = match preset {
            Some((r, w)) => (r, w),
            None => (Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout())),
        };
        let reader_task = spawn_reader(reader, self.callback.clone(), "StdioServerTransport");
        *slot = Some(Connection {
            writer: Arc::new(Mutex::new(writer)),
            reader_task,
            child: None,
        });
        self.running.store(true, Ordering::SeqCst);
        tracing::info!("StdioConnection started listening on stdin/stdout");
        Ok(())
    }

    async fn terminate(&self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        let conn = self.connection.lock().await.take();
        if let Some(conn) = conn {
            stop_connection(conn).await;
        }
        tracing::info!("ServerStdioTransport terminated");
    }

    async fn send_message(&self, message: Message, _ctx: &RequestContext) -> Result<(), McpError> {
        let writer = {
            let guard = self.connection.lock().await;
            guard.as_ref().map(|c| c.writer.clone())
        };
        let Some(writer) = writer else {
            return Err(McpError::Transport(
                "Cannot send message: no connection available".into(),
            ));
        };
        write_framed(&writer, &message).await
    }

    fn set_callback(&self, callback: Option<CallbackHandle>) {
        *self.callback.write() = callback;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportCallback;
    use ap_jsonrpc::{Notification, Request, RequestId};
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use std::sync::atomic::AtomicUsize;

    struct Collect {
        count: AtomicUsize,
        messages: parking_lot::Mutex<Vec<Message>>,
        disconnects: AtomicUsize,
    }

    #[async_trait]
    impl TransportCallback for Collect {
        async fn on_message_received(&self, message: Message, _ctx: RequestContext) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.messages.lock().push(message);
        }

        async fn on_disconnected(&self, _reason: String) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn collector() -> Arc<Collect> {
        Arc::new(Collect {
            count: AtomicUsize::new(0),
            messages: parking_lot::Mutex::new(Vec::new()),
            disconnects: AtomicUsize::new(0),
        })
    }

    async fn wait_for(f: impl Fn() -> bool) -> TestResult {
        for _ in 0..200 {
            if f() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(ap_support::testing::TestFailure::new("condition not met".to_string()).into())
    }

    #[test]
    fn line_extraction_and_framing() -> TestResult {
        let mut buffer = String::from("{\"a\":1}\r\n\n{\"b\":2}\n{\"partial\"");
        let lines = extract_lines(&mut buffer);
        assert_eq!(lines, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
        assert_eq!(buffer, "{\"partial\"");
        buffer.push_str(":3}\n");
        assert_eq!(extract_lines(&mut buffer), vec!["{\"partial\":3}".to_string()]);
        assert!(buffer.is_empty());
        let framed = frame_message(&Message::Request(Request::new(1, "ping", None)));
        assert!(framed.ends_with('\n'));
        assert_eq!(framed.matches('\n').count(), 1);
        assert!(contains_invalid_env_chars("a=b"));
        assert!(contains_invalid_env_chars("a\nb"));
        assert!(!contains_invalid_env_chars("ok"));
        Ok(())
    }

    #[tokio::test]
    async fn client_and_server_exchange_over_duplex() -> TestResult {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, server_write) = tokio::io::split(server_side);
        let client = StdioClientTransport::with_streams(client_read, client_write);
        let server = StdioServerTransport::with_streams(server_read, server_write);
        let client_cb = collector();
        let server_cb = collector();
        client.set_callback(Some(Arc::downgrade(&client_cb) as CallbackHandle));
        server.set_callback(Some(Arc::downgrade(&server_cb) as CallbackHandle));

        assert!(
            client
                .send_message(Message::Request(Request::new(1, "ping", None)))
                .await
                .is_err()
        );
        client.connect().await?;
        client.connect().await?;
        server.listen().await?;
        server.listen().await?;
        assert!(client.is_running());
        assert!(server.is_running());

        client
            .send_message(Message::Request(Request::new(1, "ping", None)))
            .await?;
        wait_for(|| server_cb.count.load(Ordering::SeqCst) == 1).await?;
        assert_eq!(server_cb.messages.lock()[0].method(), Some("ping"));

        let ctx = RequestContext::default();
        server
            .send_message(
                Message::Notification(Notification::new("notifications/message", None)),
                &ctx,
            )
            .await?;
        server
            .send_message(
                Message::Response(ap_jsonrpc::Response::success(
                    RequestId::Number(1),
                    serde_json::json!({}),
                )),
                &ctx,
            )
            .await?;
        wait_for(|| client_cb.count.load(Ordering::SeqCst) == 2).await?;

        client.terminate().await;
        client.terminate().await;
        assert!(!client.is_running());
        wait_for(|| server_cb.disconnects.load(Ordering::SeqCst) == 1).await?;
        server.terminate().await;
        server.terminate().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_lines_are_ignored() -> TestResult {
        let (client_side, server_side) = tokio::io::duplex(4096);
        let (server_read, server_write) = tokio::io::split(server_side);
        let server = StdioServerTransport::with_streams(server_read, server_write);
        let cb = collector();
        server.set_callback(Some(Arc::downgrade(&cb) as CallbackHandle));
        server.listen().await?;
        let (_, mut w) = tokio::io::split(client_side);
        w.write_all(b"not json\n\r\n{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n")
            .await?;
        w.flush().await?;
        wait_for(|| cb.count.load(Ordering::SeqCst) == 1).await?;
        server.terminate().await;
        Ok(())
    }

    #[tokio::test]
    async fn spawns_subprocess_with_env_and_args() -> TestResult {
        let mut env = std::collections::HashMap::new();
        env.insert("MCP_TEST_VALUE".to_string(), "hello".to_string());
        env.insert("BAD=KEY".to_string(), "x".to_string());
        let config = StdioClientConfig {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "read line; printf '{\"jsonrpc\":\"2.0\",\"method\":\"echo\",\"params\":{\"env\":\"%s\",\"arg\":\"%s\"}}\\n' \"$MCP_TEST_VALUE\" \"$1\"".into(),
                "sh".into(),
                "arg1".into(),
            ],
            env,
        };
        let client = StdioClientTransport::new(config);
        let cb = collector();
        client.set_callback(Some(Arc::downgrade(&cb) as CallbackHandle));
        client.connect().await?;
        client
            .send_message(Message::Request(Request::new(1, "ping", None)))
            .await?;
        wait_for(|| cb.count.load(Ordering::SeqCst) == 1).await?;
        let msg = cb.messages.lock()[0].clone();
        match msg {
            Message::Notification(n) => {
                let params = n.params.required()?;
                assert_eq!(params["env"], "hello");
                assert_eq!(params["arg"], "arg1");
            }
            _ => {
                return Err(
                    ap_support::testing::TestFailure::new("expected notification".to_string()).into(),
                );
            }
        }
        wait_for(|| cb.disconnects.load(Ordering::SeqCst) == 1).await?;
        client.terminate().await;
        Ok(())
    }

    #[tokio::test]
    async fn missing_command_fails_to_connect() -> TestResult {
        let client = StdioClientTransport::new(StdioClientConfig {
            command: "/definitely/not/a/command".into(),
            ..Default::default()
        });
        let err = client.connect().await.err_or_fail()?;
        assert!(matches!(err, McpError::Transport(_)));
        assert!(!client.is_running());
        Ok(())
    }
}
