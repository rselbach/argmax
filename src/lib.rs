//! Core library for argmax, a local terminal-native command assistant.
//!
//! Providers return inert data. They never receive terminal-writing capability;
//! the session layer owns insertion and rendering.

pub mod ai;
pub mod ai_lifecycle;
pub mod ai_prompt;
pub mod ai_provider;
pub mod ai_transport;
pub mod catalog;
pub mod cli;
pub mod completion;
pub mod config;
pub mod context;
pub mod coordinator;
pub mod crash_boundary;
pub mod diagnostics;
pub mod history;
pub mod input;
pub mod integration;
pub mod keybindings;
pub mod learning;
pub mod learning_store;
pub mod overlay;
pub mod process_runner;
pub mod providers;
pub mod pty;
pub mod ranking;
pub mod reload;
pub mod screen;
pub mod selection;
pub mod session;
pub mod setup;
pub mod shell_events;
pub mod state;
pub mod terminal;
pub mod update_apply;
pub mod updater;
pub mod version;
