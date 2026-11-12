// The lib's rules, for the same reasons (see its crate root).
#![cfg_attr(not(test), warn(clippy::print_stdout, clippy::print_stderr))]
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

/// `ExitCode`, not `Result`: std would print the error chain raw, and pack and
/// config text reach it; [`pixtuoid::report_fatal`] strips it.
fn main() -> std::process::ExitCode {
    match pixtuoid::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            pixtuoid::report_fatal(&e);
            std::process::ExitCode::FAILURE
        }
    }
}
