mod application;

use std::process::ExitCode;

use argmax::process_runner::{HiddenLauncherDispatch, dispatch_hidden_launcher};

fn main() -> ExitCode {
    match dispatch_hidden_launcher(std::env::args_os()) {
        HiddenLauncherDispatch::Exit(status) => return status,
        HiddenLauncherDispatch::NotHidden => {}
    }

    application::run(std::env::args_os())
}
