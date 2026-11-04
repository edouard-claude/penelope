//! `penelope vault atlas` (#428) : l'atlas du vault servi en local, recalculé quand le
//! vault change, sans daemon. Chaque GET de la page relit l'empreinte du vault
//! ([`penelope_memory::atlas::fingerprint`], métadonnées seules) ; si elle a bougé,
//! l'atlas est recalculé ([`penelope_memory::atlas::build`], déterministe, sans modèle),
//! sinon le dernier calcul sert. Le calcul se fait sous un verrou : deux GET concurrents
//! voient le même atlas, jamais un atlas à moitié écrit.
//!
//! Une lecture qui échoue garde le dernier atlas réussi, marqué périmé avec sa date, et ne
//! dit pas plus que « lecture du vault impossible ». Deux routes seulement (`/` et
//! `/atlas.json`), en GET ou HEAD ; aucun fichier du vault n'est servi. Lecture seule : ni
//! écriture, ni réindexation. L'écoute par défaut est `127.0.0.1:8082` ; une autre adresse
//! (le réseau local) se demande par `--listen`.

use super::*;
use penelope_memory::atlas;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PAGE: &str = include_str!("atlas.html");

#[derive(Default)]
struct Cache {
    fingerprint: String,
    /// L'atlas du dernier calcul réussi, en JSON, et sa date.
    json: Option<serde_json::Value>,
    computed_at: String,
    error: Option<String>,
}

/// L'atlas à servir : recalculé si le vault a changé, sinon le dernier.
fn current(vault: &Path, cache: &Mutex<Cache>) -> serde_json::Value {
    let mut c = cache.lock().unwrap_or_else(|p| p.into_inner());
    let fp = atlas::fingerprint(vault);
    if c.json.is_none() || c.fingerprint != fp || c.error.is_some() {
        match atlas::build(vault)
            .and_then(|a| serde_json::to_value(a).map_err(|e| std::io::Error::other(e.to_string())))
        {
            Ok(v) => {
                c.json = Some(v);
                c.fingerprint = fp;
                c.computed_at = chrono::Local::now().format("%d/%m/%Y %H:%M:%S").to_string();
                c.error = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "atlas du vault");
                c.error = Some("lecture du vault impossible".into());
            }
        }
    }
    let mut v = c.json.clone().unwrap_or_else(|| {
        serde_json::json!({"nodes": [], "links": [], "cats": [],
            "stats": {"notes": 0, "liens": 0, "references": 0}})
    });
    v["meta"] = serde_json::json!({
        "computed_at": c.computed_at,
        "stale": c.error.is_some(),
        "error": c.error,
    });
    v
}

/// La page, données incluses : `</` neutralisé pour ne jamais fermer le `<script>`.
fn page(data: &serde_json::Value) -> String {
    PAGE.replace("__ATLAS_DATA__", &data.to_string().replace("</", "<\\/"))
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8], head_only: bool) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).is_ok() && !head_only {
        let _ = stream.write_all(body);
    }
}

fn handle(mut stream: TcpStream, vault: &Path, cache: &Mutex<Cache>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut first = req.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let path = first
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"GET seulement",
            false,
        );
    }
    match path {
        "/" | "/index.html" => {
            let body = page(&current(vault, cache));
            respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                body.as_bytes(),
                head_only,
            )
        }
        "/atlas.json" => {
            let body = current(vault, cache).to_string();
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                body.as_bytes(),
                head_only,
            )
        }
        _ => respond(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"introuvable",
            head_only,
        ),
    }
}

/// Le vault de la configuration, sans daemon.
fn vault_of(cli: &Cli) -> CliResult<PathBuf> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let raw = std::fs::read_to_string(dirs.config_file()).unwrap_or_default();
    let (cfg, _) =
        penelope_kernel::Config::parse(&raw).map_err(|e| CliError::Validation(e.to_string()))?;
    Ok(dirs.expand(&cfg.memory.vault_path))
}

pub(super) fn run(cli: &Cli, listen: &str) -> CliResult<()> {
    let vault = vault_of(cli)?;
    let listener =
        TcpListener::bind(listen).map_err(|e| CliError::Io(format!("{listen} : {e}")))?;
    println!(
        "Atlas du vault `{}` : http://{listen} (Ctrl-C pour arrêter)",
        vault.display()
    );
    let cache = Arc::new(Mutex::new(Cache::default()));
    let vault = Arc::new(vault);
    for stream in listener.incoming().flatten() {
        let (cache, vault) = (cache.clone(), vault.clone());
        std::thread::spawn(move || handle(stream, &vault, &cache));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
