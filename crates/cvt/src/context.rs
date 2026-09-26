//! The opened application, shared by every command.
//!
//! [`Ctx`] exists to make one property checkable: **read commands do not
//! mutate**. The only method here that can write the profile index is
//! [`Ctx::edit_store`], so `grep -rn edit_store src/commands/` lists exactly
//! the commands that change anything on disk.
//!
//! Opening the home creates the directory layout, because that is what
//! [`cvt_core::Service::open`] does. `doctor` is the one command that has to
//! see the home as it found it, so it never goes through this type until it
//! has recorded what it saw.

use anyhow::{Context as _, Result};
use cvt_core::mihomo::client::Client;
use cvt_core::profile::source::SubscriptionFetcher;
use cvt_core::profile::store::ProfileStore;
use cvt_core::{AppPaths, Error, Service, Settings};

use crate::output::Output;

/// The application and the channel a command renders into.
#[derive(Debug, Clone)]
pub struct Ctx {
    service: Service,
    out: Output,
}

impl Ctx {
    /// Open the home, creating the directory layout the application expects.
    ///
    /// # Errors
    /// Propagates a home that cannot be created or a settings file that
    /// cannot be parsed.
    pub fn open(paths: AppPaths, out: Output) -> Result<Self> {
        let service = Service::open(paths).context("could not open the application home")?;
        Ok(Self { service, out })
    }

    /// Wrap a service that was already opened.
    ///
    /// Used by `doctor`, which must be able to look at the home before
    /// anything creates it.
    #[must_use]
    pub fn from_service(service: Service, out: Output) -> Self {
        Self { service, out }
    }

    /// The application home.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        self.service.paths()
    }

    /// Current settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        self.service.settings()
    }

    /// The underlying service, for the operations that are not worth wrapping.
    #[must_use]
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// Where results go.
    #[must_use]
    pub fn out(&self) -> &Output {
        &self.out
    }

    /// Load the profile index for reading.
    ///
    /// The returned store is owned, so a command that keeps hold of it writes
    /// nothing unless it also calls [`ProfileStore::save`] — which only
    /// [`Ctx::edit_store`] does.
    ///
    /// # Errors
    /// [`Error::Parse`] when the index is malformed.
    pub fn store(&self) -> Result<ProfileStore> {
        self.service
            .store()
            .context("could not read the profile index")
    }

    /// Load, change and persist the profile index.
    ///
    /// The one write path for `profiles.yaml`: the closure gets a store, and
    /// the save happens only if it returns `Ok`.
    ///
    /// # Errors
    /// Whatever the edit returned, or a failure to write the index.
    pub fn edit_store<T>(
        &self,
        edit: impl FnOnce(&mut ProfileStore) -> cvt_core::Result<T>,
    ) -> Result<T> {
        let mut store = self.store()?;
        let value = edit(&mut store)?;
        store.save().context("could not write the profile index")?;
        Ok(value)
    }

    /// A client for the controller.
    ///
    /// "Nothing declares a controller" is turned into the documented
    /// unreachable-controller error here, so a script sees exit code 4 for
    /// every shape of "there is no core to talk to" rather than having to
    /// distinguish a configuration gap from a refused connection.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when no endpoint is known.
    pub fn client(&self) -> Result<Client> {
        match self.service.client() {
            Ok(client) => Ok(client),
            Err(Error::MissingField {
                field: "external-controller",
                ..
            }) => Err(Error::ControllerUnreachable {
                endpoint: "unknown".to_owned(),
                source: "no profile declares an `external-controller`".into(),
            }
            .into()),
            Err(other) => Err(other.into()),
        }
    }

    /// A fetcher for subscription URLs, with the core's mixed port as its
    /// second-tier route when one is configured.
    ///
    /// # Errors
    /// [`Error::Http`] when the HTTP backend cannot be built.
    pub fn fetcher(&self) -> Result<SubscriptionFetcher> {
        SubscriptionFetcher::new(self.mixed_proxy_addr())
            .context("could not build the subscription client")
    }

    /// The core's mixed port, as the deployed configuration declares it.
    ///
    /// The fetcher uses this for its second tier: a provider that is only
    /// reachable through the tunnel needs the port the core is listening on,
    /// and the deployed configuration is what the running core was launched
    /// with.
    #[must_use]
    pub fn mixed_proxy_addr(&self) -> Option<String> {
        self.service.proxy_addr()
    }
}
