//! Talking to the mihomo core: transports, wire types, streams, the client and
//! the process supervisor.

pub mod client;
pub mod endpoint;
pub mod stream;
pub mod supervisor;
pub mod types;

pub use client::{Capabilities, Client, DEFAULT_TIMEOUT, encode_query, encode_segment};
pub use endpoint::{Endpoint, Transport};
pub use stream::{
    Event as StreamEvent, Options as StreamOptions, Selection, Stream, TransportKind,
};
pub use supervisor::{CoreStatus, Supervisor};
