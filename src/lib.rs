//! Core library for CNB Bridge.
//!
//! The crate intentionally has no UI dependencies. The Tauri shell in
//! `src-tauri` drives it through explicit commands so the same logic can be
//! exercised by unit and integration tests.

pub mod account;
pub mod api;
pub mod bridge;
pub mod browser;
pub mod dpapi;
pub mod gateway;
pub mod http_trace;
pub mod lifecycle;
pub mod logging;
pub mod model_catalog;
pub mod net;
pub mod paths;
pub mod prompt_gate;
pub mod responses;
pub mod settings;
pub mod ssh_key;
pub mod tunnel;
pub mod web_auth;
pub mod workspace;

pub use account::{AccountManager, AccountStatus, AuthMethod, Credentials};
pub use bridge::{Bridge, BridgeSnapshot, BridgeState};
pub use model_catalog::ModelSummary;
pub use settings::{Settings, SettingsStore};
