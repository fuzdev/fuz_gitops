//! `repos` — git state over the repos a `repos.toml` registry declares.

use std::process::ExitCode;

fn main() -> ExitCode {
    println!("repos {}", fuz_repos::STATUS_FORMAT_VERSION);
    ExitCode::SUCCESS
}
