//! `atelier` : la ligne de commande de l'inventaire.
#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("version") => println!("atelier {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("usage : atelier version");
            std::process::exit(2);
        }
    }
}
