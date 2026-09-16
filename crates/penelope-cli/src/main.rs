//! `penelope` : CLI et client RPC du daemon (§15).
//!
//! Sortie lisible par défaut, `--json` partout, codes de sortie documentés.

#![forbid(unsafe_code)]

mod client;
mod commands;
mod output;

use clap::Parser;
use penelope_kernel::api::exit_code;

#[tokio::main]
async fn main() {
    let cli = commands::Cli::parse();
    penelope_observe::init_test();

    let code = match commands::run(cli).await {
        Ok(()) => exit_code::OK,
        Err(e) => {
            let code = e.exit_code();
            eprintln!("erreur : {e}");
            if let Some(hint) = e.hint() {
                eprintln!("→ {hint}");
            }
            code
        }
    };
    std::process::exit(code);
}
