//! Dorean — a pi/opencode-style coding agent for the terminal.
//!
//! Library facade exposing the modules used by both the `dorean` binary and
//! the integration tests.

pub mod agent;
pub mod cli;
pub mod config;
pub mod error;
pub mod history;
pub mod permissions;
pub mod providers;
pub mod tui;
