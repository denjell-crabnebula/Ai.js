// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! A2A protocol data types and their JSON wire format.
//!
//! This module ports `include/types.h` together with the serializers of
//! `src/shared/jsonrpc.h`. Field names, enum strings and validation rules
//! follow the C++ SDK so that both implementations interoperate.
//!
//! Metadata fields, `Part::data`, `AgentExtension::params` and
//! `AgentCardSignature::header` are kept as [`serde_json::Value`] instead of
//! JSON text. On the wire they are identical to the C++ SDK.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::protocol::{
    JSON_FIELD_ID, JSON_FIELD_MESSAGE, JSON_FIELD_METADATA, STREAM_RESPONSE_TYPE_TASK,
    STREAM_RESULT_KEY_ARTIFACT_UPDATE, STREAM_RESULT_KEY_STATUS_UPDATE,
};

/// Result of a manual JSON conversion.
pub type JsonResult<T> = Result<T, String>;

/// Manual conversion to and from the wire JSON, mirroring `adl_serializer`.
pub trait WireJson: Sized {
    /// Convert to the wire JSON value.
    fn to_json_value(&self) -> JsonResult<Value>;
    /// Build from a wire JSON value.
    fn from_json_value(value: &Value) -> JsonResult<Self>;
}

macro_rules! impl_wire_serde {
    ($t:ty) => {
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let value = self.to_json_value().map_err(serde::ser::Error::custom)?;
                value.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = Value::deserialize(deserializer)?;
                Self::from_json_value(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn as_object<'a>(value: &'a Value, type_name: &str) -> JsonResult<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("{type_name} must be a JSON object"))
}

fn get_string(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<String> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("{type_name}.{key} must be a string")),
        None => Err(format!("{type_name} must contain '{key}' field")),
    }
}

fn opt_string(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Option<String>> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("{type_name}.{key} must be a string")),
    }
}

fn opt_string_nullable(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Option<String>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("{type_name}.{key} must be a string")),
    }
}

fn opt_bool(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Option<bool>> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{type_name}.{key} must be a boolean")),
    }
}

fn opt_i32(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Option<i32>> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .map(|n| Some(n as i32))
            .ok_or_else(|| format!("{type_name}.{key} must be an integer")),
    }
}

fn string_vec(value: &Value, what: &str) -> JsonResult<Vec<String>> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array"))?;
    items
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{what} must contain only strings"))
        })
        .collect()
}

fn get_string_vec(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Vec<String>> {
    match obj.get(key) {
        Some(v) => string_vec(v, &format!("{type_name}.{key}")),
        None => Err(format!("{type_name} must contain '{key}' field")),
    }
}

fn opt_string_vec(obj: &Map<String, Value>, key: &str, type_name: &str) -> JsonResult<Option<Vec<String>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => string_vec(v, &format!("{type_name}.{key}")).map(Some),
    }
}

fn opt_metadata(obj: &Map<String, Value>) -> Option<Value> {
    match obj.get(JSON_FIELD_METADATA) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
    }
}

fn vec_of<T: WireJson>(value: &Value, what: &str) -> JsonResult<Vec<T>> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{what} must be an array"))?;
    items.iter().map(T::from_json_value).collect()
}

fn vec_to_json<T: WireJson>(items: &[T]) -> JsonResult<Value> {
    items
        .iter()
        .map(T::to_json_value)
        .collect::<JsonResult<Vec<Value>>>()
        .map(Value::Array)
}

fn string_vec_json(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())
}

fn insert(obj: &mut Map<String, Value>, key: &str, value: Value) {
    obj.insert(key.to_string(), value);
}

fn insert_str(obj: &mut Map<String, Value>, key: &str, value: &str) {
    obj.insert(key.to_string(), Value::String(value.to_string()));
}

fn insert_opt_str(obj: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(v) = value {
        insert_str(obj, key, v);
    }
}

fn insert_opt_bool(obj: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if let Some(v) = value {
        obj.insert(key.to_string(), Value::Bool(v));
    }
}

fn insert_opt_metadata(obj: &mut Map<String, Value>, value: &Option<Value>) {
    if let Some(v) = value {
        obj.insert(JSON_FIELD_METADATA.to_string(), v.clone());
    }
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Role of a message sender in a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// The agent.
    #[serde(rename = "ROLE_AGENT")]
    Agent,
    /// The user.
    #[serde(rename = "ROLE_USER")]
    User,
    /// Unspecified role.
    #[serde(rename = "ROLE_UNSPECIFIED")]
    Unspecified,
}

impl Role {
    fn from_wire(s: &str) -> JsonResult<Role> {
        match s {
            "ROLE_AGENT" => Ok(Role::Agent),
            "ROLE_USER" => Ok(Role::User),
            "ROLE_UNSPECIFIED" => Ok(Role::Unspecified),
            other => Err(format!("Invalid Role value: {other}")),
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Role::Agent => "ROLE_AGENT",
            Role::User => "ROLE_USER",
            Role::Unspecified => "ROLE_UNSPECIFIED",
        }
    }
}

/// Per-request client-side call context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientCallContext {
    /// Arbitrary per-request state.
    pub state: String,
    /// Serialized request headers.
    pub headers: String,
}

/// A single content part within a message or artifact.
///
/// Exactly one of `text`, `raw`, `url` and `data` is written to the wire.
/// The first one set wins, in that order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// Plain text payload.
    pub text: Option<String>,
    /// Raw (base64) payload.
    pub raw: Option<String>,
    /// URL payload.
    pub url: Option<String>,
    /// Structured data payload (object, string, number or bool).
    pub data: Option<Value>,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Optional file name.
    pub filename: Option<String>,
    /// Optional media type.
    pub media_type: Option<String>,
}

impl Part {
    /// Build a text part with the given media type.
    pub fn text(text: impl Into<String>) -> Self {
        Part {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    /// Build a data part.
    pub fn data(data: Value) -> Self {
        Part {
            data: Some(data),
            ..Default::default()
        }
    }

    /// Set the media type.
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }
}

impl WireJson for Part {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        if let Some(t) = &self.text {
            insert_str(&mut obj, "text", t);
        } else if let Some(r) = &self.raw {
            insert_str(&mut obj, "raw", r);
        } else if let Some(u) = &self.url {
            insert_str(&mut obj, "url", u);
        } else if let Some(d) = &self.data {
            insert(&mut obj, "data", d.clone());
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        insert_opt_str(&mut obj, "filename", &self.filename);
        insert_opt_str(&mut obj, "mediaType", &self.media_type);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "Part")?;
        let mut part = Part::default();
        let mut found = false;
        for (key, v) in obj {
            if v.is_null() {
                continue;
            }
            match key.as_str() {
                "text" => {
                    part.text = Some(v.as_str().ok_or("Part.text must be a string")?.to_string());
                }
                "raw" => {
                    part.raw = Some(v.as_str().ok_or("Part.raw must be a string")?.to_string());
                }
                "url" => {
                    part.url = Some(v.as_str().ok_or("Part.url must be a string")?.to_string());
                }
                "data" => {
                    if v.is_string() || v.is_object() || v.is_number() || v.is_boolean() {
                        part.data = Some(v.clone());
                    } else {
                        return Err(
                            "data field must be a string, bool, integer double or JSON object".to_string()
                        );
                    }
                }
                _ => continue,
            }
            found = true;
            break;
        }
        if !found {
            return Err("Part must contain one of: text, raw, url, data".to_string());
        }
        part.metadata = opt_metadata(obj);
        part.filename = opt_string_nullable(obj, "filename", "Part")?;
        part.media_type = opt_string_nullable(obj, "mediaType", "Part")?;
        Ok(part)
    }
}
impl_wire_serde!(Part);

/// Agent-produced artifact attached to a task.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Artifact {
    /// Artifact identifier, required and non-empty.
    pub artifact_id: String,
    /// Optional description.
    pub description: Option<String>,
    /// Optional extension URIs.
    pub extensions: Option<Vec<String>>,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Optional name.
    pub name: Option<String>,
    /// Content parts.
    pub parts: Vec<Part>,
}

impl WireJson for Artifact {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.artifact_id.is_empty() {
            return Err("Artifact.artifactId cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, "artifactId", &self.artifact_id);
        insert(&mut obj, "parts", vec_to_json(&self.parts)?);
        insert_opt_str(&mut obj, "description", &self.description);
        if let Some(e) = &self.extensions {
            insert(&mut obj, "extensions", string_vec_json(e));
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        insert_opt_str(&mut obj, "name", &self.name);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "Artifact")?;
        if !obj.contains_key("artifactId") || !obj.contains_key("parts") {
            return Err("Artifact missing required fields".to_string());
        }
        let artifact_id = get_string(obj, "artifactId", "Artifact")?;
        if artifact_id.is_empty() {
            return Err("Artifact.artifactId cannot be empty".to_string());
        }
        Ok(Artifact {
            artifact_id,
            description: opt_string(obj, "description", "Artifact")?,
            extensions: opt_string_vec(obj, "extensions", "Artifact")?,
            metadata: opt_metadata(obj),
            name: opt_string(obj, "name", "Artifact")?,
            parts: vec_of(&obj["parts"], "Artifact.parts")?,
        })
    }
}
impl_wire_serde!(Artifact);

/// A conversation message (user or agent).
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Conversation context identifier.
    pub context_id: Option<String>,
    /// Extension URIs.
    pub extensions: Option<Vec<String>>,
    /// Message identifier, required and non-empty.
    pub message_id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Content parts.
    pub parts: Vec<Part>,
    /// Referenced task identifiers.
    pub reference_task_ids: Option<Vec<String>>,
    /// Sender role.
    pub role: Role,
    /// Task identifier.
    pub task_id: Option<String>,
}

impl Default for Message {
    fn default() -> Self {
        Message {
            context_id: None,
            extensions: None,
            message_id: String::new(),
            metadata: None,
            parts: Vec::new(),
            reference_task_ids: None,
            role: Role::User,
            task_id: None,
        }
    }
}

impl WireJson for Message {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.message_id.is_empty() {
            return Err("Message.messageId cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, "messageId", &self.message_id);
        insert(&mut obj, "parts", vec_to_json(&self.parts)?);
        insert_str(&mut obj, "role", self.role.as_wire());
        insert_opt_str(&mut obj, "contextId", &self.context_id);
        if let Some(e) = &self.extensions {
            insert(&mut obj, "extensions", string_vec_json(e));
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        if let Some(r) = &self.reference_task_ids {
            insert(&mut obj, "referenceTaskIds", string_vec_json(r));
        }
        insert_opt_str(&mut obj, "taskId", &self.task_id);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "Message")?;
        if !obj.contains_key("messageId") || !obj.contains_key("parts") {
            return Err("Message missing required fields".to_string());
        }
        let message_id = get_string(obj, "messageId", "Message")?;
        if message_id.is_empty() {
            return Err("Message.messageId cannot be empty".to_string());
        }
        let role = match obj.get("role") {
            Some(v) => Role::from_wire(v.as_str().ok_or("Message.role must be a string")?)?,
            None => Role::User,
        };
        Ok(Message {
            context_id: opt_string(obj, "contextId", "Message")?,
            extensions: opt_string_vec(obj, "extensions", "Message")?,
            message_id,
            metadata: opt_metadata(obj),
            parts: vec_of(&obj["parts"], "Message.parts")?,
            reference_task_ids: opt_string_vec(obj, "referenceTaskIds", "Message")?,
            role,
            task_id: opt_string(obj, "taskId", "Message")?,
        })
    }
}
impl_wire_serde!(Message);

/// Lifecycle state of a task.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Task submitted.
    Submitted,
    /// Task in progress.
    Working,
    /// Task waits for user input.
    InputRequired,
    /// Task completed.
    Completed,
    /// Task canceled.
    Canceled,
    /// Task failed.
    Failed,
    /// Task rejected.
    Rejected,
    /// Task waits for authentication.
    AuthRequired,
    /// Unknown or unspecified state.
    #[default]
    Unspecified,
}

impl TaskState {
    /// Wire string of the state.
    pub fn as_wire(self) -> &'static str {
        match self {
            TaskState::Submitted => "TASK_STATE_SUBMITTED",
            TaskState::Working => "TASK_STATE_WORKING",
            TaskState::InputRequired => "TASK_STATE_INPUT_REQUIRED",
            TaskState::Completed => "TASK_STATE_COMPLETED",
            TaskState::Canceled => "TASK_STATE_CANCELED",
            TaskState::Failed => "TASK_STATE_FAILED",
            TaskState::Rejected => "TASK_STATE_REJECTED",
            TaskState::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
            TaskState::Unspecified => "TASK_STATE_UNSPECIFIED",
        }
    }

    /// Parse a wire string. Unknown strings map to `Unspecified`.
    pub fn from_wire(s: &str) -> TaskState {
        match s {
            "TASK_STATE_SUBMITTED" => TaskState::Submitted,
            "TASK_STATE_WORKING" => TaskState::Working,
            "TASK_STATE_INPUT_REQUIRED" => TaskState::InputRequired,
            "TASK_STATE_COMPLETED" => TaskState::Completed,
            "TASK_STATE_CANCELED" => TaskState::Canceled,
            "TASK_STATE_FAILED" => TaskState::Failed,
            "TASK_STATE_REJECTED" => TaskState::Rejected,
            "TASK_STATE_AUTH_REQUIRED" => TaskState::AuthRequired,
            _ => TaskState::Unspecified,
        }
    }
}

impl Serialize for TaskState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for TaskState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(TaskState::from_wire(&s))
    }
}

/// Current status snapshot of a task.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskStatus {
    /// Optional status message.
    pub message: Option<Message>,
    /// Task state.
    pub state: TaskState,
    /// Optional ISO-8601 timestamp.
    pub timestamp: Option<String>,
}

impl TaskStatus {
    /// Build a status with only a state.
    pub fn new(state: TaskState) -> Self {
        TaskStatus {
            message: None,
            state,
            timestamp: None,
        }
    }
}

impl WireJson for TaskStatus {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "state", self.state.as_wire());
        if let Some(m) = &self.message {
            insert(&mut obj, JSON_FIELD_MESSAGE, m.to_json_value()?);
        }
        insert_opt_str(&mut obj, "timestamp", &self.timestamp);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "TaskStatus")?;
        let state = match obj.get("state") {
            Some(Value::String(s)) => TaskState::from_wire(s),
            _ => TaskState::Unspecified,
        };
        let message = match obj.get(JSON_FIELD_MESSAGE) {
            Some(v) => Some(Message::from_json_value(v)?),
            None => None,
        };
        Ok(TaskStatus {
            message,
            state,
            timestamp: opt_string(obj, "timestamp", "TaskStatus")?,
        })
    }
}
impl_wire_serde!(TaskStatus);

/// A2A task object with status, history and artifacts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Task {
    /// Produced artifacts.
    pub artifacts: Option<Vec<Artifact>>,
    /// Conversation context identifier.
    pub context_id: String,
    /// Message history.
    pub history: Option<Vec<Message>>,
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Current status.
    pub status: TaskStatus,
    /// Creation time; not implemented in the original SDK.
    pub creat_at: Option<String>,
    /// Last modification time; not implemented in the original SDK.
    pub last_modified: Option<String>,
}

impl WireJson for Task {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.id.is_empty() {
            return Err("Task.id cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, "contextId", &self.context_id);
        insert_str(&mut obj, JSON_FIELD_ID, &self.id);
        insert(&mut obj, "status", self.status.to_json_value()?);
        if let Some(a) = &self.artifacts {
            insert(&mut obj, "artifacts", vec_to_json(a)?);
        }
        if let Some(h) = &self.history {
            insert(&mut obj, "history", vec_to_json(h)?);
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "Task")?;
        if !obj.contains_key("contextId") || !obj.contains_key(JSON_FIELD_ID) || !obj.contains_key("status") {
            return Err("Task missing required fields".to_string());
        }
        let id = get_string(obj, JSON_FIELD_ID, "Task")?;
        if id.is_empty() {
            return Err("Task.id cannot be empty".to_string());
        }
        let artifacts = match obj.get("artifacts") {
            Some(v) => Some(vec_of(v, "Task.artifacts")?),
            None => None,
        };
        let history = match obj.get("history") {
            Some(v) => Some(vec_of(v, "Task.history")?),
            None => None,
        };
        Ok(Task {
            artifacts,
            context_id: get_string(obj, "contextId", "Task")?,
            history,
            id,
            metadata: opt_metadata(obj),
            status: TaskStatus::from_json_value(&obj["status"])?,
            creat_at: None,
            last_modified: None,
        })
    }
}
impl_wire_serde!(Task);

/// Authentication info for push-notification webhooks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushNotificationAuthenticationInfo {
    /// Optional credentials.
    pub credentials: Option<String>,
    /// Supported authentication schemes.
    pub schemes: Vec<String>,
}

impl WireJson for PushNotificationAuthenticationInfo {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert(&mut obj, "schemes", string_vec_json(&self.schemes));
        insert_opt_str(&mut obj, "credentials", &self.credentials);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "PushNotificationAuthenticationInfo")?;
        let schemes_json = obj
            .get("schemes")
            .ok_or("PushNotificationAuthenticationInfo must contain 'schemes' field")?;
        let schemes = string_vec(schemes_json, "PushNotificationAuthenticationInfo.schemes")?;
        Ok(PushNotificationAuthenticationInfo {
            credentials: opt_string(obj, "credentials", "PushNotificationAuthenticationInfo")?,
            schemes,
        })
    }
}
impl_wire_serde!(PushNotificationAuthenticationInfo);

/// Push-notification webhook configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushNotificationConfig {
    /// Optional authentication info.
    pub authentication: Option<PushNotificationAuthenticationInfo>,
    /// Optional config identifier (alias of `id` on the wire).
    pub config_id: Option<String>,
    /// Optional identifier.
    pub id: Option<String>,
    /// Optional token.
    pub token: Option<String>,
    /// Webhook URL, required and non-empty.
    pub url: String,
    /// Creation time; not implemented in the original SDK.
    pub created_at: Option<String>,
}

impl WireJson for PushNotificationConfig {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.url.is_empty() {
            return Err("PushNotificationConfig.url cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, "url", &self.url);
        if let Some(a) = &self.authentication {
            insert(&mut obj, "authentication", a.to_json_value()?);
        }
        insert_opt_str(&mut obj, "configId", &self.config_id);
        insert_opt_str(&mut obj, JSON_FIELD_ID, &self.id);
        insert_opt_str(&mut obj, "token", &self.token);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "PushNotificationConfig")?;
        if !obj.contains_key("url") {
            return Err("PushNotificationConfig must contain 'url' field".to_string());
        }
        let url = get_string(obj, "url", "PushNotificationConfig")?;
        if url.is_empty() {
            return Err("PushNotificationConfig.url cannot be empty".to_string());
        }
        let authentication = match obj.get("authentication") {
            Some(v) => Some(PushNotificationAuthenticationInfo::from_json_value(v)?),
            None => None,
        };
        // Like the C++ SDK, `configId` overrides `id` when both are present.
        let mut id = opt_string(obj, JSON_FIELD_ID, "PushNotificationConfig")?;
        if let Some(cid) = opt_string(obj, "configId", "PushNotificationConfig")? {
            id = Some(cid);
        }
        Ok(PushNotificationConfig {
            authentication,
            config_id: None,
            id,
            token: opt_string(obj, "token", "PushNotificationConfig")?,
            url,
            created_at: None,
        })
    }
}
impl_wire_serde!(PushNotificationConfig);

/// Client configuration for `message/send` requests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageSendConfiguration {
    /// Accepted output modes.
    pub accepted_output_modes: Option<Vec<String>>,
    /// Maximum history length in responses.
    pub history_length: Option<i32>,
    /// Push notification config for the task.
    pub push_notification_config: Option<PushNotificationConfig>,
    /// Return immediately instead of blocking.
    pub return_immediately: Option<bool>,
}

impl WireJson for MessageSendConfiguration {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        if let Some(m) = &self.accepted_output_modes {
            insert(&mut obj, "acceptedOutputModes", string_vec_json(m));
        }
        if let Some(h) = self.history_length {
            insert(&mut obj, "historyLength", Value::from(h));
        }
        if let Some(p) = &self.push_notification_config {
            insert(&mut obj, "pushNotificationConfig", p.to_json_value()?);
        }
        insert_opt_bool(&mut obj, "returnImmediately", self.return_immediately);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "MessageSendConfiguration")?;
        let accepted_output_modes = match obj.get("acceptedOutputModes") {
            None | Some(Value::Null) => None,
            Some(v) => Some(string_vec(v, "acceptedOutputModes")?),
        };
        let push_notification_config = match obj.get("pushNotificationConfig") {
            Some(v) => Some(PushNotificationConfig::from_json_value(v)?),
            None => None,
        };
        Ok(MessageSendConfiguration {
            accepted_output_modes,
            history_length: opt_i32(obj, "historyLength", "MessageSendConfiguration")?,
            push_notification_config,
            return_immediately: opt_bool(obj, "returnImmediately", "MessageSendConfiguration")?,
        })
    }
}
impl_wire_serde!(MessageSendConfiguration);

/// Parameters for `message/send` and `message/stream`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageSendParams {
    /// Optional send configuration.
    pub configuration: Option<MessageSendConfiguration>,
    /// The message to send.
    pub message: Message,
    /// Optional metadata object.
    pub metadata: Option<Value>,
}

impl WireJson for MessageSendParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert(&mut obj, JSON_FIELD_MESSAGE, self.message.to_json_value()?);
        if let Some(c) = &self.configuration {
            insert(&mut obj, "configuration", c.to_json_value()?);
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "MessageSendParams")?;
        let message_json = obj
            .get(JSON_FIELD_MESSAGE)
            .ok_or("MessageSendParams must contain 'message' field")?;
        let configuration = match obj.get("configuration") {
            Some(v) => Some(MessageSendConfiguration::from_json_value(v)?),
            None => None,
        };
        Ok(MessageSendParams {
            configuration,
            message: Message::from_json_value(message_json)?,
            metadata: opt_metadata(obj),
        })
    }
}
impl_wire_serde!(MessageSendParams);

/// Streaming event: artifact chunk update for a task.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskArtifactUpdateEvent {
    /// Append parts to an existing artifact.
    pub append: Option<bool>,
    /// The artifact chunk.
    pub artifact: Artifact,
    /// Context identifier.
    pub context_id: String,
    /// Whether this is the last chunk.
    pub last_chunk: Option<bool>,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Task identifier, required and non-empty.
    pub task_id: String,
    /// Chunk index; not implemented in the original SDK.
    pub index: Option<i32>,
}

impl WireJson for TaskArtifactUpdateEvent {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert(&mut obj, "artifact", self.artifact.to_json_value()?);
        insert_str(&mut obj, "contextId", &self.context_id);
        insert_str(&mut obj, "taskId", &self.task_id);
        insert_opt_bool(&mut obj, "append", self.append);
        insert_opt_bool(&mut obj, "lastChunk", self.last_chunk);
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "TaskArtifactUpdateEvent")?;
        if !obj.contains_key("artifact") || !obj.contains_key("contextId") || !obj.contains_key("taskId") {
            return Err("TaskArtifactUpdateEvent missing required fields".to_string());
        }
        let task_id = get_string(obj, "taskId", "TaskArtifactUpdateEvent")?;
        if task_id.is_empty() {
            return Err("TaskArtifactUpdateEvent.taskId cannot be empty".to_string());
        }
        Ok(TaskArtifactUpdateEvent {
            append: opt_bool(obj, "append", "TaskArtifactUpdateEvent")?,
            artifact: Artifact::from_json_value(&obj["artifact"])?,
            context_id: get_string(obj, "contextId", "TaskArtifactUpdateEvent")?,
            last_chunk: opt_bool(obj, "lastChunk", "TaskArtifactUpdateEvent")?,
            metadata: opt_metadata(obj),
            task_id,
            index: None,
        })
    }
}
impl_wire_serde!(TaskArtifactUpdateEvent);

/// Streaming event: task status change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskStatusUpdateEvent {
    /// Context identifier.
    pub context_id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// New status.
    pub status: TaskStatus,
    /// Task identifier, required and non-empty.
    pub task_id: String,
}

impl WireJson for TaskStatusUpdateEvent {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "contextId", &self.context_id);
        insert(&mut obj, "status", self.status.to_json_value()?);
        insert_str(&mut obj, "taskId", &self.task_id);
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "TaskStatusUpdateEvent")?;
        if !obj.contains_key("contextId") || !obj.contains_key("status") || !obj.contains_key("taskId") {
            return Err("TaskStatusUpdateEvent missing required fields".to_string());
        }
        let task_id = get_string(obj, "taskId", "TaskStatusUpdateEvent")?;
        if task_id.is_empty() {
            return Err("TaskStatusUpdateEvent.taskId cannot be empty".to_string());
        }
        Ok(TaskStatusUpdateEvent {
            context_id: get_string(obj, "contextId", "TaskStatusUpdateEvent")?,
            metadata: opt_metadata(obj),
            status: TaskStatus::from_json_value(&obj["status"])?,
            task_id,
        })
    }
}
impl_wire_serde!(TaskStatusUpdateEvent);

/// Parameter object identifying a task by ID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskIdParams {
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
}

impl TaskIdParams {
    /// Build params for a task id.
    pub fn new(id: impl Into<String>) -> Self {
        TaskIdParams {
            id: id.into(),
            metadata: None,
        }
    }
}

fn id_params_to_json(type_name: &str, id: &str, metadata: &Option<Value>) -> JsonResult<Map<String, Value>> {
    if id.is_empty() {
        return Err(format!("{type_name}.id cannot be empty"));
    }
    let mut obj = Map::new();
    insert_str(&mut obj, JSON_FIELD_ID, id);
    insert_opt_metadata(&mut obj, metadata);
    Ok(obj)
}

fn id_params_from_json<'a>(
    type_name: &str,
    value: &'a Value,
) -> JsonResult<(&'a Map<String, Value>, String)> {
    let obj = as_object(value, type_name)?;
    if !obj.contains_key(JSON_FIELD_ID) {
        return Err(format!("{type_name} must contain 'id' field"));
    }
    let id = get_string(obj, JSON_FIELD_ID, type_name)?;
    if id.is_empty() {
        return Err(format!("{type_name}.id cannot be empty"));
    }
    Ok((obj, id))
}

impl WireJson for TaskIdParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        Ok(Value::Object(id_params_to_json(
            "TaskIdParams",
            &self.id,
            &self.metadata,
        )?))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let (obj, id) = id_params_from_json("TaskIdParams", value)?;
        Ok(TaskIdParams {
            id,
            metadata: opt_metadata(obj),
        })
    }
}
impl_wire_serde!(TaskIdParams);

/// Parameter object for `tasks/get`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskQueryParams {
    /// Maximum number of history messages to return.
    pub history_length: Option<i32>,
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
}

impl TaskQueryParams {
    /// Build params for a task id.
    pub fn new(id: impl Into<String>) -> Self {
        TaskQueryParams {
            history_length: None,
            id: id.into(),
            metadata: None,
        }
    }
}

impl WireJson for TaskQueryParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = id_params_to_json("TaskQueryParams", &self.id, &None)?;
        if let Some(h) = self.history_length {
            insert(&mut obj, "historyLength", Value::from(h));
        }
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let (obj, id) = id_params_from_json("TaskQueryParams", value)?;
        Ok(TaskQueryParams {
            history_length: opt_i32(obj, "historyLength", "TaskQueryParams")?,
            id,
            metadata: opt_metadata(obj),
        })
    }
}
impl_wire_serde!(TaskQueryParams);

/// Push-notification config bound to a specific task.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskPushNotificationConfig {
    /// The config.
    pub push_notification_config: PushNotificationConfig,
    /// Task identifier, required and non-empty.
    pub task_id: String,
}

impl WireJson for TaskPushNotificationConfig {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.task_id.is_empty() {
            return Err("TaskPushNotificationConfig.taskId cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert(
            &mut obj,
            "pushNotificationConfig",
            self.push_notification_config.to_json_value()?,
        );
        insert_str(&mut obj, "taskId", &self.task_id);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "TaskPushNotificationConfig")?;
        if !obj.contains_key("pushNotificationConfig") || !obj.contains_key("taskId") {
            return Err("TaskPushNotificationConfig missing required fields".to_string());
        }
        let task_id = get_string(obj, "taskId", "TaskPushNotificationConfig")?;
        if task_id.is_empty() {
            return Err("TaskPushNotificationConfig.taskId cannot be empty".to_string());
        }
        Ok(TaskPushNotificationConfig {
            push_notification_config: PushNotificationConfig::from_json_value(
                &obj["pushNotificationConfig"],
            )?,
            task_id,
        })
    }
}
impl_wire_serde!(TaskPushNotificationConfig);

/// Parameters for `GetTaskPushNotificationConfig`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GetTaskPushNotificationConfigParams {
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Optional config identifier.
    pub push_notification_config_id: Option<String>,
}

impl WireJson for GetTaskPushNotificationConfigParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = id_params_to_json("GetTaskPushNotificationConfigParams", &self.id, &self.metadata)?;
        insert_opt_str(
            &mut obj,
            "pushNotificationConfigId",
            &self.push_notification_config_id,
        );
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let (obj, id) = id_params_from_json("GetTaskPushNotificationConfigParams", value)?;
        Ok(GetTaskPushNotificationConfigParams {
            id,
            metadata: opt_metadata(obj),
            push_notification_config_id: opt_string(
                obj,
                "pushNotificationConfigId",
                "GetTaskPushNotificationConfigParams",
            )?,
        })
    }
}
impl_wire_serde!(GetTaskPushNotificationConfigParams);

/// Parameters for `ListTaskPushNotificationConfigs`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListTaskPushNotificationConfigParams {
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
}

impl WireJson for ListTaskPushNotificationConfigParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        Ok(Value::Object(id_params_to_json(
            "ListTaskPushNotificationConfigParams",
            &self.id,
            &self.metadata,
        )?))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let (obj, id) = id_params_from_json("ListTaskPushNotificationConfigParams", value)?;
        Ok(ListTaskPushNotificationConfigParams {
            id,
            metadata: opt_metadata(obj),
        })
    }
}
impl_wire_serde!(ListTaskPushNotificationConfigParams);

/// Parameters for `DeleteTaskPushNotificationConfig`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeleteTaskPushNotificationConfigParams {
    /// Task identifier, required and non-empty.
    pub id: String,
    /// Optional metadata object.
    pub metadata: Option<Value>,
    /// Config identifier, required and non-empty.
    pub push_notification_config_id: String,
}

impl WireJson for DeleteTaskPushNotificationConfigParams {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.id.is_empty() {
            return Err("DeleteTaskPushNotificationConfigParams.id cannot be empty".to_string());
        }
        if self.push_notification_config_id.is_empty() {
            return Err(
                "DeleteTaskPushNotificationConfigParams.pushNotificationConfigId cannot be empty".to_string(),
            );
        }
        let mut obj = Map::new();
        insert_str(&mut obj, JSON_FIELD_ID, &self.id);
        insert_str(
            &mut obj,
            "pushNotificationConfigId",
            &self.push_notification_config_id,
        );
        insert_opt_metadata(&mut obj, &self.metadata);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "DeleteTaskPushNotificationConfigParams")?;
        if !obj.contains_key(JSON_FIELD_ID) || !obj.contains_key("pushNotificationConfigId") {
            return Err("DeleteTaskPushNotificationConfigParams missing required fields".to_string());
        }
        let id = get_string(obj, JSON_FIELD_ID, "DeleteTaskPushNotificationConfigParams")?;
        let config_id = get_string(
            obj,
            "pushNotificationConfigId",
            "DeleteTaskPushNotificationConfigParams",
        )?;
        if id.is_empty() {
            return Err("DeleteTaskPushNotificationConfigParams.id cannot be empty".to_string());
        }
        if config_id.is_empty() {
            return Err(
                "DeleteTaskPushNotificationConfigParams.pushNotificationConfigId cannot be empty".to_string(),
            );
        }
        Ok(DeleteTaskPushNotificationConfigParams {
            id,
            metadata: opt_metadata(obj),
            push_notification_config_id: config_id,
        })
    }
}
impl_wire_serde!(DeleteTaskPushNotificationConfigParams);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Wire-format error object (JSON-RPC error or A2A extension).
#[derive(Clone, Debug, PartialEq)]
pub struct A2AError {
    /// Error code.
    pub code: i64,
    /// Optional additional data.
    pub data: Option<Value>,
    /// Optional message.
    pub message: Option<String>,
}

impl Default for A2AError {
    fn default() -> Self {
        A2AError {
            code: -1,
            data: None,
            message: None,
        }
    }
}

impl A2AError {
    /// Build an error with a code and message.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        A2AError {
            code,
            data: None,
            message: Some(message.into()),
        }
    }

    fn predefined(code: crate::error::A2AErrorCode, message: &str) -> Self {
        A2AError::new(code.code(), message)
    }

    /// JSON-RPC -32600 Invalid Request (`InvalidRequestError`).
    pub fn invalid_request() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::JsonrpcInvalidRequest,
            "Request payload validation error",
        )
    }

    /// JSON-RPC -32603 generic server error (`ServerError`).
    pub fn server_error() -> Self {
        Self::predefined(crate::error::A2AErrorCode::JsonrpcInternalError, "Server error")
    }

    /// JSON-RPC -32602 Invalid params (`InvalidParamsError`).
    pub fn invalid_params() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::JsonrpcInvalidParams,
            "Invalid parameters",
        )
    }

    /// JSON-RPC -32700 Parse error (`JSONParseError`).
    pub fn json_parse_error() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::JsonrpcParseError,
            "Invalid JSON payload",
        )
    }

    /// JSON-RPC -32603 Internal error (`InternalError`).
    pub fn internal_error() -> Self {
        Self::predefined(crate::error::A2AErrorCode::JsonrpcInternalError, "Internal error")
    }

    /// JSON-RPC -32601 Method not found (`MethodNotFoundError`).
    pub fn method_not_found() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::JsonrpcMethodNotFound,
            "Method not found",
        )
    }

    /// A2A -32001 Task not found (`TaskNotFoundError`).
    pub fn task_not_found() -> Self {
        Self::predefined(crate::error::A2AErrorCode::TaskNotFound, "Task not found")
    }

    /// A2A -32002 Task not cancelable (`TaskNotCancelableError`).
    pub fn task_not_cancelable() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::TaskNotCancelable,
            "Task cannot be canceled",
        )
    }

    /// A2A -32003 Push notification not supported (`PushNotificationNotSupportedError`).
    pub fn push_notification_not_supported() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::PushNotificationNotSupported,
            "Push Notification is not supported",
        )
    }

    /// A2A -32004 Unsupported operation (`UnsupportedOperationError`).
    pub fn unsupported_operation() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::UnsupportedOperation,
            "This operation is not supported",
        )
    }

    /// A2A -32005 Content type not supported (`ContentTypeNotSupportedError`).
    pub fn content_type_not_supported() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::ContentTypeNotSupported,
            "Incompatible content types",
        )
    }

    /// A2A -32006 Invalid agent response (`InvalidAgentResponseError`).
    pub fn invalid_agent_response() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::InvalidAgentResponse,
            "Invalid agent response",
        )
    }

    /// A2A -32007 (`AuthenticatedExtendedCardNotConfiguredError`).
    pub fn authenticated_extended_card_not_configured() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::AuthenticatedExtendedCardNotConfigured,
            "Authenticated Extended Card is not configured",
        )
    }

    /// A2A -32008 Extension support required (`ExtensionSupportRequiredError`).
    pub fn extension_support_required() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::ExtensionSupportRequiredError,
            "Extension Support Required",
        )
    }

    /// A2A -32009 Version not supported (`VersionNotSupportedError`).
    pub fn version_not_supported() -> Self {
        Self::predefined(
            crate::error::A2AErrorCode::VersionNotSupportedError,
            "Version Not Supported",
        )
    }
}

impl WireJson for A2AError {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert(&mut obj, "code", Value::from(self.code));
        insert_str(
            &mut obj,
            JSON_FIELD_MESSAGE,
            self.message.as_deref().unwrap_or(""),
        );
        if let Some(d) = &self.data {
            insert(&mut obj, "data", d.clone());
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "A2AError")?;
        let code = obj
            .get("code")
            .and_then(Value::as_i64)
            .ok_or("A2AError must contain an integer 'code' field")?;
        Ok(A2AError {
            code,
            data: obj.get("data").cloned(),
            message: opt_string(obj, JSON_FIELD_MESSAGE, "A2AError")?,
        })
    }
}
impl_wire_serde!(A2AError);

impl From<ap_jsonrpc::RpcError> for A2AError {
    fn from(e: ap_jsonrpc::RpcError) -> Self {
        A2AError {
            code: e.code,
            data: e.data,
            message: Some(e.message),
        }
    }
}

impl From<A2AError> for ap_jsonrpc::RpcError {
    fn from(e: A2AError) -> Self {
        ap_jsonrpc::RpcError {
            code: e.code,
            message: e.message.unwrap_or_default(),
            data: e.data,
        }
    }
}

// ---------------------------------------------------------------------------
// Security schemes
// ---------------------------------------------------------------------------

/// OpenAPI API-key security scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct APIKeySecurityScheme {
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Location of the key: cookie, header or query.
    #[serde(rename = "in")]
    pub in_: String,
    /// Parameter name.
    pub name: String,
}

/// OpenAPI HTTP authentication security scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HTTPAuthSecurityScheme {
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// HTTP auth scheme name.
    pub scheme: String,
}

/// OAuth2 implicit flow definition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplicitOAuthFlow {
    /// Authorization URL.
    pub authorization_url: String,
    /// Optional refresh URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    /// Available scopes.
    pub scopes: BTreeMap<String, String>,
}

/// OAuth2 authorization-code flow definition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationCodeOAuthFlow {
    /// Authorization URL.
    pub authorization_url: String,
    /// Optional refresh URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    /// Available scopes.
    pub scopes: BTreeMap<String, String>,
    /// Token URL.
    pub token_url: String,
}

/// OAuth2 password flow definition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordOAuthFlow {
    /// Optional refresh URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    /// Available scopes.
    pub scopes: BTreeMap<String, String>,
    /// Token URL.
    pub token_url: String,
}

/// OAuth2 client-credentials flow definition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCredentialsOAuthFlow {
    /// Optional refresh URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    /// Available scopes.
    pub scopes: BTreeMap<String, String>,
    /// Token URL.
    pub token_url: String,
}

/// Collection of supported OAuth2 flows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlows {
    /// Authorization-code flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<AuthorizationCodeOAuthFlow>,
    /// Client-credentials flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<ClientCredentialsOAuthFlow>,
    /// Implicit flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit: Option<ImplicitOAuthFlow>,
    /// Password flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<PasswordOAuthFlow>,
}

/// OpenAPI OAuth2 security scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuth2SecurityScheme {
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Supported flows.
    pub flows: OAuthFlows,
    /// Optional OAuth2 metadata URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2_metadata_url: Option<String>,
}

/// OpenAPI OpenID Connect security scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenIdConnectSecurityScheme {
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// OpenID Connect discovery URL.
    pub open_id_connect_url: String,
}

/// Mutual TLS security scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutualTLSSecurityScheme {
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Discriminated union of supported security schemes.
///
/// The `type` field on the wire selects the variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SecurityScheme {
    /// `apiKey`.
    #[serde(rename = "apiKey")]
    ApiKey(APIKeySecurityScheme),
    /// `http`.
    #[serde(rename = "http")]
    HttpAuth(HTTPAuthSecurityScheme),
    /// `oauth2`.
    #[serde(rename = "oauth2")]
    OAuth2(Box<OAuth2SecurityScheme>),
    /// `openIdConnect`.
    #[serde(rename = "openIdConnect")]
    OpenIdConnect(OpenIdConnectSecurityScheme),
    /// `mutualTLS`.
    #[serde(rename = "mutualTLS")]
    MutualTls(MutualTLSSecurityScheme),
}

impl SecurityScheme {
    /// Wire value of the `type` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            SecurityScheme::ApiKey(_) => "apiKey",
            SecurityScheme::HttpAuth(_) => "http",
            SecurityScheme::OAuth2(_) => "oauth2",
            SecurityScheme::OpenIdConnect(_) => "openIdConnect",
            SecurityScheme::MutualTls(_) => "mutualTLS",
        }
    }
}

// ---------------------------------------------------------------------------
// Agent card
// ---------------------------------------------------------------------------

/// A capability advertised by an agent (skill).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSkill {
    /// Skill identifier, required and non-empty.
    pub id: String,
    /// Skill name, required and non-empty.
    pub name: String,
    /// Description.
    pub description: String,
    /// Tags.
    pub tags: Vec<String>,
    /// Example prompts.
    pub examples: Option<Vec<String>>,
    /// Input modes.
    pub input_modes: Option<Vec<String>>,
    /// Output modes.
    pub output_modes: Option<Vec<String>>,
    /// Extension.
    pub extension: Option<String>,
}

impl WireJson for AgentSkill {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.id.is_empty() {
            return Err("AgentSkill.id cannot be empty".to_string());
        }
        if self.name.is_empty() {
            return Err("AgentSkill.name cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, JSON_FIELD_ID, &self.id);
        insert_str(&mut obj, "name", &self.name);
        insert_str(&mut obj, "description", &self.description);
        insert(&mut obj, "tags", string_vec_json(&self.tags));
        if let Some(e) = &self.examples {
            insert(&mut obj, "examples", string_vec_json(e));
        }
        if let Some(m) = &self.input_modes {
            insert(&mut obj, "inputModes", string_vec_json(m));
        }
        if let Some(m) = &self.output_modes {
            insert(&mut obj, "outputModes", string_vec_json(m));
        }
        insert_opt_str(&mut obj, "extension", &self.extension);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentSkill")?;
        if !obj.contains_key(JSON_FIELD_ID)
            || !obj.contains_key("name")
            || !obj.contains_key("description")
            || !obj.contains_key("tags")
        {
            return Err("AgentSkill missing required fields".to_string());
        }
        let id = get_string(obj, JSON_FIELD_ID, "AgentSkill")?;
        let name = get_string(obj, "name", "AgentSkill")?;
        if id.is_empty() {
            return Err("AgentSkill.id cannot be empty".to_string());
        }
        if name.is_empty() {
            return Err("AgentSkill.name cannot be empty".to_string());
        }
        Ok(AgentSkill {
            id,
            name,
            description: get_string(obj, "description", "AgentSkill")?,
            tags: get_string_vec(obj, "tags", "AgentSkill")?,
            examples: opt_string_vec(obj, "examples", "AgentSkill")?,
            input_modes: opt_string_vec(obj, "inputModes", "AgentSkill")?,
            output_modes: opt_string_vec(obj, "outputModes", "AgentSkill")?,
            extension: opt_string(obj, "extension", "AgentSkill")?,
        })
    }
}
impl_wire_serde!(AgentSkill);

/// Protocol extension entry in an agent card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentExtension {
    /// Extension URI, required and non-empty.
    pub uri: String,
    /// Whether the extension is required.
    pub required: Option<bool>,
    /// Description.
    pub description: Option<String>,
    /// Extension parameters.
    pub params: Option<Value>,
}

impl WireJson for AgentExtension {
    fn to_json_value(&self) -> JsonResult<Value> {
        if self.uri.is_empty() {
            return Err("AgentExtension.uri cannot be empty".to_string());
        }
        let mut obj = Map::new();
        insert_str(&mut obj, "uri", &self.uri);
        insert_opt_bool(&mut obj, "required", self.required);
        insert_opt_str(&mut obj, "description", &self.description);
        if let Some(p) = &self.params {
            insert(&mut obj, "params", p.clone());
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentExtension")?;
        if !obj.contains_key("uri") {
            return Err("AgentExtension must contain 'uri' field".to_string());
        }
        let uri = get_string(obj, "uri", "AgentExtension")?;
        if uri.is_empty() {
            return Err("AgentExtension.uri cannot be empty".to_string());
        }
        Ok(AgentExtension {
            uri,
            required: opt_bool(obj, "required", "AgentExtension")?,
            description: opt_string(obj, "description", "AgentExtension")?,
            params: obj.get("params").cloned(),
        })
    }
}
impl_wire_serde!(AgentExtension);

/// Feature flags advertised in an agent card.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Streaming support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Push notification support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    /// Extended agent card support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
    /// Extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// Extensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<AgentExtension>>,
}

/// Agent provider metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentProvider {
    /// Organization name.
    pub organization: String,
    /// Organization URL.
    pub url: String,
}

impl WireJson for AgentProvider {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "organization", &self.organization);
        insert_str(&mut obj, "url", &self.url);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentProvider")?;
        if !obj.contains_key("organization") || !obj.contains_key("url") {
            return Err("AgentProvider missing required fields".to_string());
        }
        Ok(AgentProvider {
            organization: get_string(obj, "organization", "AgentProvider")?,
            url: get_string(obj, "url", "AgentProvider")?,
        })
    }
}
impl_wire_serde!(AgentProvider);

/// A transport endpoint exposed by an agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentInterface {
    /// Endpoint URL.
    pub url: String,
    /// Protocol binding label, for example `JSONRPC`.
    pub protocol_binding: String,
    /// Protocol version.
    pub protocol_version: String,
    /// Optional tenant.
    pub tenant: Option<String>,
}

impl AgentInterface {
    /// Build a JSON-RPC interface for a URL with protocol version 1.0.
    pub fn jsonrpc(url: impl Into<String>) -> Self {
        AgentInterface {
            url: url.into(),
            protocol_binding: crate::protocol::JSONRPC_TRANSPORT.to_string(),
            protocol_version: crate::protocol::DEFAULT_PROTOCOL_VERSION.to_string(),
            tenant: None,
        }
    }
}

impl WireJson for AgentInterface {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "url", &self.url);
        insert_str(&mut obj, "protocolBinding", &self.protocol_binding);
        insert_str(&mut obj, "protocolVersion", &self.protocol_version);
        insert_opt_str(&mut obj, "tenant", &self.tenant);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentInterface")?;
        for key in ["url", "protocolBinding", "protocolVersion"] {
            if !obj.contains_key(key) {
                return Err(format!("AgentInterface must contain '{key}' field"));
            }
        }
        Ok(AgentInterface {
            url: get_string(obj, "url", "AgentInterface")?,
            protocol_binding: get_string(obj, "protocolBinding", "AgentInterface")?,
            protocol_version: get_string(obj, "protocolVersion", "AgentInterface")?,
            tenant: opt_string(obj, "tenant", "AgentInterface")?,
        })
    }
}
impl_wire_serde!(AgentInterface);

/// Security requirement referencing named schemes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityRequirement {
    /// Scheme name to required scopes.
    pub schemes: BTreeMap<String, Vec<String>>,
}

impl WireJson for SecurityRequirement {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        for (name, scopes) in &self.schemes {
            insert(&mut obj, name, string_vec_json(scopes));
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = value
            .as_object()
            .ok_or("SecurityRequirement must be a JSON object")?;
        let mut schemes = BTreeMap::new();
        for (name, scopes_json) in obj {
            let items = scopes_json.as_array().ok_or_else(|| {
                format!("SecurityRequirement scheme '{name}' must be a JSON array of strings")
            })?;
            let mut scopes = Vec::new();
            for scope in items {
                let s = scope.as_str().ok_or_else(|| {
                    format!("SecurityRequirement scope value must be a string in scheme '{name}'")
                })?;
                scopes.push(s.to_string());
            }
            schemes.insert(name.clone(), scopes);
        }
        Ok(SecurityRequirement { schemes })
    }
}
impl_wire_serde!(SecurityRequirement);

/// JWS signature over an agent card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentCardSignature {
    /// Base64url-encoded protected JWS header.
    pub protected_: String,
    /// Base64url-encoded signature bytes.
    pub signature: String,
    /// Optional unprotected JWS header.
    pub header: Option<Value>,
}

impl WireJson for AgentCardSignature {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "protected", &self.protected_);
        insert_str(&mut obj, "signature", &self.signature);
        if let Some(h) = &self.header {
            insert(&mut obj, "header", h.clone());
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentCardSignature")?;
        let protected_ = match obj.get("protected") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("AgentCardSignature missing required field: protected".to_string()),
        };
        let signature = match obj.get("signature") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("AgentCardSignature missing required field: signature".to_string()),
        };
        let header = match obj.get("header") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.clone()),
        };
        Ok(AgentCardSignature {
            protected_,
            signature,
            header,
        })
    }
}
impl_wire_serde!(AgentCardSignature);

/// Agent discovery card served at `/.well-known/agent-card.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentCard {
    /// Agent name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Provider.
    pub provider: Option<AgentProvider>,
    /// Icon URL.
    pub icon_url: Option<String>,
    /// Agent version.
    pub version: String,
    /// Documentation URL.
    pub documentation_url: Option<String>,
    /// Capabilities.
    pub capabilities: AgentCapabilities,
    /// Security schemes by name.
    pub security_schemes: Option<BTreeMap<String, SecurityScheme>>,
    /// Security requirement names.
    pub security: Option<Vec<String>>,
    /// Default input modes.
    pub default_input_modes: Vec<String>,
    /// Default output modes.
    pub default_output_modes: Vec<String>,
    /// Skills.
    pub skills: Vec<AgentSkill>,
    /// Supported transport interfaces; the first one is primary.
    pub supported_interfaces: Vec<AgentInterface>,
    /// Security requirements.
    pub security_requirements: Option<Vec<SecurityRequirement>>,
    /// Signatures.
    pub signatures: Option<Vec<AgentCardSignature>>,
    /// Category.
    pub category: Option<String>,
    /// Extension.
    pub extension: Option<String>,
}

impl WireJson for AgentCard {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        insert_str(&mut obj, "name", &self.name);
        insert_str(&mut obj, "description", &self.description);
        insert_str(&mut obj, "version", &self.version);
        let caps = serde_json::to_value(&self.capabilities).map_err(|e| e.to_string())?;
        insert(&mut obj, "capabilities", caps);
        insert(
            &mut obj,
            "defaultInputModes",
            string_vec_json(&self.default_input_modes),
        );
        insert(
            &mut obj,
            "defaultOutputModes",
            string_vec_json(&self.default_output_modes),
        );
        insert(&mut obj, "skills", vec_to_json(&self.skills)?);
        insert(
            &mut obj,
            "supportedInterfaces",
            vec_to_json(&self.supported_interfaces)?,
        );
        if let Some(p) = &self.provider {
            insert(&mut obj, "provider", p.to_json_value()?);
        }
        insert_opt_str(&mut obj, "iconUrl", &self.icon_url);
        insert_opt_str(&mut obj, "documentationUrl", &self.documentation_url);
        if let Some(s) = &self.security_schemes {
            insert(
                &mut obj,
                "securitySchemes",
                serde_json::to_value(s).map_err(|e| e.to_string())?,
            );
        }
        if let Some(s) = &self.security {
            insert(&mut obj, "security", string_vec_json(s));
        }
        if let Some(r) = &self.security_requirements {
            insert(&mut obj, "securityRequirements", vec_to_json(r)?);
        }
        if let Some(s) = &self.signatures {
            insert(&mut obj, "signatures", vec_to_json(s)?);
        }
        insert_opt_str(&mut obj, "category", &self.category);
        insert_opt_str(&mut obj, "extension", &self.extension);
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "AgentCard")?;
        for field in [
            "name",
            "description",
            "version",
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "skills",
            "supportedInterfaces",
        ] {
            if !obj.contains_key(field) {
                return Err(format!("AgentCard missing required field: {field}"));
            }
        }
        let capabilities: AgentCapabilities =
            serde_json::from_value(obj["capabilities"].clone()).map_err(|e| e.to_string())?;
        let provider = match obj.get("provider") {
            Some(v) => Some(AgentProvider::from_json_value(v)?),
            None => None,
        };
        let security_schemes = match obj.get("securitySchemes") {
            Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| e.to_string())?),
            None => None,
        };
        let security_requirements = match obj.get("securityRequirements") {
            Some(v) if v.is_array() => Some(vec_of(v, "AgentCard.securityRequirements")?),
            _ => None,
        };
        let signatures = match obj.get("signatures") {
            Some(v) if v.is_array() => Some(vec_of(v, "AgentCard.signatures")?),
            _ => None,
        };
        Ok(AgentCard {
            name: get_string(obj, "name", "AgentCard")?,
            description: get_string(obj, "description", "AgentCard")?,
            provider,
            icon_url: opt_string(obj, "iconUrl", "AgentCard")?,
            version: get_string(obj, "version", "AgentCard")?,
            documentation_url: opt_string(obj, "documentationUrl", "AgentCard")?,
            capabilities,
            security_schemes,
            security: opt_string_vec(obj, "security", "AgentCard")?,
            default_input_modes: get_string_vec(obj, "defaultInputModes", "AgentCard")?,
            default_output_modes: get_string_vec(obj, "defaultOutputModes", "AgentCard")?,
            skills: vec_of(&obj["skills"], "AgentCard.skills")?,
            supported_interfaces: vec_of(&obj["supportedInterfaces"], "AgentCard.supportedInterfaces")?,
            security_requirements,
            signatures,
            category: opt_string(obj, "category", "AgentCard")?,
            extension: opt_string(obj, "extension", "AgentCard")?,
        })
    }
}
impl_wire_serde!(AgentCard);

// ---------------------------------------------------------------------------
// Response result payloads
// ---------------------------------------------------------------------------

/// Result payload of a `SendMessage` success response.
///
/// On the wire the result is `{"task": ...}` or `{"message": ...}`.
#[derive(Clone, Debug, PartialEq)]
pub enum SendMessageResult {
    /// A task.
    Task(Box<Task>),
    /// A message.
    Message(Box<Message>),
}

impl WireJson for SendMessageResult {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        match self {
            SendMessageResult::Task(t) => insert(&mut obj, STREAM_RESPONSE_TYPE_TASK, t.to_json_value()?),
            SendMessageResult::Message(m) => insert(&mut obj, JSON_FIELD_MESSAGE, m.to_json_value()?),
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "SendMessage result")?;
        let has_message = obj.contains_key(JSON_FIELD_MESSAGE);
        let has_task = obj.contains_key(STREAM_RESPONSE_TYPE_TASK);
        if has_message && has_task {
            return Err("Response must contain either 'message' or 'task', not both".to_string());
        }
        if has_message {
            return Ok(SendMessageResult::Message(Box::new(Message::from_json_value(
                &obj[JSON_FIELD_MESSAGE],
            )?)));
        }
        if has_task {
            return Ok(SendMessageResult::Task(Box::new(Task::from_json_value(
                &obj[STREAM_RESPONSE_TYPE_TASK],
            )?)));
        }
        Err("Response must contain either 'message' or 'task' field".to_string())
    }
}
impl_wire_serde!(SendMessageResult);

/// One event of a streaming response, also used by the server task manager.
///
/// On the wire the result is `{"task": ..}`, `{"message": ..}`,
/// `{"statusUpdate": ..}` or `{"artifactUpdate": ..}`.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// A full task snapshot.
    Task(Task),
    /// A message.
    Message(Message),
    /// An artifact update.
    ArtifactUpdate(TaskArtifactUpdateEvent),
    /// A status update.
    StatusUpdate(TaskStatusUpdateEvent),
}

impl StreamEvent {
    /// Task identifier carried by the event, if any.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            StreamEvent::Task(t) => Some(&t.id),
            StreamEvent::Message(m) => m.task_id.as_deref(),
            StreamEvent::ArtifactUpdate(e) => Some(&e.task_id),
            StreamEvent::StatusUpdate(e) => Some(&e.task_id),
        }
    }
}

impl From<Task> for StreamEvent {
    fn from(t: Task) -> Self {
        StreamEvent::Task(t)
    }
}

impl From<Message> for StreamEvent {
    fn from(m: Message) -> Self {
        StreamEvent::Message(m)
    }
}

impl From<TaskArtifactUpdateEvent> for StreamEvent {
    fn from(e: TaskArtifactUpdateEvent) -> Self {
        StreamEvent::ArtifactUpdate(e)
    }
}

impl From<TaskStatusUpdateEvent> for StreamEvent {
    fn from(e: TaskStatusUpdateEvent) -> Self {
        StreamEvent::StatusUpdate(e)
    }
}

impl WireJson for StreamEvent {
    fn to_json_value(&self) -> JsonResult<Value> {
        let mut obj = Map::new();
        match self {
            StreamEvent::StatusUpdate(e) => {
                insert(&mut obj, STREAM_RESULT_KEY_STATUS_UPDATE, e.to_json_value()?)
            }
            StreamEvent::ArtifactUpdate(e) => {
                insert(&mut obj, STREAM_RESULT_KEY_ARTIFACT_UPDATE, e.to_json_value()?)
            }
            StreamEvent::Task(t) => insert(&mut obj, STREAM_RESPONSE_TYPE_TASK, t.to_json_value()?),
            StreamEvent::Message(m) => insert(&mut obj, JSON_FIELD_MESSAGE, m.to_json_value()?),
        }
        Ok(Value::Object(obj))
    }

    fn from_json_value(value: &Value) -> JsonResult<Self> {
        let obj = as_object(value, "SendStreamingMessage result")?;
        if let Some(v) = obj.get(STREAM_RESPONSE_TYPE_TASK) {
            return Ok(StreamEvent::Task(Task::from_json_value(v)?));
        }
        if let Some(v) = obj.get(JSON_FIELD_MESSAGE) {
            return Ok(StreamEvent::Message(Message::from_json_value(v)?));
        }
        if let Some(v) = obj.get(STREAM_RESULT_KEY_STATUS_UPDATE) {
            return Ok(StreamEvent::StatusUpdate(TaskStatusUpdateEvent::from_json_value(
                v,
            )?));
        }
        if let Some(v) = obj.get(STREAM_RESULT_KEY_ARTIFACT_UPDATE) {
            return Ok(StreamEvent::ArtifactUpdate(
                TaskArtifactUpdateEvent::from_json_value(v)?,
            ));
        }
        Err("result deserialize field".to_string())
    }
}
impl_wire_serde!(StreamEvent);

/// Build a JSON-RPC success response whose result is a wire type.
pub fn success_response<T: Serialize>(
    id: Option<ap_jsonrpc::RequestId>,
    result: &T,
) -> JsonResult<ap_jsonrpc::Response> {
    let value = serde_json::to_value(result).map_err(|e| e.to_string())?;
    Ok(ap_jsonrpc::Response {
        jsonrpc: ap_jsonrpc::JSONRPC_VERSION.to_string(),
        id,
        result: Some(value),
        error: None,
    })
}
