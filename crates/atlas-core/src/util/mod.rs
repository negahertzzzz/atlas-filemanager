//! Core-side utilities: config/session persistence, display formatting, and the
//! extension registry helpers. Platform/UI utils (native drag, window blur) stay in
//! the binary.

pub mod config;
pub mod display;
pub mod guard;
#[cfg(windows)]
pub mod winfsp;
