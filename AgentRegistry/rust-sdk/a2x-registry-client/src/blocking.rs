// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Synchronous wrapper around [`crate::A2xRegistryClient`].
//!
//! Mirrors the Python `A2XRegistryClient` (the sync entry point). Each method
//! runs the async counterpart on a private tokio runtime with one worker
//! thread, so heartbeat renewers keep running between calls.
//!
//! Do not use this type from inside an async runtime; call the async client
//! there instead.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use serde_json::Value;

use crate::client::{
    ClientConfig, CreateDatasetOptions, ListOptions, RegisterOptions, ReserveOptions, ShutdownOptions,
};
use crate::errors::ClientError;
use crate::models::{
    AgentDetail, DatasetCreateResponse, DatasetDeleteResponse, DeregisterResponse, JsonObject, PatchResponse,
    PrincipalCreateResponse, RegisterResponse, Reservation, ShutdownReport,
};
use crate::transport::HttpMethod;

/// Blocking client. Owns a private tokio runtime.
pub struct A2xRegistryClient {
    inner: crate::A2xRegistryClient,
    rt: tokio::runtime::Runtime,
}

impl std::fmt::Debug for A2xRegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blocking::A2xRegistryClient")
            .field("inner", &self.inner)
            .finish()
    }
}

macro_rules! block {
    ($self:ident, $fut:expr_2021) => {
        $self.rt.block_on($fut)
    };
}

impl A2xRegistryClient {
    /// Build a blocking client and its runtime. No HTTP is sent.
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let inner = rt.block_on(async { crate::A2xRegistryClient::new(config) })?;
        Ok(A2xRegistryClient { inner, rt })
    }

    /// Convenience: explicit URL, memory-only ownership, no API key lookup.
    pub fn connect(base_url: &str) -> Result<Self, ClientError> {
        Self::new(
            ClientConfig::new()
                .base_url(base_url)
                .ownership_file(crate::OwnershipFile::Disabled),
        )
    }

    /// The wrapped async client.
    pub fn async_client(&self) -> &crate::A2xRegistryClient {
        &self.inner
    }

    /// Normalised base URL.
    pub fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    /// Configured HTTP timeout.
    pub fn timeout(&self) -> Duration {
        self.inner.timeout()
    }

    /// Resolved API key, if any.
    pub fn api_key(&self) -> Option<&str> {
        self.inner.api_key()
    }

    /// Stop background renewers (best-effort). Not a deregister.
    pub fn close(&self) {
        block!(self, self.inner.close())
    }

    /// See [`crate::A2xRegistryClient::request_json`].
    pub fn request_json(
        &self,
        method: HttpMethod,
        path: &str,
        json: Option<&Value>,
    ) -> Result<Value, ClientError> {
        block!(self, self.inner.request_json(method, path, json))
    }

    /// See [`crate::A2xRegistryClient::create_dataset`].
    pub fn create_dataset(
        &self,
        name: &str,
        opts: &CreateDatasetOptions,
    ) -> Result<DatasetCreateResponse, ClientError> {
        block!(self, self.inner.create_dataset(name, opts))
    }

    /// See [`crate::A2xRegistryClient::create_principal`].
    pub fn create_principal(
        &self,
        handle: &str,
        role: &str,
        namespaces: Option<&[String]>,
        note: Option<&str>,
    ) -> Result<PrincipalCreateResponse, ClientError> {
        block!(self, self.inner.create_principal(handle, role, namespaces, note))
    }

    /// See [`crate::A2xRegistryClient::delete_dataset`].
    pub fn delete_dataset(&self, name: &str) -> Result<DatasetDeleteResponse, ClientError> {
        block!(self, self.inner.delete_dataset(name))
    }

    /// See [`crate::A2xRegistryClient::register_agent`].
    pub fn register_agent(
        &self,
        dataset: &str,
        agent_card: &JsonObject,
        opts: &RegisterOptions,
    ) -> Result<RegisterResponse, ClientError> {
        block!(self, self.inner.register_agent(dataset, agent_card, opts))
    }

    /// See [`crate::A2xRegistryClient::heartbeat`].
    pub fn heartbeat(
        &self,
        dataset: &str,
        service_id: &str,
        status: Option<&str>,
    ) -> Result<Value, ClientError> {
        block!(self, self.inner.heartbeat(dataset, service_id, status))
    }

    /// See [`crate::A2xRegistryClient::drain`].
    pub fn drain(&self, dataset: &str, service_id: &str) -> Result<PatchResponse, ClientError> {
        block!(self, self.inner.drain(dataset, service_id))
    }

    /// See [`crate::A2xRegistryClient::shutdown`].
    pub fn shutdown(&self, opts: &ShutdownOptions) -> Result<ShutdownReport, ClientError> {
        block!(self, self.inner.shutdown(opts))
    }

    /// See [`crate::A2xRegistryClient::update_agent`].
    pub fn update_agent(
        &self,
        dataset: &str,
        service_id: &str,
        fields: &JsonObject,
    ) -> Result<PatchResponse, ClientError> {
        block!(self, self.inner.update_agent(dataset, service_id, fields))
    }

    /// See [`crate::A2xRegistryClient::set_status`].
    pub fn set_status(
        &self,
        dataset: &str,
        service_id: &str,
        status: &str,
    ) -> Result<PatchResponse, ClientError> {
        block!(self, self.inner.set_status(dataset, service_id, status))
    }

    /// See [`crate::A2xRegistryClient::list_agents`].
    pub fn list_agents(&self, dataset: &str, opts: &ListOptions) -> Result<Vec<JsonObject>, ClientError> {
        block!(self, self.inner.list_agents(dataset, opts))
    }

    /// See [`crate::A2xRegistryClient::get_agent`].
    pub fn get_agent(&self, dataset: &str, service_id: &str) -> Result<AgentDetail, ClientError> {
        block!(self, self.inner.get_agent(dataset, service_id))
    }

    /// See [`crate::A2xRegistryClient::deregister_agent`].
    pub fn deregister_agent(
        &self,
        dataset: &str,
        service_id: &str,
    ) -> Result<DeregisterResponse, ClientError> {
        block!(self, self.inner.deregister_agent(dataset, service_id))
    }

    /// See [`crate::A2xRegistryClient::register_blank_agent`].
    pub fn register_blank_agent(
        &self,
        dataset: &str,
        endpoint: &str,
        service_id: Option<&str>,
        persistent: bool,
    ) -> Result<RegisterResponse, ClientError> {
        block!(
            self,
            self.inner
                .register_blank_agent(dataset, endpoint, service_id, persistent)
        )
    }

    /// See [`crate::A2xRegistryClient::list_idle_blank_agents`].
    pub fn list_idle_blank_agents(&self, dataset: &str, n: usize) -> Result<Vec<JsonObject>, ClientError> {
        block!(self, self.inner.list_idle_blank_agents(dataset, n))
    }

    /// See [`crate::A2xRegistryClient::replace_agent_card`].
    pub fn replace_agent_card(
        &self,
        dataset: &str,
        service_id: &str,
        agent_card: &JsonObject,
        release_lease: bool,
    ) -> Result<RegisterResponse, ClientError> {
        block!(
            self,
            self.inner
                .replace_agent_card(dataset, service_id, agent_card, release_lease)
        )
    }

    /// See [`crate::A2xRegistryClient::restore_to_blank`].
    pub fn restore_to_blank(&self, dataset: &str, service_id: &str) -> Result<RegisterResponse, ClientError> {
        block!(self, self.inner.restore_to_blank(dataset, service_id))
    }

    /// See [`crate::A2xRegistryClient::reserve_blank_agents`].
    pub fn reserve_blank_agents(
        &self,
        dataset: &str,
        opts: &ReserveOptions,
    ) -> Result<Reservation, ClientError> {
        block!(self, self.inner.reserve_blank_agents(dataset, opts))
    }

    /// Reserve and wrap the result in a [`ReservationGuard`] that releases on drop.
    ///
    /// This is the Rust form of Python's `with client.reserve_blank_agents(...) as r:`.
    pub fn reserve_blank_agents_guarded(
        &self,
        dataset: &str,
        opts: &ReserveOptions,
    ) -> Result<ReservationGuard<'_>, ClientError> {
        let reservation = self.reserve_blank_agents(dataset, opts)?;
        Ok(ReservationGuard {
            client: self,
            reservation,
        })
    }

    /// See [`crate::A2xRegistryClient::release_reservation`].
    pub fn release_reservation(
        &self,
        reservation: &mut Reservation,
        service_ids: Option<&[String]>,
    ) -> Result<Vec<String>, ClientError> {
        block!(self, self.inner.release_reservation(reservation, service_ids))
    }

    /// See [`crate::A2xRegistryClient::extend_reservation`].
    pub fn extend_reservation(
        &self,
        reservation: &mut Reservation,
        ttl_seconds: i64,
    ) -> Result<f64, ClientError> {
        block!(self, self.inner.extend_reservation(reservation, ttl_seconds))
    }

    /// See [`crate::A2xRegistryClient::release_my_lease`].
    pub fn release_my_lease(&self, dataset: &str, service_id: &str) -> Result<bool, ClientError> {
        block!(self, self.inner.release_my_lease(dataset, service_id))
    }

    /// See [`crate::A2xRegistryClient::whoami`].
    pub fn whoami(&self) -> Result<Value, ClientError> {
        block!(self, self.inner.whoami())
    }

    /// See [`crate::A2xRegistryClient::list_keys`].
    pub fn list_keys(&self) -> Result<Value, ClientError> {
        block!(self, self.inner.list_keys())
    }

    /// See [`crate::A2xRegistryClient::create_key`].
    pub fn create_key(&self, name: &str) -> Result<Value, ClientError> {
        block!(self, self.inner.create_key(name))
    }

    /// See [`crate::A2xRegistryClient::revoke_key`].
    pub fn revoke_key(&self, key_id: &str) -> Result<Value, ClientError> {
        block!(self, self.inner.revoke_key(key_id))
    }
}

impl Drop for A2xRegistryClient {
    fn drop(&mut self) {
        self.close();
    }
}

/// A [`Reservation`] that releases its leases when dropped (best-effort).
///
/// Dereferences to the inner [`Reservation`]. Use [`ReservationGuard::into_inner`]
/// to keep the leases alive past the scope.
pub struct ReservationGuard<'a> {
    client: &'a A2xRegistryClient,
    reservation: Reservation,
}

impl std::fmt::Debug for ReservationGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReservationGuard")
            .field("reservation", &self.reservation)
            .finish()
    }
}

impl ReservationGuard<'_> {
    /// Release explicitly (idempotent) and return the released sids.
    pub fn release(&mut self) -> Result<Vec<String>, ClientError> {
        if self.reservation.released {
            return Ok(Vec::new());
        }
        self.client.release_reservation(&mut self.reservation, None)
    }

    /// Take the reservation out without releasing it.
    pub fn into_inner(mut self) -> Reservation {
        self.reservation.released = true;
        std::mem::replace(
            &mut self.reservation,
            Reservation {
                holder_id: String::new(),
                dataset: String::new(),
                ttl_seconds: 0,
                expires_at_unix: 0.0,
                agents: Vec::new(),
                released: true,
            },
        )
    }
}

impl Deref for ReservationGuard<'_> {
    type Target = Reservation;

    fn deref(&self) -> &Reservation {
        &self.reservation
    }
}

impl DerefMut for ReservationGuard<'_> {
    fn deref_mut(&mut self) -> &mut Reservation {
        &mut self.reservation
    }
}

impl Drop for ReservationGuard<'_> {
    fn drop(&mut self) {
        if !self.reservation.released {
            // Best-effort; the lease expires by TTL anyway.
            let _ = self.client.release_reservation(&mut self.reservation, None);
        }
    }
}
