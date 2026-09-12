// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Transport emitters, the port of `src/transport/transport_emitter.h` and
//! `stream_server_emitter.*`.

use std::sync::atomic::{AtomicBool, Ordering};

use ap_jsonrpc::sse::SseEvent;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::a2a_log;
use crate::log::A2aLogLevel;

/// Writes response data back to the transport.
pub trait TransportEmitter: Send + Sync {
    /// Write one streaming (SSE) data event.
    fn write_streaming_data(&self, data: &str);
    /// Write the single non-streaming response body. Later calls are ignored.
    fn write_non_streaming_data(&self, data: &str);
    /// End the stream.
    fn write_done(&self);
}

/// Emitter backed by channels feeding an HTTP response.
///
/// Streaming data is formatted as SSE (`data: <json>` followed by a blank
/// line) and sent through an unbounded channel. The non-streaming body is
/// sent once through a oneshot channel.
#[derive(Default)]
pub struct StreamServerEmitter {
    streaming: Mutex<Option<mpsc::UnboundedSender<String>>>,
    non_streaming: Mutex<Option<oneshot::Sender<String>>>,
    response_sent: AtomicBool,
}

impl StreamServerEmitter {
    /// Emitter for a streaming response.
    pub fn streaming(sender: mpsc::UnboundedSender<String>) -> Self {
        StreamServerEmitter {
            streaming: Mutex::new(Some(sender)),
            ..Default::default()
        }
    }

    /// Emitter for a non-streaming response.
    pub fn non_streaming(sender: oneshot::Sender<String>) -> Self {
        StreamServerEmitter {
            non_streaming: Mutex::new(Some(sender)),
            ..Default::default()
        }
    }

    /// Whether the non-streaming response was already written.
    pub fn response_sent(&self) -> bool {
        self.response_sent.load(Ordering::SeqCst)
    }

    /// Whether the stream is still open.
    pub fn is_open(&self) -> bool {
        self.streaming.lock().is_some()
    }
}

impl TransportEmitter for StreamServerEmitter {
    fn write_streaming_data(&self, data: &str) {
        let guard = self.streaming.lock();
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(SseEvent::data(data).to_wire());
        }
    }

    fn write_non_streaming_data(&self, data: &str) {
        if self.response_sent.swap(true, Ordering::SeqCst) {
            a2a_log!(A2aLogLevel::Warn, "Non-streaming data already written");
            return;
        }
        if let Some(tx) = self.non_streaming.lock().take() {
            let _ = tx.send(data.to_string());
        }
    }

    fn write_done(&self) {
        self.streaming.lock().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[tokio::test]
    async fn streaming_writes_sse_and_done_closes() -> TestResult {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let e = StreamServerEmitter::streaming(tx);
        e.write_streaming_data("{\"a\":1}");
        assert_eq!(rx.recv().await.required()?, "data: {\"a\":1}\n\n");
        assert!(e.is_open());
        e.write_done();
        assert!(!e.is_open());
        assert!(rx.recv().await.is_none());
        e.write_streaming_data("ignored");
        Ok(())
    }

    #[tokio::test]
    async fn non_streaming_writes_once() -> TestResult {
        let (tx, rx) = oneshot::channel();
        let e = StreamServerEmitter::non_streaming(tx);
        assert!(!e.response_sent());
        e.write_non_streaming_data("first");
        e.write_non_streaming_data("second");
        assert!(e.response_sent());
        assert_eq!(rx.await?, "first");
        Ok(())
    }

    #[test]
    fn default_emitter_does_nothing() -> TestResult {
        let e = StreamServerEmitter::default();
        e.write_streaming_data("x");
        e.write_non_streaming_data("y");
        e.write_done();
        Ok(())
    }
}
