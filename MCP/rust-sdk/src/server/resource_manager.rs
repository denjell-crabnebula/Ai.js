// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Resource registry, port of `src/server/resource_manager.*`.

use std::collections::BTreeMap;

use parking_lot::Mutex;

use super::tool_manager::cursor_start;
use super::{ResourceHandler, ServerContext};
use crate::error::McpError;
use crate::types::{
    DEFAULT_RESOURCES_PAGE_SIZE, ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult,
    ResourceInfo, ResourceTemplate,
};

#[derive(Clone)]
struct ResourceEntry {
    info: ResourceInfo,
    handler: ResourceHandler,
}

/// Thread safe resource and resource template registry.
pub struct ResourceManager {
    overwrite: bool,
    resources: Mutex<BTreeMap<String, ResourceEntry>>,
    templates: Mutex<BTreeMap<String, ResourceTemplate>>,
    page_size: Mutex<usize>,
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new(true, DEFAULT_RESOURCES_PAGE_SIZE)
    }
}

impl ResourceManager {
    /// Create a registry. `overwrite` allows re-adding existing entries.
    pub fn new(overwrite: bool, page_size: usize) -> Self {
        Self {
            overwrite,
            resources: Mutex::new(BTreeMap::new()),
            templates: Mutex::new(BTreeMap::new()),
            page_size: Mutex::new(page_size),
        }
    }

    /// Set the page size of `list_resources`.
    pub fn set_page_size(&self, page_size: usize) {
        *self.page_size.lock() = page_size;
    }

    /// Number of registered resources.
    pub fn len(&self) -> usize {
        self.resources.lock().len()
    }

    /// True when no resource is registered.
    pub fn is_empty(&self) -> bool {
        self.resources.lock().is_empty()
    }

    /// Register a resource. URI and name must not be empty.
    pub fn add_resource(&self, resource: ResourceInfo, handler: ResourceHandler) -> Result<(), McpError> {
        if resource.uri.is_empty() {
            return Err(McpError::argument("Resource URI cannot be empty"));
        }
        if resource.name.is_empty() {
            return Err(McpError::argument("Resource name cannot be empty"));
        }
        let mut resources = self.resources.lock();
        if resources.contains_key(&resource.uri) {
            if !self.overwrite {
                return Err(McpError::state(format!(
                    "Resource '{}' already exists",
                    resource.uri
                )));
            }
            tracing::warn!("Resource '{}' already exists, overwriting", resource.uri);
        }
        resources.insert(
            resource.uri.clone(),
            ResourceEntry {
                info: resource,
                handler,
            },
        );
        Ok(())
    }

    /// Remove a resource.
    pub fn remove_resource(&self, uri: &str) -> Result<(), McpError> {
        if uri.is_empty() {
            return Err(McpError::argument("Resource URI cannot be empty"));
        }
        if self.resources.lock().remove(uri).is_none() {
            return Err(McpError::state(format!("Resource '{uri}' not found")));
        }
        Ok(())
    }

    /// Register a resource template. URI template and name must not be empty.
    pub fn add_resource_template(&self, template: ResourceTemplate) -> Result<(), McpError> {
        if template.uri_template.is_empty() {
            return Err(McpError::argument("Resource template URI cannot be empty"));
        }
        if template.name.is_empty() {
            return Err(McpError::argument("Resource template name cannot be empty"));
        }
        let mut templates = self.templates.lock();
        if templates.contains_key(&template.uri_template) {
            if !self.overwrite {
                return Err(McpError::state(format!(
                    "Resource template '{}' already exists",
                    template.uri_template
                )));
            }
            tracing::warn!(
                "Resource template '{}' already exists, overwriting",
                template.uri_template
            );
        }
        templates.insert(template.uri_template.clone(), template);
        Ok(())
    }

    /// Remove a resource template.
    pub fn remove_resource_template(&self, uri_template: &str) -> Result<(), McpError> {
        if uri_template.is_empty() {
            return Err(McpError::argument("Resource template URI cannot be empty"));
        }
        if self.templates.lock().remove(uri_template).is_none() {
            return Err(McpError::state(format!(
                "Resource template '{uri_template}' not found"
            )));
        }
        Ok(())
    }

    /// One page of resources. URIs are sorted; the cursor is a start index.
    pub fn list_resources(&self, cursor: Option<&str>) -> ListResourcesResult {
        let resources = self.resources.lock();
        let uris: Vec<&String> = resources.keys().collect();
        let start = cursor_start(cursor, uris.len());
        let end = (start + *self.page_size.lock()).min(uris.len());
        let page = uris[start..end]
            .iter()
            .filter_map(|u| resources.get(*u))
            .map(|e| e.info.clone())
            .collect();
        ListResourcesResult {
            resources: page,
            next_cursor: (end < uris.len()).then(|| end.to_string()),
            meta: None,
        }
    }

    /// Read a resource.
    pub async fn read_resource(&self, ctx: ServerContext, uri: &str) -> Result<ReadResourceResult, McpError> {
        let handler = self
            .resources
            .lock()
            .get(uri)
            .map(|e| e.handler.clone())
            .ok_or_else(|| McpError::state(format!("Resource not found:{uri}")))?;
        handler(ctx, uri.to_string()).await
    }

    /// All templates, sorted by URI template.
    pub fn list_resource_templates(&self) -> ListResourceTemplatesResult {
        ListResourceTemplatesResult {
            resource_templates: self.templates.lock().values().cloned().collect(),
            next_cursor: None,
            meta: None,
        }
    }

    /// Subscribe to a resource. Only checks that the resource exists.
    pub fn subscribe_resource(&self, uri: &str) -> Result<(), McpError> {
        if !self.resources.lock().contains_key(uri) {
            return Err(McpError::state(format!("Resource not found:{uri}")));
        }
        Ok(())
    }

    /// Unsubscribe from a resource. Only checks that the resource exists.
    pub fn unsubscribe_resource(&self, uri: &str) -> Result<(), McpError> {
        if !self.resources.lock().contains_key(uri) {
            return Err(McpError::state(format!("Resource not found:{uri}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{resource_handler, session::ServerSession};
    use crate::types::{Annotations, BlobResourceContents, Icon, ResourceContents, TextResourceContents};
    use ap_support::testing::{ResultExt, TestResult};
    use std::sync::Arc;

    fn info(uri: &str) -> ResourceInfo {
        ResourceInfo {
            uri: uri.into(),
            name: "Test Resource".into(),
            ..Default::default()
        }
    }

    fn text_handler(content: &'static str) -> ResourceHandler {
        resource_handler(move |_ctx, uri| async move {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::Text(TextResourceContents {
                    uri,
                    text: content.to_string(),
                    mime_type: Some("text/plain".into()),
                })],
                meta: None,
            })
        })
    }

    fn ctx() -> ServerContext {
        ServerContext::new(ServerSession::detached(), None)
    }

    #[tokio::test]
    async fn add_read_remove_resources() -> TestResult {
        let m = ResourceManager::default();
        assert_eq!(
            m.add_resource(info(""), text_handler("x"))
                .err_or_fail()?
                .to_string(),
            "Resource URI cannot be empty"
        );
        let mut no_name = info("test://r");
        no_name.name.clear();
        assert_eq!(
            m.add_resource(no_name, text_handler("x"))
                .err_or_fail()?
                .to_string(),
            "Resource name cannot be empty"
        );
        m.add_resource(info("test://resource"), text_handler("First content"))?;
        let r = m.read_resource(ctx(), "test://resource").await?;
        assert!(
            matches!(&r.contents[0], ResourceContents::Text(t) if t.text == "First content" && t.uri == "test://resource")
        );
        m.add_resource(info("test://resource"), text_handler("Second content"))?;
        let r = m.read_resource(ctx(), "test://resource").await?;
        assert!(matches!(&r.contents[0], ResourceContents::Text(t) if t.text == "Second content"));
        assert_eq!(
            m.read_resource(ctx(), "nonexistent://resource")
                .await
                .err_or_fail()?
                .to_string(),
            "Resource not found:nonexistent://resource"
        );
        m.add_resource(info("test://resource2"), text_handler("x"))?;
        m.remove_resource("test://resource")?;
        assert_eq!(
            m.remove_resource("").err_or_fail()?.to_string(),
            "Resource URI cannot be empty"
        );
        assert_eq!(
            m.remove_resource("nonexistent://resource")
                .err_or_fail()?
                .to_string(),
            "Resource 'nonexistent://resource' not found"
        );
        let list = m.list_resources(None);
        assert_eq!(list.resources.len(), 1);
        assert_eq!(list.resources[0].uri, "test://resource2");

        let blob = resource_handler(|_c, uri| async move {
            Ok(ReadResourceResult {
                contents: vec![ResourceContents::Blob(BlobResourceContents {
                    uri,
                    blob: "SGVsbG8gV29ybGQh".into(),
                    mime_type: Some("image/png".into()),
                })],
                meta: None,
            })
        });
        m.add_resource(info("test://blob"), blob)?;
        let r = m.read_resource(ctx(), "test://blob").await?;
        assert!(matches!(&r.contents[0], ResourceContents::Blob(b) if b.blob == "SGVsbG8gV29ybGQh"));
        m.subscribe_resource("test://blob")?;
        m.unsubscribe_resource("test://blob")?;
        assert!(m.subscribe_resource("nonexistent://resource").is_err());
        assert!(m.unsubscribe_resource("nonexistent://resource").is_err());
        Ok(())
    }

    #[test]
    fn no_overwrite_mode() -> TestResult {
        let m = ResourceManager::new(false, 10);
        m.add_resource(info("r"), text_handler("a"))?;
        assert_eq!(
            m.add_resource(info("r"), text_handler("b"))
                .err_or_fail()?
                .to_string(),
            "Resource 'r' already exists"
        );
        let t = ResourceTemplate {
            uri_template: "t/{id}".into(),
            name: "T".into(),
            ..Default::default()
        };
        m.add_resource_template(t.clone())?;
        assert!(m.add_resource_template(t).is_err());
        Ok(())
    }

    #[test]
    fn resource_info_and_pagination() -> TestResult {
        let m = ResourceManager::new(true, 2);
        let detailed = ResourceInfo {
            uri: "test://detailed".into(),
            name: "Detailed".into(),
            title: Some("Title".into()),
            description: Some("Desc".into()),
            mime_type: Some("text/plain".into()),
            size: Some(1024),
            icons: Some(vec![Icon {
                src: "icon.png".into(),
                ..Default::default()
            }]),
            annotations: Some(Annotations {
                priority: Some(1.0),
                ..Default::default()
            }),
        };
        m.add_resource(detailed.clone(), text_handler("x"))?;
        assert_eq!(m.list_resources(None).resources[0], detailed);
        for i in 0..4 {
            m.add_resource(info(&format!("test://r{i}")), text_handler("x"))?;
        }
        assert_eq!(m.len(), 5);
        let mut total = 0;
        let mut cursor: Option<String> = None;
        loop {
            let page = m.list_resources(cursor.as_deref());
            total += page.resources.len();
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(total, 5);
        m.set_page_size(50);
        assert_eq!(m.list_resources(None).resources.len(), 5);
        assert!(ResourceManager::default().is_empty());
        Ok(())
    }

    #[test]
    fn templates() -> TestResult {
        let m = ResourceManager::default();
        let empty_uri = ResourceTemplate {
            name: "n".into(),
            ..Default::default()
        };
        assert_eq!(
            m.add_resource_template(empty_uri).err_or_fail()?.to_string(),
            "Resource template URI cannot be empty"
        );
        let empty_name = ResourceTemplate {
            uri_template: "t".into(),
            ..Default::default()
        };
        assert_eq!(
            m.add_resource_template(empty_name).err_or_fail()?.to_string(),
            "Resource template name cannot be empty"
        );
        let t1 = ResourceTemplate {
            uri_template: "test://t/{id}".into(),
            name: "First Template".into(),
            title: Some("First".into()),
            ..Default::default()
        };
        m.add_resource_template(t1.clone())?;
        let mut t2 = t1.clone();
        t2.name = "Second Template".into();
        t2.title = Some("Second".into());
        m.add_resource_template(t2)?;
        let list = m.list_resource_templates();
        assert_eq!(list.resource_templates.len(), 1);
        assert_eq!(list.resource_templates[0].name, "Second Template");
        assert_eq!(
            m.remove_resource_template("").err_or_fail()?.to_string(),
            "Resource template URI cannot be empty"
        );
        assert!(m.remove_resource_template("nonexistent://template").is_err());
        m.remove_resource_template("test://t/{id}")?;
        assert!(m.list_resource_templates().resource_templates.is_empty());
        // Resources and templates are independent.
        m.add_resource(info("r"), text_handler("x"))?;
        m.add_resource_template(t1)?;
        m.remove_resource("r")?;
        assert_eq!(m.list_resource_templates().resource_templates.len(), 1);
        let _ = Arc::new(());
        Ok(())
    }
}
