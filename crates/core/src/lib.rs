//! Remote Play protocol core: registration, session, Takion transport and controller state.
//!
//! Ported from chiaki-controller (AGPL-3.0-only) which is based on chiaki-ng, see NOTICE.

pub mod config;
pub mod controller;
pub mod gkcrypt;
pub mod http;
pub mod logbuf;
pub mod proto;
pub mod regist;
pub mod rpcrypt;
mod rpcrypt_tables;
pub mod session;
pub mod stream;
pub mod takion;
#[cfg(test)]
mod test_vectors;
