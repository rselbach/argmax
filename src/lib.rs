//! Core library for argmax, a local terminal-native command assistant.
//!
//! Providers return inert data. They never receive terminal-writing capability;
//! the session layer owns insertion and rendering.

pub mod ai;
pub mod ai_lifecycle;
pub mod ai_prompt;
pub mod ai_provider;
pub mod catalog;
pub mod cli;
pub mod completion;
pub mod config;
pub mod context;
pub mod coordinator;
pub mod diagnostics;
pub mod history;
pub mod input;
pub mod integration;
pub mod keybindings;
pub mod learning;
pub mod providers;
pub mod ranking;
pub mod reload;
pub mod selection;
pub mod session;
pub mod shell_events;
pub mod updater;
pub mod version;
