// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Thin reqwest wrapper: the SDK's only network exit.
//!
//! [`Transport::request`] returns 2xx responses as a [`Response`] and maps
//! 4xx / 5xx bodies and transport failures to [`ClientError`] through the
//! same table the Python `transport.py` uses.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::errors::{ClientError, wrap_http_error};
use crate::internal::{build_default_headers, normalize_base_url};

/// HTTP methods used by the registry API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
}

impl HttpMethod {
    fn as_reqwest(self) -> reqwest::Method {
        match self {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
        }
    }

    /// Upper-case method name.
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
        }
    }
}

/// A successful (status < 400) response with its body buffered.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Type` header, when present.
    pub content_type: Option<String>,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// Decode the body as JSON.
    pub fn json(&self) -> Result<Value, ClientError> {
        serde_json::from_slice(&self.body)
            .map_err(|e| ClientError::decode(format!("response body is not valid JSON: {e}")))
    }
}

/// Async HTTP transport backed by `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct Transport {
    client: reqwest::Client,
    base_url: String,
}

impl Transport {
    /// Build a transport. `base_url` gets a trailing slash so paths join under any mount point.
    ///
    /// Redirects are not followed, matching the `httpx` default.
    pub fn new(base_url: &str, timeout: Duration, api_key: Option<&str>) -> Result<Self, ClientError> {
        let mut headers = HeaderMap::new();
        if let Some((_, value)) = build_default_headers(api_key) {
            let hv = HeaderValue::from_str(&value)
                .map_err(|e| ClientError::invalid(format!("api_key is not a valid header value: {e}")))?;
            headers.insert(AUTHORIZATION, hv);
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ClientError::Connection {
                message: format!("failed to build HTTP client: {e}"),
            })?;
        Ok(Transport {
            client,
            base_url: normalize_base_url(base_url),
        })
    }

    /// Normalised base URL (always ends with `/`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Absolute URL for a relative path (leading slashes are stripped first).
    pub fn url_for(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path.trim_start_matches('/'))
    }

    /// Send one request. Bodies are JSON; `params` become query parameters.
    pub async fn request(
        &self,
        method: HttpMethod,
        path: &str,
        json: Option<&Value>,
        params: Option<&[(String, String)]>,
    ) -> Result<Response, ClientError> {
        let mut req = self.client.request(method.as_reqwest(), self.url_for(path));
        if let Some(body) = json {
            req = req.json(body);
        }
        if let Some(p) = params {
            if !p.is_empty() {
                req = req.query(p);
            }
        }
        let resp = req.send().await.map_err(wrap_transport_error)?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.bytes().await.map_err(wrap_transport_error)?.to_vec();
        if status.as_u16() >= 400 {
            return Err(wrap_http_error(status.as_u16(), status.canonical_reason(), &body));
        }
        Ok(Response {
            status: status.as_u16(),
            content_type,
            body,
        })
    }
}

fn wrap_transport_error(err: reqwest::Error) -> ClientError {
    if err.is_timeout() {
        ClientError::Timeout {
            message: format!("TimeoutException: {err}"),
        }
    } else {
        ClientError::Connection {
            message: format!("ConnectError: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{ResultExt, TestResult};

    #[test]
    fn url_join_strips_leading_slash() -> TestResult {
        let t = Transport::new("http://h/mount", Duration::from_secs(1), None)?;
        assert_eq!(t.base_url(), "http://h/mount/");
        assert_eq!(t.url_for("/api/auth/whoami"), "http://h/mount/api/auth/whoami");
        assert_eq!(t.url_for("api/datasets"), "http://h/mount/api/datasets");
        Ok(())
    }

    #[tokio::test]
    async fn unreachable_host_is_connection_error() -> TestResult {
        let t = Transport::new("http://127.0.0.1:1", Duration::from_secs(2), None)?;
        let err = t.request(HttpMethod::Get, "x", None, None).await.err_or_fail()?;
        assert!(err.is_connection(), "{err:?}");
        Ok(())
    }
}
