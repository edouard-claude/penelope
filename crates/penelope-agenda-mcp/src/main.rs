//! Binaire `penelope-agenda-mcp` : le serveur seul, pour qui ne passe pas par
//! `penelope agenda-mcp`.

#![forbid(unsafe_code)]

#[tokio::main]
async fn main() {
    if let Err(e) = penelope_agenda_mcp::run_stdio().await {
        eprintln!("penelope-agenda-mcp : {e}");
        std::process::exit(2);
    }
}
