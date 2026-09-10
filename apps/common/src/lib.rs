//! Wiring shared by `monke-api` and `monke-worker`.
//!
//! Nothing here is an abstraction for its own sake — each piece is something both binaries
//! need to do identically, where two copies could drift apart in a way nothing would catch:
//! which database they connect to, which bucket they sign against, and which three transcode
//! targets exist.

pub mod cache;
pub mod pg;
pub mod s3;
pub mod targets;

pub use cache::redis_client;
pub use pg::pg_pool;
pub use s3::S3Store;
pub use targets::TARGETS;
