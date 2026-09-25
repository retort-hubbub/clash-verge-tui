//! Domain models: the configuration document, proxies, groups and rules.

pub mod config;
pub mod proxy;
pub mod rule;

pub use config::{Config, ConfigStats, LOG_LEVELS, MODES};
pub use proxy::{GroupKind, HealthCheck, Proxy, ProxyGroup, ProxyInventory, ProxyProvider};
pub use rule::Rule;
