// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! MCP wire types and configuration structs, port of `mcp_type.h`.
//!
//! Field names follow the protocol (camelCase on the wire). Where the C++
//! SDK carries JSON as `std::string` (tool arguments, schemas, structured
//! content, `_meta`, elicitation schemas, experimental capabilities) this
//! port uses [`serde_json::Value`] or [`MetaMap`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub use ap_jsonrpc::RequestId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::auth::{Authenticator, Authorizer};

/// A JSON object carried in `_meta` style fields.
pub type MetaMap = Map<String, Value>;

/// Default server name.
pub const DEFAULT_SERVER_NAME: &str = "MCP Server";
/// Default client name.
pub const DEFAULT_CLIENT_NAME: &str = "MCP Client";
/// Default implementation version.
pub const DEFAULT_VERSION: &str = "1.0.0";
/// Default request timeout in milliseconds.
pub const DEFAULT_TIMEOUT: u64 = 30_000;
/// Default page size for `tools/list`.
pub const DEFAULT_TOOLS_PAGE_SIZE: usize = 50;
/// Default page size for `resources/list`.
pub const DEFAULT_RESOURCES_PAGE_SIZE: usize = 50;

// ---------------------------------------------------------------------------
// Progress token and request meta
// ---------------------------------------------------------------------------

/// MCP progress token: an integer or a string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProgressToken {
    /// Numeric token.
    Number(i64),
    /// String token.
    String(String),
}

impl From<i64> for ProgressToken {
    fn from(v: i64) -> Self {
        ProgressToken::Number(v)
    }
}

impl From<&str> for ProgressToken {
    fn from(v: &str) -> Self {
        ProgressToken::String(v.to_string())
    }
}

impl From<String> for ProgressToken {
    fn from(v: String) -> Self {
        ProgressToken::String(v)
    }
}

impl From<RequestId> for ProgressToken {
    fn from(id: RequestId) -> Self {
        match id {
            RequestId::Number(n) => ProgressToken::Number(n),
            RequestId::String(s) => ProgressToken::String(s),
        }
    }
}

impl ProgressToken {
    /// Map the token back to a request id the way `ClientSession` does:
    /// numeric strings become numeric ids.
    pub fn to_request_id(&self) -> RequestId {
        match self {
            ProgressToken::Number(n) => RequestId::Number(*n),
            ProgressToken::String(s) => match s.parse::<i64>() {
                Ok(n) => RequestId::Number(n),
                Err(_) => RequestId::String(s.clone()),
            },
        }
    }
}

impl fmt::Display for ProgressToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgressToken::Number(n) => write!(f, "{n}"),
            ProgressToken::String(s) => write!(f, "{s}"),
        }
    }
}

/// Optional `_meta` object on request params.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestParamsMeta {
    /// Token the receiver should use in `notifications/progress`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_token: Option<ProgressToken>,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration of the stdio client transport.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StdioClientConfig {
    /// Command to spawn. Empty means talk over the process's own stdio.
    pub command: String,
    /// Command line arguments.
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    pub env: HashMap<String, String>,
}

/// TLS settings shared by client and server transports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    /// Enable TLS on the server. The client applies certificate fields whenever the URL is https.
    pub enabled: bool,
    /// PEM file with trusted CA certificates.
    pub ca_file: String,
    /// PEM certificate (server certificate, or client certificate for mutual TLS).
    pub cert_file: String,
    /// PEM private key for `cert_file`.
    pub key_file: String,
    /// Server name for SNI and certificate matching.
    pub server_name: String,
    /// Verify the peer certificate.
    pub verify_peer: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ca_file: String::new(),
            cert_file: String::new(),
            key_file: String::new(),
            server_name: String::new(),
            verify_peer: true,
        }
    }
}

/// Configuration of the Streamable HTTP client transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamableHttpClientConfig {
    /// MCP endpoint URL, for example `http://127.0.0.1:8000/mcp`.
    pub endpoint: String,
    /// Connection timeout.
    pub timeout: Duration,
    /// Request and SSE read timeout.
    pub sse_timeout: Duration,
    /// Extra headers added to every request.
    pub headers: HashMap<String, String>,
    /// TLS settings.
    pub tls_config: TlsConfig,
}

impl Default for StreamableHttpClientConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT),
            sse_timeout: Duration::from_millis(DEFAULT_TIMEOUT),
            headers: HashMap::new(),
            tls_config: TlsConfig::default(),
        }
    }
}

/// Configuration of the Streamable HTTP server transport.
#[derive(Clone, Default)]
pub struct StreamableHttpServerConfig {
    /// Listen URL, for example `http://127.0.0.1:8000/mcp`.
    pub endpoint: String,
    /// Respond to POST requests with JSON instead of an SSE stream.
    pub is_json_response_enabled: bool,
    /// Handle every HTTP request independently without sessions.
    pub stateless: bool,
    /// Number of I/O threads in the C++ SDK. Validated but the runtime is tokio.
    pub io_threads: u32,
    /// TLS settings.
    pub tls_config: TlsConfig,
    /// Optional authenticator run before every request.
    pub authenticator: Option<Arc<dyn Authenticator>>,
    /// Optional authorizer run after successful authentication.
    pub authorizer: Option<Arc<dyn Authorizer>>,
}

impl StreamableHttpServerConfig {
    /// Configuration with the C++ defaults and the given endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            io_threads: 1,
            ..Default::default()
        }
    }
}

impl fmt::Debug for StreamableHttpServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamableHttpServerConfig")
            .field("endpoint", &self.endpoint)
            .field("is_json_response_enabled", &self.is_json_response_enabled)
            .field("stateless", &self.stateless)
            .field("io_threads", &self.io_threads)
            .field("tls_config", &self.tls_config)
            .field("authenticator", &self.authenticator.is_some())
            .field("authorizer", &self.authorizer.is_some())
            .finish()
    }
}

/// Client identity used in the `initialize` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// Client name.
    pub name: String,
    /// Client version.
    pub version: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_CLIENT_NAME.to_string(),
            version: DEFAULT_VERSION.to_string(),
        }
    }
}

/// Server configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerConfig {
    /// Server name.
    pub name: String,
    /// Server version. Must contain a dot.
    pub version: String,
    /// Worker thread count in the C++ SDK. Validated but the runtime is tokio.
    pub worker_threads: u32,
    /// Page size for `tools/list`.
    pub tools_page_size: usize,
    /// Page size for `resources/list`.
    pub resources_page_size: usize,
    /// Capabilities advertised during `initialize`.
    pub capabilities: ServerCapabilities,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_SERVER_NAME.to_string(),
            version: DEFAULT_VERSION.to_string(),
            worker_threads: 1,
            tools_page_size: DEFAULT_TOOLS_PAGE_SIZE,
            resources_page_size: DEFAULT_RESOURCES_PAGE_SIZE,
            capabilities: ServerCapabilities::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Server capabilities
// ---------------------------------------------------------------------------

/// Logging capability marker, serialized as `{}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingCapabilities {}

/// Prompts capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapabilities {
    /// Server sends `notifications/prompts/list_changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Resources capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapabilities {
    /// Server supports `resources/subscribe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    /// Server sends `notifications/resources/list_changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Tools capability.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapabilities {
    /// Server sends `notifications/tools/list_changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Capabilities a server advertises in `initialize`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// Experimental, non standard capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<MetaMap>,
    /// Logging support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingCapabilities>,
    /// Prompt support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapabilities>,
    /// Resource support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapabilities>,
    /// Tool support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapabilities>,
}

// ---------------------------------------------------------------------------
// Client capabilities (custom wire shape)
// ---------------------------------------------------------------------------

/// Client sampling capability flags, serialized as `{"context":{},"tools":{}}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SamplingCapability {
    /// Client accepts `includeContext` values other than `none`.
    pub context: bool,
    /// Client accepts tool enabled sampling requests.
    pub tools: bool,
}

impl Serialize for SamplingCapability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut obj = Map::new();
        if self.context {
            obj.insert("context".into(), json!({}));
        }
        if self.tools {
            obj.insert("tools".into(), json!({}));
        }
        Value::Object(obj).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SamplingCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        Ok(Self {
            context: v.get("context").map(Value::is_object).unwrap_or(false),
            tools: v.get("tools").map(Value::is_object).unwrap_or(false),
        })
    }
}

/// Form elicitation marker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormElicitationCapability {}

/// URL elicitation marker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlElicitationCapability {}

/// Client elicitation capability. Serialized as `{}` like the C++ SDK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElicitationCapability {
    /// Form mode support.
    pub form: Option<FormElicitationCapability>,
    /// URL mode support.
    pub url: Option<UrlElicitationCapability>,
}

impl Serialize for ElicitationCapability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Value::Object(Map::new()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ElicitationCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        Ok(Self {
            form: v
                .get("form")
                .filter(|f| f.is_object())
                .map(|_| FormElicitationCapability {}),
            url: v
                .get("url")
                .filter(|f| f.is_object())
                .map(|_| UrlElicitationCapability {}),
        })
    }
}

/// Client roots capability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootsCapability {
    /// Client sends `notifications/roots/list_changed`.
    pub list_changed: bool,
}

impl Serialize for RootsCapability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut obj = Map::new();
        if self.list_changed {
            obj.insert("listChanged".into(), Value::Bool(true));
        }
        Value::Object(obj).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RootsCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        Ok(Self {
            list_changed: v.get("listChanged").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Client tasks capability, flattened form of the nested schema.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientTasksCapability {
    /// `tasks.list`
    pub list: bool,
    /// `tasks.cancel`
    pub cancel: bool,
    /// `tasks.requests.sampling.createMessage`
    pub sampling_create_message: bool,
    /// `tasks.requests.elicitation.create`
    pub elicitation_create: bool,
}

impl Serialize for ClientTasksCapability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut obj = Map::new();
        if self.list {
            obj.insert("list".into(), json!({}));
        }
        if self.cancel {
            obj.insert("cancel".into(), json!({}));
        }
        if self.sampling_create_message || self.elicitation_create {
            let mut requests = Map::new();
            if self.sampling_create_message {
                requests.insert("sampling".into(), json!({"createMessage": {}}));
            }
            if self.elicitation_create {
                requests.insert("elicitation".into(), json!({"create": {}}));
            }
            obj.insert("requests".into(), Value::Object(requests));
        }
        Value::Object(obj).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClientTasksCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        let mut caps = Self::default();
        if !v.is_object() {
            return Ok(caps);
        }
        caps.list = v.get("list").map(Value::is_object).unwrap_or(false);
        caps.cancel = v.get("cancel").map(Value::is_object).unwrap_or(false);
        if let Some(req) = v.get("requests").filter(|r| r.is_object()) {
            caps.sampling_create_message = req
                .get("sampling")
                .and_then(|s| s.get("createMessage"))
                .map(Value::is_object)
                .unwrap_or(false);
            caps.elicitation_create = req
                .get("elicitation")
                .and_then(|s| s.get("create"))
                .map(Value::is_object)
                .unwrap_or(false);
        }
        Ok(caps)
    }
}

/// Capabilities a client advertises in `initialize`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientCapabilities {
    /// Experimental capabilities. Not serialized by the C++ SDK; this port
    /// includes them when set.
    pub experimental: Option<MetaMap>,
    /// Sampling support.
    pub sampling: Option<SamplingCapability>,
    /// Elicitation support.
    pub elicitation: Option<ElicitationCapability>,
    /// Roots support.
    pub roots: Option<RootsCapability>,
    /// Tasks support.
    pub tasks: Option<ClientTasksCapability>,
}

impl Serialize for ClientCapabilities {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut obj = Map::new();
        if let Some(e) = &self.experimental {
            obj.insert("experimental".into(), Value::Object(e.clone()));
        }
        if let Some(s) = &self.sampling {
            obj.insert(
                "sampling".into(),
                serde_json::to_value(s).map_err(serde::ser::Error::custom)?,
            );
        }
        if self.elicitation.is_some() {
            obj.insert("elicitation".into(), json!({}));
        }
        if let Some(t) = &self.tasks {
            obj.insert(
                "tasks".into(),
                serde_json::to_value(t).map_err(serde::ser::Error::custom)?,
            );
        }
        if let Some(r) = &self.roots {
            obj.insert(
                "roots".into(),
                serde_json::to_value(r).map_err(serde::ser::Error::custom)?,
            );
        }
        Value::Object(obj).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClientCapabilities {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        let mut caps = Self::default();
        let Value::Object(obj) = v else {
            return Ok(caps);
        };
        if let Some(Value::Object(e)) = obj.get("experimental") {
            caps.experimental = Some(e.clone());
        }
        if let Some(s) = obj.get("sampling").filter(|s| s.is_object()) {
            caps.sampling = serde_json::from_value(s.clone()).ok();
        }
        if let Some(e) = obj.get("elicitation").filter(|e| e.is_object()) {
            caps.elicitation = serde_json::from_value(e.clone()).ok();
        }
        if let Some(t) = obj.get("tasks").filter(|t| t.is_object()) {
            caps.tasks = serde_json::from_value(t.clone()).ok();
        }
        if let Some(r) = obj.get("roots").filter(|r| r.is_object()) {
            caps.roots = serde_json::from_value(r.clone()).ok();
        }
        Ok(caps)
    }
}

// ---------------------------------------------------------------------------
// Implementation info
// ---------------------------------------------------------------------------

/// Name and version of a client or server (`Implementation`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    /// Implementation name.
    #[serde(default)]
    pub name: String,
    /// Human readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Implementation version.
    #[serde(default)]
    pub version: String,
    /// Website URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
}

impl Implementation {
    /// Build an implementation description from name and version.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            ..Default::default()
        }
    }
}

/// `Mcp::ClientInfo`, the same shape as [`Implementation`].
pub type ClientInfo = Implementation;
/// `Mcp::ServerInfo`, the same shape as [`Implementation`].
pub type ServerInfo = Implementation;

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

/// Who authored a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleType {
    /// A human user.
    #[default]
    User,
    /// An automated assistant.
    Assistant,
}

impl<'de> Deserialize<'de> for RoleType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "assistant" => RoleType::Assistant,
            _ => RoleType::User,
        })
    }
}

impl RoleType {
    /// Wire name of the role.
    pub const fn as_str(self) -> &'static str {
        match self {
            RoleType::User => "user",
            RoleType::Assistant => "assistant",
        }
    }
}

/// Annotations attached to content and resources.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    /// Intended audience.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<RoleType>>,
    /// ISO 8601 timestamp of the last modification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Priority between 0 and 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
}

/// Icon description.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Icon {
    /// Icon URL or data URI.
    pub src: String,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Sizes such as `32x32`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    /// `light` or `dark`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// Text content block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    /// The text.
    #[serde(default)]
    pub text: String,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

impl TextContent {
    /// Build a text block.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            annotations: None,
        }
    }
}

/// Image content block with base64 data.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64 encoded image data.
    #[serde(default)]
    pub data: String,
    /// MIME type.
    pub mime_type: String,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Audio content block with base64 data.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioContent {
    /// Base64 encoded audio data.
    #[serde(default)]
    pub data: String,
    /// MIME type.
    pub mime_type: String,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Text resource contents.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextResourceContents {
    /// Resource URI.
    pub uri: String,
    /// The text.
    #[serde(default)]
    pub text: String,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Binary resource contents.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobResourceContents {
    /// Resource URI.
    pub uri: String,
    /// Base64 encoded bytes.
    #[serde(default)]
    pub blob: String,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Either text or binary resource contents. Selected by the `text` or `blob` field.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResourceContents {
    /// Text contents.
    Text(TextResourceContents),
    /// Binary contents.
    Blob(BlobResourceContents),
}

impl Default for ResourceContents {
    fn default() -> Self {
        ResourceContents::Text(TextResourceContents::default())
    }
}

impl<'de> Deserialize<'de> for ResourceContents {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        if v.get("text").is_some() {
            serde_json::from_value(v)
                .map(ResourceContents::Text)
                .map_err(serde::de::Error::custom)
        } else if v.get("blob").is_some() {
            serde_json::from_value(v)
                .map(ResourceContents::Blob)
                .map_err(serde::de::Error::custom)
        } else {
            Ok(ResourceContents::Text(TextResourceContents {
                uri: v
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ..Default::default()
            }))
        }
    }
}

/// Embedded resource content block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    /// The resource contents.
    pub resource: ResourceContents,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Resource link content block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLink {
    /// Resource URI.
    pub uri: String,
    /// Resource name.
    pub name: String,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Tool behaviour hints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The tool does not modify its environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// The tool may perform destructive updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Repeated calls have no additional effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// The tool interacts with the open world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

fn with_type(inner: &impl Serialize, type_name: &str) -> Result<Value, serde_json::Error> {
    let mut v = serde_json::to_value(inner)?;
    if let Value::Object(obj) = &mut v {
        obj.insert("type".into(), Value::String(type_name.into()));
        // Keep "type" first for readability of the wire format.
        let mut ordered = Map::new();
        ordered.insert("type".into(), Value::String(type_name.into()));
        for (k, val) in obj.iter() {
            if k != "type" {
                ordered.insert(k.clone(), val.clone());
            }
        }
        return Ok(Value::Object(ordered));
    }
    Ok(v)
}

fn from_value_or<T: DeserializeOwned, E: serde::de::Error>(v: Value) -> Result<T, E> {
    serde_json::from_value(v).map_err(E::custom)
}

/// Any content block returned by tools, prompts and resources.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentBlock {
    /// `"type": "text"`
    Text(TextContent),
    /// `"type": "image"`
    Image(ImageContent),
    /// `"type": "audio"`
    Audio(AudioContent),
    /// `"type": "resource_link"`
    ResourceLink(ResourceLink),
    /// `"type": "resource"`
    EmbeddedResource(EmbeddedResource),
}

impl ContentBlock {
    /// Build a text block.
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text(TextContent::new(text))
    }

    /// The wire type name.
    pub const fn type_name(&self) -> &'static str {
        match self {
            ContentBlock::Text(_) => "text",
            ContentBlock::Image(_) => "image",
            ContentBlock::Audio(_) => "audio",
            ContentBlock::ResourceLink(_) => "resource_link",
            ContentBlock::EmbeddedResource(_) => "resource",
        }
    }

    /// The text of a text block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text(t) => Some(&t.text),
            _ => None,
        }
    }
}

impl Default for ContentBlock {
    fn default() -> Self {
        ContentBlock::Text(TextContent::default())
    }
}

impl Serialize for ContentBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            ContentBlock::Text(c) => with_type(c, "text"),
            ContentBlock::Image(c) => with_type(c, "image"),
            ContentBlock::Audio(c) => with_type(c, "audio"),
            ContentBlock::ResourceLink(c) => with_type(c, "resource_link"),
            ContentBlock::EmbeddedResource(c) => with_type(c, "resource"),
        }
        .map_err(serde::ser::Error::custom)?;
        v.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        let type_name = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        match type_name.as_str() {
            "text" => from_value_or(v).map(ContentBlock::Text),
            "image" => from_value_or(v).map(ContentBlock::Image),
            "audio" => from_value_or(v).map(ContentBlock::Audio),
            "resource" => from_value_or(v).map(ContentBlock::EmbeddedResource),
            "resource_link" => from_value_or(v).map(ContentBlock::ResourceLink),
            // Unknown blocks are kept as text carrying the raw JSON, like the C++ SDK.
            _ => Ok(ContentBlock::Text(TextContent::new(v.to_string()))),
        }
    }
}

/// `Mcp::ContentType`, the C++ name of [`ContentBlock`].
pub type ContentType = ContentBlock;

/// Content block for a model initiated tool call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseContent {
    /// Tool call identifier.
    #[serde(default)]
    pub id: String,
    /// Tool name.
    #[serde(default)]
    pub name: String,
    /// Tool input.
    #[serde(default)]
    pub input: MetaMap,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Content block carrying a tool result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultContent {
    /// Id of the `tool_use` block being answered.
    #[serde(default)]
    pub tool_use_id: String,
    /// Result content.
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Structured result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// True when the tool failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Content block inside a sampling message.
#[derive(Clone, Debug, PartialEq)]
pub enum SamplingMessageContentBlock {
    /// Text.
    Text(TextContent),
    /// Image.
    Image(ImageContent),
    /// Audio.
    Audio(AudioContent),
    /// Tool use.
    ToolUse(ToolUseContent),
    /// Tool result.
    ToolResult(ToolResultContent),
}

impl SamplingMessageContentBlock {
    /// Build a text block.
    pub fn text(text: impl Into<String>) -> Self {
        SamplingMessageContentBlock::Text(TextContent::new(text))
    }

    /// The text of a text block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            SamplingMessageContentBlock::Text(t) => Some(&t.text),
            _ => None,
        }
    }
}

impl Serialize for SamplingMessageContentBlock {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let v = match self {
            SamplingMessageContentBlock::Text(c) => with_type(c, "text"),
            SamplingMessageContentBlock::Image(c) => with_type(c, "image"),
            SamplingMessageContentBlock::Audio(c) => with_type(c, "audio"),
            SamplingMessageContentBlock::ToolUse(c) => with_type(c, "tool_use"),
            SamplingMessageContentBlock::ToolResult(c) => with_type(c, "tool_result"),
        }
        .map_err(serde::ser::Error::custom)?;
        v.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SamplingMessageContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        let type_name = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        match type_name.as_str() {
            "text" => from_value_or(v).map(SamplingMessageContentBlock::Text),
            "image" => from_value_or(v).map(SamplingMessageContentBlock::Image),
            "audio" => from_value_or(v).map(SamplingMessageContentBlock::Audio),
            "tool_use" => from_value_or(v).map(SamplingMessageContentBlock::ToolUse),
            "tool_result" => from_value_or(v).map(SamplingMessageContentBlock::ToolResult),
            _ => Ok(SamplingMessageContentBlock::Text(TextContent::new(v.to_string()))),
        }
    }
}

/// Sampling message content: a single block or a list of blocks.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SamplingContent {
    /// One block, serialized as an object.
    Single(SamplingMessageContentBlock),
    /// Several blocks, serialized as an array.
    Multiple(Vec<SamplingMessageContentBlock>),
}

impl Default for SamplingContent {
    fn default() -> Self {
        SamplingContent::Multiple(Vec::new())
    }
}

impl SamplingContent {
    /// Build single text content.
    pub fn text(text: impl Into<String>) -> Self {
        SamplingContent::Single(SamplingMessageContentBlock::text(text))
    }

    /// The blocks as a list.
    pub fn as_list(&self) -> Vec<SamplingMessageContentBlock> {
        match self {
            SamplingContent::Single(b) => vec![b.clone()],
            SamplingContent::Multiple(list) => list.clone(),
        }
    }

    /// The first text block, if any.
    pub fn first_text(&self) -> Option<String> {
        self.as_list()
            .iter()
            .find_map(|b| b.as_text().map(|s| s.to_string()))
    }
}

impl<'de> Deserialize<'de> for SamplingContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        match v {
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(from_value_or(item)?);
                }
                Ok(SamplingContent::Multiple(out))
            }
            Value::Object(_) => from_value_or(v).map(SamplingContent::Single),
            _ => Ok(SamplingContent::Multiple(Vec::new())),
        }
    }
}

/// A message sent to or received from an LLM.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingMessage {
    /// Author role.
    #[serde(default)]
    pub role: RoleType,
    /// Content.
    #[serde(default)]
    pub content: SamplingContent,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

impl SamplingMessage {
    /// Build a text message.
    pub fn text(role: RoleType, text: impl Into<String>) -> Self {
        Self {
            role,
            content: SamplingContent::text(text),
            meta: None,
        }
    }
}

/// Model name hint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelHint {
    /// Substring of a model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Model selection preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferences {
    /// Ordered hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<ModelHint>>,
    /// Cost priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_priority: Option<f64>,
    /// Speed priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_priority: Option<f64>,
    /// Intelligence priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intelligence_priority: Option<f64>,
}

/// Tool choice mode for sampling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolChoice {
    /// `none`, `required` or `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// Result of `sampling/createMessage`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMessageResult {
    /// Model that produced the message.
    #[serde(default)]
    pub model: String,
    /// Why generation stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Author role, normally `assistant`.
    #[serde(default = "assistant_role")]
    pub role: RoleType,
    /// Content.
    #[serde(default)]
    pub content: SamplingContent,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

fn assistant_role() -> RoleType {
    RoleType::Assistant
}

/// Tool description.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Tool name.
    pub name: String,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON schema of the arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// JSON schema of `structuredContent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Behaviour hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
}

/// Parameters of `sampling/createMessage`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMessageParams {
    /// Conversation so far.
    #[serde(default)]
    pub messages: Vec<SamplingMessage>,
    /// Model preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<ModelPreferences>,
    /// System prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// `none`, `thisServer` or `allServers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_context: Option<String>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: i64,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Provider specific metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetaMap>,
    /// Tools available to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Tool choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

fn deserialize_content_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ContentBlock>, D::Error> {
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(from_value_or(item)?);
            }
            Ok(out)
        }
        // Compatibility: a plain string is wrapped as text content.
        Value::String(s) => Ok(vec![ContentBlock::text(s)]),
        _ => Ok(Vec::new()),
    }
}

/// Result of `tools/call`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// Unstructured content.
    #[serde(default, deserialize_with = "deserialize_content_list")]
    pub content: Vec<ContentBlock>,
    /// Structured content matching the tool's `outputSchema`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// True when the tool reports a failure.
    #[serde(default)]
    pub is_error: bool,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

impl CallToolResult {
    /// A successful result with one text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            ..Default::default()
        }
    }

    /// An error result with one text block.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: true,
            ..Default::default()
        }
    }

    /// The C++ `ToolReturn` string form: structured content plus its text rendering.
    pub fn from_structured(value: Value) -> Self {
        let text = match &value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        Self {
            content: vec![ContentBlock::text(text)],
            structured_content: Some(value),
            ..Default::default()
        }
    }
}

/// Result of `tools/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    /// Tools on this page.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Cursor of the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

fn deserialize_prompt_content<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ContentBlock, D::Error> {
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Array(mut items) => {
            if items.is_empty() {
                Ok(ContentBlock::default())
            } else {
                from_value_or(items.remove(0))
            }
        }
        Value::Object(_) => from_value_or(v),
        _ => Ok(ContentBlock::default()),
    }
}

/// A message inside a prompt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessage {
    /// Author role.
    #[serde(default)]
    pub role: RoleType,
    /// Content. Accepts an object or the first element of an array.
    #[serde(default, deserialize_with = "deserialize_prompt_content")]
    pub content: ContentBlock,
}

/// Prompt argument description.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptArgument {
    /// Argument name.
    pub name: String,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the argument is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl PromptArgument {
    /// Build an argument description.
    pub fn new(name: impl Into<String>, description: impl Into<String>, required: bool) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
            required: Some(required),
            title: None,
        }
    }
}

/// Result of `prompts/get`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptResult {
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Messages.
    pub messages: Vec<PromptMessage>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Prompt description.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInfo {
    /// Prompt name.
    pub name: String,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// Arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// Result of `initialize`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Negotiated protocol version.
    #[serde(default = "default_protocol_version")]
    pub protocol_version: String,
    /// Server capabilities.
    #[serde(default)]
    pub capabilities: ServerCapabilities,
    /// Server identity.
    #[serde(default)]
    pub server_info: Implementation,
    /// Usage instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

fn default_protocol_version() -> String {
    crate::protocol::DEFAULT_PROTOCOL_VERSION.to_string()
}

impl Default for InitializeResult {
    fn default() -> Self {
        Self {
            protocol_version: default_protocol_version(),
            capabilities: ServerCapabilities::default(),
            server_info: Implementation::default(),
            instructions: None,
            meta: None,
        }
    }
}

impl InitializeResult {
    /// Build a result, mirroring the C++ constructor.
    pub fn new(
        protocol_version: impl Into<String>,
        capabilities: ServerCapabilities,
        server_info: Implementation,
        instructions: Option<String>,
    ) -> Self {
        Self {
            protocol_version: protocol_version.into(),
            capabilities,
            server_info,
            instructions,
            meta: None,
        }
    }
}

/// Result of `prompts/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPromptsResult {
    /// Prompts.
    #[serde(default)]
    pub prompts: Vec<PromptInfo>,
    /// Cursor of the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// A root exposed by the client.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    /// Human readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// URI, normally `file://`.
    pub uri: String,
}

/// Result of `roots/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListRootsResult {
    /// Roots.
    #[serde(default)]
    pub roots: Vec<Root>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Result of `elicitation/create`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ElicitResult {
    /// `accept`, `decline` or `cancel`.
    #[serde(default)]
    pub action: String,
    /// Collected values.
    #[serde(default)]
    pub content: MetaMap,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// A result that carries no data.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmptyResult {
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Resource description.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    /// URI.
    pub uri: String,
    /// Name.
    pub name: String,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// Annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Resource template description.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    /// URI template.
    pub uri_template: String,
    /// Name.
    pub name: String,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Icons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// Annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// Result of `resources/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourcesResult {
    /// Resources on this page.
    #[serde(default)]
    pub resources: Vec<ResourceInfo>,
    /// Cursor of the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Result of `resources/read`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadResourceResult {
    /// Contents.
    #[serde(default)]
    pub contents: Vec<ResourceContents>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// Result of `resources/templates/list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourceTemplatesResult {
    /// Templates.
    #[serde(default)]
    pub resource_templates: Vec<ResourceTemplate>,
    /// Cursor of the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

/// MCP protocol logging levels used by `logging/setLevel` and `notifications/message`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoggingLevel {
    /// debug
    Debug,
    /// info
    Info,
    /// notice
    Notice,
    /// warning
    Warning,
    /// error
    Error,
    /// critical
    Critical,
    /// alert
    Alert,
    /// emergency
    Emergency,
}

impl LoggingLevel {
    /// Wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            LoggingLevel::Debug => "debug",
            LoggingLevel::Info => "info",
            LoggingLevel::Notice => "notice",
            LoggingLevel::Warning => "warning",
            LoggingLevel::Error => "error",
            LoggingLevel::Critical => "critical",
            LoggingLevel::Alert => "alert",
            LoggingLevel::Emergency => "emergency",
        }
    }

    /// Parse a wire name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "debug" => LoggingLevel::Debug,
            "info" => LoggingLevel::Info,
            "notice" => LoggingLevel::Notice,
            "warning" => LoggingLevel::Warning,
            "error" => LoggingLevel::Error,
            "critical" => LoggingLevel::Critical,
            "alert" => LoggingLevel::Alert,
            "emergency" => LoggingLevel::Emergency,
            _ => return None,
        })
    }
}

impl fmt::Display for LoggingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

/// Reference to a resource template.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceTemplateReference {
    /// URI template.
    pub uri: String,
}

/// Reference to a prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptReference {
    /// Prompt name.
    pub name: String,
}

/// What a completion request refers to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CompleteReference {
    /// `ref/resource`
    #[serde(rename = "ref/resource")]
    Resource(ResourceTemplateReference),
    /// `ref/prompt`
    #[serde(rename = "ref/prompt")]
    Prompt(PromptReference),
}

/// Argument being completed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionArgument {
    /// Argument name.
    pub name: String,
    /// Current value.
    pub value: String,
}

/// Additional completion context.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionContext {
    /// Already resolved arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<HashMap<String, String>>,
}

/// Completion values.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    /// Suggested values.
    pub values: Vec<String>,
    /// Total number of matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    /// More values exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

/// Result of `completion/complete`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompleteResult {
    /// The completion.
    pub completion: Completion,
    /// Optional meta.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaMap>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[test]
    fn client_capabilities_wire_shape() -> TestResult {
        let caps = ClientCapabilities {
            sampling: Some(SamplingCapability {
                context: true,
                tools: true,
            }),
            elicitation: Some(ElicitationCapability {
                form: Some(FormElicitationCapability {}),
                url: None,
            }),
            roots: Some(RootsCapability { list_changed: true }),
            tasks: Some(ClientTasksCapability {
                sampling_create_message: true,
                ..Default::default()
            }),
            experimental: None,
        };
        let v = serde_json::to_value(&caps)?;
        assert_eq!(v["sampling"], json!({"context": {}, "tools": {}}));
        assert_eq!(v["elicitation"], json!({}));
        assert_eq!(v["roots"], json!({"listChanged": true}));
        assert_eq!(
            v["tasks"],
            json!({"requests": {"sampling": {"createMessage": {}}}})
        );
        let back: ClientCapabilities = serde_json::from_value(v)?;
        assert!(back.sampling.required()?.context);
        assert!(back.elicitation.is_some());
        assert!(back.roots.required()?.list_changed);
        assert!(back.tasks.required()?.sampling_create_message);

        let empty: ClientCapabilities = serde_json::from_value(json!({}))?;
        assert_eq!(empty, ClientCapabilities::default());
        assert_eq!(serde_json::to_value(&empty)?, json!({}));
        let roots_false = serde_json::to_value(RootsCapability { list_changed: false })?;
        assert_eq!(roots_false, json!({}));
        Ok(())
    }

    #[test]
    fn server_capabilities_wire_shape() -> TestResult {
        let caps = ServerCapabilities {
            logging: Some(LoggingCapabilities {}),
            prompts: Some(PromptsCapabilities {
                list_changed: Some(true),
            }),
            resources: Some(ResourcesCapabilities {
                subscribe: Some(false),
                list_changed: Some(false),
            }),
            tools: Some(ToolsCapabilities {
                list_changed: Some(true),
            }),
            experimental: None,
        };
        let v = serde_json::to_value(&caps)?;
        assert_eq!(v["logging"], json!({}));
        assert_eq!(v["prompts"]["listChanged"], json!(true));
        assert_eq!(v["resources"]["subscribe"], json!(false));
        assert_eq!(v["tools"]["listChanged"], json!(true));
        let back: ServerCapabilities = serde_json::from_value(v)?;
        assert_eq!(back, caps);
        Ok(())
    }

    #[test]
    fn content_block_roundtrip_and_fallback() -> TestResult {
        let blocks = vec![
            ContentBlock::text("hi"),
            ContentBlock::Image(ImageContent {
                data: "abc".into(),
                mime_type: "image/png".into(),
                annotations: None,
            }),
            ContentBlock::Audio(AudioContent {
                data: "abc".into(),
                mime_type: "audio/wav".into(),
                annotations: None,
            }),
            ContentBlock::ResourceLink(ResourceLink {
                uri: "test".into(),
                name: "test".into(),
                size: Some(1024),
                ..Default::default()
            }),
            ContentBlock::EmbeddedResource(EmbeddedResource {
                resource: ResourceContents::Blob(BlobResourceContents {
                    uri: "u".into(),
                    blob: "AQID".into(),
                    mime_type: Some("application/octet-stream".into()),
                }),
                annotations: None,
            }),
        ];
        let v = serde_json::to_value(&blocks)?;
        assert_eq!(v[0], json!({"type": "text", "text": "hi"}));
        assert_eq!(v[3]["type"], json!("resource_link"));
        assert_eq!(v[4]["type"], json!("resource"));
        assert_eq!(v[4]["resource"]["blob"], json!("AQID"));
        let back: Vec<ContentBlock> = serde_json::from_value(v)?;
        assert_eq!(back, blocks);

        let unknown: ContentBlock = serde_json::from_value(json!({"type": "video", "x": 1}))?;
        assert_eq!(unknown.as_text().required()?, r#"{"type":"video","x":1}"#);
        let embedded: EmbeddedResource =
            serde_json::from_value(json!({"resource": {"uri": "u", "text": "t"}, "annotations": null}))?;
        assert_eq!(
            embedded.resource,
            ResourceContents::Text(TextResourceContents {
                uri: "u".into(),
                text: "t".into(),
                mime_type: None
            })
        );
        Ok(())
    }

    #[test]
    fn call_tool_result_content_string_compat() -> TestResult {
        let r: CallToolResult = serde_json::from_value(json!({"content": "plain"}))?;
        assert_eq!(r.content[0].as_text(), Some("plain"));
        assert!(!r.is_error);
        let r: CallToolResult = serde_json::from_value(
            json!({"content": [{"type": "text", "text": "a"}], "structuredContent": {"k": 1},
                "isError": true, "_meta": {"trace": "x"}}),
        )?;
        assert!(r.is_error);
        assert_eq!(r.structured_content, Some(json!({"k": 1})));
        assert_eq!(r.meta.required()?["trace"], json!("x"));
        let v = serde_json::to_value(CallToolResult::text("x"))?;
        assert_eq!(v["isError"], json!(false));
        let structured = CallToolResult::from_structured(json!({"status": "ok"}));
        assert_eq!(structured.content[0].as_text(), Some(r#"{"status":"ok"}"#));
        Ok(())
    }

    #[test]
    fn sampling_message_single_or_multiple() -> TestResult {
        let single: SamplingMessage =
            serde_json::from_value(json!({"role": "user", "content": {"type": "text", "text": "hi"}}))?;
        assert!(matches!(single.content, SamplingContent::Single(_)));
        assert_eq!(single.content.first_text().required()?, "hi");
        let multiple: SamplingMessage = serde_json::from_value(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "1", "name": "t", "input": {"a": 1}},
            {"type": "weird"}
        ]}))?;
        assert_eq!(multiple.role, RoleType::Assistant);
        let list = multiple.content.as_list();
        assert!(matches!(list[0], SamplingMessageContentBlock::ToolUse(_)));
        assert!(matches!(list[1], SamplingMessageContentBlock::Text(_)));
        let missing: SamplingMessage = serde_json::from_value(json!({"role": "user"}))?;
        assert_eq!(missing.content, SamplingContent::Multiple(vec![]));
        let wrong: SamplingMessage = serde_json::from_value(json!({"role": "user", "content": 5}))?;
        assert_eq!(wrong.content, SamplingContent::Multiple(vec![]));
        let unknown_role: SamplingMessage = serde_json::from_value(json!({"role": "system"}))?;
        assert_eq!(unknown_role.role, RoleType::User);

        let out = serde_json::to_value(SamplingMessage::text(RoleType::User, "x"))?;
        assert_eq!(
            out,
            json!({"role": "user", "content": {"type": "text", "text": "x"}})
        );
        Ok(())
    }

    #[test]
    fn create_message_result_defaults() -> TestResult {
        let r: CreateMessageResult = serde_json::from_value(json!({"model": "m"}))?;
        assert_eq!(r.role, RoleType::Assistant);
        assert_eq!(r.content, SamplingContent::Multiple(vec![]));
        let v = serde_json::to_value(CreateMessageResult {
            model: "m".into(),
            role: RoleType::Assistant,
            content: SamplingContent::text("ok"),
            stop_reason: Some("stop".into()),
            meta: None,
        })?;
        assert_eq!(v["role"], json!("assistant"));
        assert_eq!(v["stopReason"], json!("stop"));
        assert_eq!(v["content"]["text"], json!("ok"));
        Ok(())
    }

    #[test]
    fn tool_schemas_are_json_values() -> TestResult {
        let tool: Tool = serde_json::from_value(json!({"name": "echo", "inputSchema": {"type": "object"},
            "outputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}))?;
        assert_eq!(tool.input_schema, Some(json!({"type": "object"})));
        assert_eq!(tool.annotations.as_ref().required()?.read_only_hint, Some(true));
        let v = serde_json::to_value(&tool)?;
        assert_eq!(v["outputSchema"], json!({"type": "object"}));
        assert!(v.get("title").is_none());
        Ok(())
    }

    #[test]
    fn prompt_message_content_object_or_array() -> TestResult {
        let obj: PromptMessage =
            serde_json::from_value(json!({"role": "user", "content": {"type": "text", "text": "a"}}))?;
        assert_eq!(obj.content.as_text(), Some("a"));
        let arr: PromptMessage =
            serde_json::from_value(json!({"role": "user", "content": [{"type": "text", "text": "b"}]}))?;
        assert_eq!(arr.content.as_text(), Some("b"));
        let v = serde_json::to_value(&obj)?;
        assert_eq!(v["content"]["type"], json!("text"));
        let prompt: PromptInfo = serde_json::from_value(json!({"name": "p", "description": null,
            "arguments": [{"name": "a", "required": true}, {"name": "b", "title": null}]}))?;
        assert!(prompt.description.is_none());
        let args = prompt.arguments.required()?;
        assert_eq!(args[0].required, Some(true));
        assert!(args[1].title.is_none());
        Ok(())
    }

    #[test]
    fn initialize_result_defaults_and_meta() -> TestResult {
        let r: InitializeResult = serde_json::from_value(json!({}))?;
        assert_eq!(r.protocol_version, crate::protocol::DEFAULT_PROTOCOL_VERSION);
        let r: InitializeResult = serde_json::from_value(json!({"protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}}, "serverInfo": {"name": "s", "version": "1.0"},
            "instructions": "Welcome!", "_meta": {"traceId": "abc"}}))?;
        assert_eq!(r.server_info.name, "s");
        assert_eq!(r.instructions.as_deref(), Some("Welcome!"));
        assert_eq!(r.meta.required()?["traceId"], json!("abc"));
        assert!(r.capabilities.tools.is_some());
        Ok(())
    }

    #[test]
    fn roots_and_elicit_results() -> TestResult {
        let v = serde_json::to_value(ListRootsResult {
            roots: vec![
                Root {
                    uri: "file:///tmp".into(),
                    name: Some("tmp".into()),
                },
                Root {
                    uri: "file:///home".into(),
                    name: None,
                },
            ],
            meta: Some(json!({"traceId": "abc"}).as_object().required()?.clone()),
        })?;
        assert_eq!(v["roots"][0]["name"], json!("tmp"));
        assert!(v["roots"][1].get("name").is_none());
        assert_eq!(v["_meta"]["traceId"], json!("abc"));
        let e: ElicitResult = serde_json::from_value(json!({"action": "accept", "content": {"a": 1}}))?;
        assert_eq!(e.action, "accept");
        assert_eq!(e.content["a"], json!(1));
        assert_eq!(serde_json::to_value(EmptyResult::default())?, json!({}));
        Ok(())
    }

    #[test]
    fn completion_reference_tagging() -> TestResult {
        let prompt = CompleteReference::Prompt(PromptReference { name: "p".into() });
        let v = serde_json::to_value(&prompt)?;
        assert_eq!(v, json!({"type": "ref/prompt", "name": "p"}));
        let back: CompleteReference = serde_json::from_value(json!({"type": "ref/resource", "uri": "u"}))?;
        assert_eq!(
            back,
            CompleteReference::Resource(ResourceTemplateReference { uri: "u".into() })
        );
        let c: CompleteResult = serde_json::from_value(json!({"completion": {"values": ["a"], "total": 1,
            "hasMore": false}}))?;
        assert_eq!(c.completion.total, Some(1));
        Ok(())
    }

    #[test]
    fn resource_contents_selection() -> TestResult {
        let list: Vec<ResourceContents> = serde_json::from_value(json!([
            {"uri": "a", "text": "hello", "mimeType": "text/plain"},
            {"uri": "b", "blob": "AQIDBA=="},
            {"uri": "c"}
        ]))?;
        assert!(matches!(&list[0], ResourceContents::Text(t) if t.text == "hello"));
        assert!(
            matches!(&list[1], ResourceContents::Blob(b) if b.blob == "AQIDBA==" && b.mime_type.is_none())
        );
        assert!(matches!(&list[2], ResourceContents::Text(t) if t.uri == "c"));
        Ok(())
    }

    #[test]
    fn progress_token_and_logging_level() -> TestResult {
        let n: ProgressToken = serde_json::from_value(json!(42))?;
        assert_eq!(n, ProgressToken::Number(42));
        let s: ProgressToken = serde_json::from_value(json!("7"))?;
        assert_eq!(s.to_request_id(), RequestId::Number(7));
        assert_eq!(
            ProgressToken::from("abc").to_request_id(),
            RequestId::String("abc".into())
        );
        assert_eq!(serde_json::to_value(LoggingLevel::Warning)?, json!("warning"));
        assert_eq!(LoggingLevel::parse("emergency"), Some(LoggingLevel::Emergency));
        assert_eq!(LoggingLevel::parse("nope"), None);
        assert_eq!(LoggingLevel::Debug.to_string(), "debug");
        Ok(())
    }

    #[test]
    fn config_defaults() -> TestResult {
        let server = ServerConfig::default();
        assert_eq!(server.name, "MCP Server");
        assert_eq!(server.version, "1.0.0");
        assert_eq!(server.worker_threads, 1);
        assert_eq!(server.tools_page_size, 50);
        let client = ClientConfig::default();
        assert_eq!(client.name, "MCP Client");
        let http = StreamableHttpServerConfig::default();
        assert!(http.endpoint.is_empty());
        assert!(!http.is_json_response_enabled);
        assert!(!http.tls_config.enabled);
        assert_eq!(StreamableHttpServerConfig::new("http://x:1").io_threads, 1);
        let c = StreamableHttpClientConfig::default();
        assert_eq!(c.timeout, Duration::from_millis(30_000));
        assert!(TlsConfig::default().verify_peer);
        Ok(())
    }
}
