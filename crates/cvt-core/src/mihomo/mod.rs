//! Talking to the mihomo core: transports, wire types and the API client.

pub mod client;
pub mod endpoint;
pub mod types;

pub use client::{Capabilities, Client, DEFAULT_TIMEOUT, encode_query, encode_segment};
pub use endpoint::{Endpoint, Transport};
