// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Note tool logic shared by the note tool server, the agent simulator and the smoke test.
//!
//! This is the Rust equivalent of `examples/note_mcp_a4p/note_mcp_server.py`:
//! `list_notes`, `get_note` and `add_note` need no authorization, while
//! `delete_note` requires an Operation Mandate or an Intent Token.
use std::collections::HashMap;

use a4p::{A4PClient, JsonDict};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Agent id used by the demo.
pub const AGENT_ID: &str = "demo-agent";
/// User id used by the demo.
pub const USER_ID: &str = "demo-user";

#[derive(Debug, Clone)]
struct Note {
    id: String,
    title: String,
    body: String,
}

impl Note {
    fn summary(&self) -> Value {
        json!({"id": self.id, "title": self.title})
    }

    fn full(&self) -> Value {
        json!({"id": self.id, "title": self.title, "body": self.body})
    }
}

#[derive(Debug)]
struct State {
    notes: Vec<Note>,
    next_note_id: u64,
    operation_results: HashMap<String, Value>,
}

/// The in-memory note store protected by A4P.
pub struct NoteService {
    state: Mutex<State>,
    a4p_base_url: Option<String>,
}

impl NoteService {
    /// Create the service with the three demo notes.
    ///
    /// `a4p_base_url` overrides `A4P_SERVER_BASE_URL` when given.
    pub fn new(a4p_base_url: Option<String>) -> Self {
        Self {
            state: Mutex::new(State {
                notes: vec![
                    Note {
                        id: "note-1".into(),
                        title: "Project checklist".into(),
                        body: "Ship the A4P package layout and examples.".into(),
                    },
                    Note {
                        id: "note-2".into(),
                        title: "Meeting notes".into(),
                        body: "Deletion should require explicit user authorization.".into(),
                    },
                    Note {
                        id: "note-3".into(),
                        title: "Draft".into(),
                        body: "This note exists so the simulated agent has another target.".into(),
                    },
                ],
                next_note_id: 4,
                operation_results: HashMap::new(),
            }),
            a4p_base_url,
        }
    }

    /// Replace the notes with the two smoke-test notes and clear cached results.
    pub fn reset_for_smoke_test(&self) {
        let mut state = self.state.lock();
        state.notes = vec![
            Note {
                id: "note-1".into(),
                title: "One".into(),
                body: "First".into(),
            },
            Note {
                id: "note-2".into(),
                title: "Two".into(),
                body: "Second".into(),
            },
        ];
        state.next_note_id = 3;
        state.operation_results.clear();
    }

    /// True when a note with this id exists.
    pub fn has_note(&self, note_id: &str) -> bool {
        self.state.lock().notes.iter().any(|note| note.id == note_id)
    }

    fn client(&self) -> A4PClient {
        A4PClient::with_options(self.a4p_base_url.as_deref(), None)
    }

    fn delete_operation(note_id: &str) -> Value {
        json!({"action": "delete_note", "params": {"note_id": note_id}})
    }

    /// List note ids and titles.
    pub fn list_notes(&self) -> Value {
        Value::Array(self.state.lock().notes.iter().map(Note::summary).collect())
    }

    /// Return one note by id.
    pub fn get_note(&self, note_id: &str) -> Value {
        match self.state.lock().notes.iter().find(|note| note.id == note_id) {
            Some(note) => json!({"found": true, "note": note.full()}),
            None => json!({"found": false, "error": format!("note not found: {note_id}")}),
        }
    }

    /// Add a note and return the created entry.
    pub fn add_note(&self, title: &str, body: &str) -> Value {
        let mut state = self.state.lock();
        let note = Note {
            id: format!("note-{}", state.next_note_id),
            title: title.into(),
            body: body.into(),
        };
        state.next_note_id += 1;
        let created = note.full();
        state.notes.push(note);
        json!({"created": true, "note": created})
    }

    /// Delete a note after A4P authorization.
    ///
    /// Without authorization material the first call returns
    /// `authorization_required` with the mandate and signing options.
    pub async fn delete_note(
        &self,
        note_id: &str,
        operation_authorization: Option<&JsonDict>,
        intent_token: Option<&JsonDict>,
    ) -> Value {
        let note = self
            .state
            .lock()
            .notes
            .iter()
            .find(|note| note.id == note_id)
            .cloned();
        let Some(note) = note else {
            return json!({"deleted": false, "error": format!("note not found: {note_id}")});
        };

        let mut operation_id: Option<String> = None;
        let (authorized, reason) = if let Some(operation_authorization) = operation_authorization {
            let Some(signed_mandate) = operation_authorization
                .get("signedMandate")
                .and_then(Value::as_object)
            else {
                return json!({
                    "deleted": false,
                    "error": "A4P authorization failed: signedMandate missing",
                    "note": note.summary(),
                });
            };
            let request = json!({
                "signedMandate": signed_mandate,
                "operation": Self::delete_operation(note_id),
            });
            match self.client().complete_operation_authorization(request).await {
                Ok(completed) => {
                    let authorized = completed.approved && completed.operation_id.is_some();
                    operation_id = completed.operation_id;
                    (authorized, completed.reject_reason.unwrap_or_default())
                }
                Err(error) => (false, error.to_string()),
            }
        } else if let Some(intent_token) = intent_token {
            let request = json!({
                "token": intent_token,
                "expected": {
                    "action": "delete_note",
                    "params": {"note_id": note_id},
                    "agentId": format!("agent:{AGENT_ID}"),
                    "userId": USER_ID,
                },
            });
            match self.client().verify_intent_token(request).await {
                Ok(response) => (response.valid, response.reason.unwrap_or_default()),
                Err(error) => (false, error.to_string()),
            }
        } else {
            let request = json!({
                "agentId": AGENT_ID,
                "userId": USER_ID,
                "operation": Self::delete_operation(note_id),
                "validitySeconds": 300,
                "metadata": {"noteTitle": note.title},
            });
            let challenge = match self.client().prepare_operation_authorization(request).await {
                Ok(challenge) => challenge,
                Err(error) => {
                    return json!({
                        "deleted": false,
                        "error": format!("A4P authorization preparation failed: {error}"),
                        "note": note.summary(),
                    });
                }
            };
            let Some(mandate) = challenge.mandate else {
                return json!({
                    "deleted": false,
                    "error": format!(
                        "A4P authorization preparation failed: {}",
                        challenge.reject_reason.unwrap_or_default()
                    ),
                    "note": note.summary(),
                });
            };
            return json!({
                "deleted": false,
                "status": "authorization_required",
                "authorization": {"mandate": mandate, "signingOptions": challenge.signing_options},
                "note": note.summary(),
            });
        };

        if !authorized {
            return json!({
                "deleted": false,
                "error": format!("A4P authorization failed: {reason}"),
                "note": note.summary(),
            });
        }

        let mut state = self.state.lock();
        if let Some(operation_id) = &operation_id {
            if let Some(cached) = state.operation_results.get(operation_id) {
                return cached.clone();
            }
        }
        state.notes.retain(|item| item.id != note_id);
        let remaining: Vec<Value> = state.notes.iter().map(Note::summary).collect();
        let mut result = json!({"deleted": true, "note": note.summary(), "remaining": remaining});
        if let Some(operation_id) = operation_id {
            result["operationId"] = Value::String(operation_id.clone());
            state.operation_results.insert(operation_id, result.clone());
        }
        result
    }

    /// Dispatch a tool call by name with JSON arguments, like an MCP `tools/call`.
    pub async fn call_tool(&self, name: &str, arguments: &JsonDict) -> Result<Value, String> {
        let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::to_string);
        match name {
            "list_notes" => Ok(self.list_notes()),
            "get_note" => Ok(self.get_note(&text("note_id").ok_or("note_id missing")?)),
            "add_note" => Ok(self.add_note(
                &text("title").ok_or("title missing")?,
                &text("body").ok_or("body missing")?,
            )),
            "delete_note" => Ok(self
                .delete_note(
                    &text("note_id").ok_or("note_id missing")?,
                    arguments
                        .get("operation_authorization")
                        .and_then(Value::as_object),
                    arguments.get("intent_token").and_then(Value::as_object),
                )
                .await),
            other => Err(format!("unknown tool: {other}")),
        }
    }
}
