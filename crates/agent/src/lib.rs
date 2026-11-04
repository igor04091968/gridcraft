//! Server/native agent integration. Browser builds use the authenticated control channel.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod remote;
#[cfg(not(target_arch = "wasm32"))]
pub mod web_agent;
#[cfg(not(target_arch = "wasm32"))]
pub mod web_control;
