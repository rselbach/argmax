//! Core library for argmax, a local terminal-native command assistant.
//!
//! Providers return inert data. They never receive terminal-writing capability;
//! the session layer owns insertion and rendering.

pub mod ai;
pub mod catalog;
pub mod completion;
pub mod history;
pub mod learning;
pub mod providers;
pub mod ranking;
pub mod selection;
