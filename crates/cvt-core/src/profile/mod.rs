//! The subscription store: index, documents, and remote updates.

pub mod item;
pub mod overrides;
pub mod source;
pub mod store;

pub use item::{PrfItem, PrfOption, ProfileType, SeqPatch, UserInfo};
pub use source::{
    Attempt, DEFAULT_TIMEOUT, FetchSource, Fetched, MAX_BODY_BYTES, MIHOMO_COMPAT_VERSION,
    SubscriptionFetcher, UpdateOutcome, decode_body, is_due, looks_like_config, parse_user_info,
};
pub use store::{ImportReport, Index, ProfileStore, document_path};
