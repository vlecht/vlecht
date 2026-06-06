// vlecht-atp: ATproto identity, service auth, and sh.tangled.* XRPC endpoints.
//
// The lex module hand-writes the JSON shapes for the query endpoints the Go
// knotserver exposes, in terms of axum handlers. They use plain `serde_json`
// so we don't have to depend on jacquard-lexgen codegen for a tight build.

pub mod config;
pub mod error;
pub mod identity;
pub mod lex;
pub mod service_auth;
