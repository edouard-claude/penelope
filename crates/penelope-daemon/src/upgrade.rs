//! Mise à jour du binaire (§2.12) : release GitHub vérifiée, bascule par renommage,
//! retour arrière automatique.
//!
//! ```text
//!  upgrade ─► release ─► SHA256SUMS ─► archive (somme OK) ─► `penelope --version`
//!                                                                  │
//!     binaire courant ─► <binaire>.previous      nouveau ─► <binaire> (rename)
//!                                                                  │
//!  upgrade.json (en attente) ─► sortie du daemon ─► le service relance le nouveau binaire
//!                                                                  │
//!  démarrage : essai n ─► santé confirmée ─► upgrade.json supprimé, annonce au propriétaire
//!            └► pas de confirmation 60 s après le premier essai ─► .previous remis en place
//! ```
//!
//! Le processus en cours garde l'ancien inode : le remplacement est sûr pendant qu'il
//! tourne. Un chien de garde interne quitte le processus si la santé n'est pas confirmée
//! à temps, pour qu'un blocage finisse aussi en retour arrière. Une installation depuis
//! les sources se met à jour par `make deploy`.

use crate::runtime::Daemon;
use penelope_kernel::event::EventDraft;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// API des releases, surchargeable par `PENELOPE_RELEASES_URL`.
pub const RELEASES_URL: &str = "https://api.github.com/repos/edouard-claude/penelope/releases";
/// Fenêtre de santé (§2.12) : au-delà, un nouveau démarrage non confirmé annule.
pub const HEALTH_WINDOW_MS: i64 = 60_000;
/// Délai du chien de garde d'un démarrage à l'essai.
pub const WATCHDOG: Duration = Duration::from_secs(90);
/// Démarrages à l'essai tolérés quelle que soit l'horloge.
pub const MAX_BOOT_ATTEMPTS: u32 = 5;
/// Temps de fonctionnement avant de déclarer le nouveau binaire sain.
const SETTLE: Duration = Duration::from_secs(10);
const ARCHIVE_MAX_BYTES: usize = 200 * 1024 * 1024;
/// Première version qui compte ses démarrages : en dessous, pas de retour automatique.
const FIRST_SELF_UPGRADING: (u64, u64, u64) = (0, 3, 1);

static CONFIRMED: AtomicBool = AtomicBool::new(false);
static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Clé publique minisign intégrée au binaire de release (`PENELOPE_MINISIGN_PUBKEY` à la
/// compilation). Un binaire qui la porte exige une signature valide pour se mettre à jour.
pub const BUILT_IN_PUBKEY: Option<&str> = option_env!("PENELOPE_MINISIGN_PUBKEY");

/// D'où viennent les releases, et avec quelle clé elles se vérifient.
#[derive(Debug, Clone)]
pub struct Source {
    pub releases_url: String,
    pub os: String,
    /// Clé publique minisign ; `None` : somme SHA-256 seule.
    pub pubkey: Option<String>,
}

impl Source {
    /// `PENELOPE_RELEASES_URL`, sinon `upgrade.base_url`, sinon GitHub ; clé de
    /// `upgrade.minisign_pubkey`, sinon celle du binaire.
    pub fn from_config(cfg: &penelope_kernel::config::Config) -> Source {
        let releases_url = std::env::var("PENELOPE_RELEASES_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .or_else(|| Some(cfg.upgrade.base_url.trim().to_string()).filter(|u| !u.is_empty()))
            .unwrap_or_else(|| RELEASES_URL.to_string());
        Source {
            releases_url,
            os: std::env::consts::OS.to_string(),
            pubkey: release_pubkey(&cfg.upgrade.minisign_pubkey),
        }
    }
}

/// Clé à exiger : celle de la configuration, sinon celle du binaire.
pub fn release_pubkey(configured: &str) -> Option<String> {
    Some(configured.trim())
        .filter(|k| !k.is_empty())
        .or(BUILT_IN_PUBKEY.map(str::trim).filter(|k| !k.is_empty()))
        .map(String::from)
}

/// Vérifie la signature minisign de `SHA256SUMS`. La clé s'écrit en base64
/// (`RWQ…`) ou comme le contenu de `minisign.pub`.
pub fn verify_signature(data: &[u8], minisig: &str, pubkey: &str) -> Result<(), String> {
    let key = if pubkey.contains("untrusted comment") {
        minisign_verify::PublicKey::decode(pubkey)
    } else {
        minisign_verify::PublicKey::from_base64(pubkey.trim())
    }
    .map_err(|e| format!("clé publique minisign illisible : {e}"))?;
    let signature = minisign_verify::Signature::decode(minisig.trim())
        .map_err(|e| format!("signature illisible : {e}"))?;
    key.verify(data, &signature, false)
        .map_err(|e| format!("signature minisign invalide : {e}"))
}

/// Release résolue.
#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub archive_name: String,
    pub archive_url: String,
    pub sums_url: String,
    /// `SHA256SUMS.minisig`, si la release est signée.
    pub signature_url: Option<String>,
}

/// Nom de l'archive publiée pour un OS.
pub fn asset_name(tag: &str, os: &str) -> Result<String, String> {
    match os {
        "macos" => Ok(format!("penelope-{tag}-macos-universal.tar.gz")),
        other => Err(format!(
            "pas d'artefact publié pour {other} : mettre à jour depuis les sources (`make deploy`)"
        )),
    }
}

/// Somme attendue d'un fichier dans `SHA256SUMS` (`<hex>  ./<nom>` ou `<hex>  <nom>`).
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let file = parts
            .next()?
            .trim_start_matches('*')
            .trim_start_matches("./");
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_lowercase())
    })
}

/// `0.3.1`, `v0.3.1`, `0.3.1-rc1` : les trois premiers nombres.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((n.next()??, n.next()??, n.next().flatten().unwrap_or(0)))
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => candidate != current,
    }
}

/// La sortie de `--version` annonce-t-elle exactement cette version ?
fn announces(said: &str, version: &str) -> bool {
    said.split_whitespace()
        .any(|t| t.trim_start_matches('v') == version)
}

async fn fetch(client: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, String> {
    crate::mcp_auth::check_endpoint(url)?;
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            concat!("penelope/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| format!("GET {url} : {e}"))?;
    let status = resp.status().as_u16();
    if status >= 400 {
        return Err(format!("GET {url} : HTTP {status}"));
    }
    if resp.content_length().is_some_and(|n| n as usize > max) {
        return Err(format!("{url} : réponse trop volumineuse"));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("GET {url} : {e}"))?;
    if bytes.len() > max {
        return Err(format!("{url} : réponse trop volumineuse"));
    }
    Ok(bytes.to_vec())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

/// Résout une release (`tag` absent : la plus haute version publiée). Les versions 0.x
/// sont publiées en pre-release, que `releases/latest` ignore : on lit la liste.
pub async fn release(
    client: &reqwest::Client,
    source: &Source,
    tag: Option<&str>,
) -> Result<Release, String> {
    let base = source.releases_url.trim_end_matches('/');
    let meta: Value = match tag {
        Some(t) => {
            serde_json::from_slice(&fetch(client, &format!("{base}/tags/{t}"), 1024 * 1024).await?)
                .map_err(|e| format!("release illisible : {e}"))?
        }
        None => {
            let list: Vec<Value> = serde_json::from_slice(
                &fetch(client, &format!("{base}?per_page=30"), 4 * 1024 * 1024).await?,
            )
            .map_err(|e| format!("liste des releases illisible : {e}"))?;
            list.into_iter()
                .filter(|r| !r["draft"].as_bool().unwrap_or(false))
                .filter_map(|r| {
                    let v = parse_version(r["tag_name"].as_str()?)?;
                    Some((v, r))
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, r)| r)
                .ok_or("aucune release publiée")?
        }
    };
    let tag = meta["tag_name"]
        .as_str()
        .ok_or("release sans `tag_name`")?
        .to_string();
    let version = tag.trim_start_matches('v').to_string();
    let archive_name = asset_name(&tag, &source.os)?;
    let url_of = |wanted: &str| {
        meta["assets"].as_array().and_then(|a| {
            a.iter()
                .find(|x| x["name"].as_str() == Some(wanted))
                .and_then(|x| x["browser_download_url"].as_str())
                .map(String::from)
        })
    };
    Ok(Release {
        archive_url: url_of(&archive_name)
            .ok_or_else(|| format!("archive `{archive_name}` absente de {tag}"))?,
        sums_url: url_of("SHA256SUMS").ok_or_else(|| format!("SHA256SUMS absent de {tag}"))?,
        signature_url: url_of("SHA256SUMS.minisig"),
        tag,
        version,
        archive_name,
    })
}

/// Version publiée la plus récente, comparée à celle qui tourne.
pub async fn check(source: &Source) -> Result<Value, String> {
    let r = release(&client()?, source, None).await?;
    Ok(json!({
        "current": crate::VERSION,
        "latest": r.version,
        "tag": r.tag,
        "up_to_date": !is_newer(&r.version, crate::VERSION),
        "source_install": running_binary().is_ok_and(|b| is_source_build(&b)),
    }))
}

/// Chemin réel du binaire en cours.
pub fn running_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Vrai pour un binaire de compilation (`target/debug`, `target/release`).
pub fn is_source_build(exe: &Path) -> bool {
    let parts: Vec<String> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    parts
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
}

/// Binaire qu'une mise à jour peut remplacer. Un binaire de compilation ne se remplace
/// pas : il se met à jour par `make deploy`, ou bascule vers les releases (issue #33).
pub fn installable_binary(exe: &Path) -> Result<PathBuf, String> {
    if is_source_build(exe) {
        return Err(format!(
            "{} est un binaire de compilation : `penelope upgrade --switch` (ou `/upgrade \
             install` sur Telegram) bascule vers les releases, `make deploy` reste aux sources",
            exe.display()
        ));
    }
    Ok(exe.to_path_buf())
}

/// Chemin réel du binaire en cours, s'il peut être remplacé.
pub fn installed_binary() -> Result<PathBuf, String> {
    installable_binary(&running_binary()?)
}

/// Identité et identifiant de re-signature configurés (`upgrade.codesign_identity`).
pub fn codesign_of(cfg: &penelope_kernel::config::Config) -> Option<(&str, &str)> {
    let identity = cfg.upgrade.codesign_identity.trim();
    let identifier = match cfg.upgrade.codesign_identifier.trim() {
        "" => penelope_platform::codesign::DEFAULT_IDENTIFIER,
        id => id,
    };
    (!identity.is_empty()).then_some((identity, identifier))
}

/// Une installation.
pub struct Install<'a> {
    pub source: &'a Source,
    pub tag: Option<&'a str>,
    pub force: bool,
    pub binary: &'a Path,
    pub state_dir: &'a Path,
    pub now: String,
    /// Identité de re-signature macOS et identifiant fixe (issue #28).
    pub codesign: Option<(&'a str, &'a str)>,
}

/// Release téléchargée, vérifiée et extraite, prête à être mise en place.
struct Fetched {
    release: Release,
    fresh: PathBuf,
    work: PathBuf,
    signature: &'static str,
}

/// Télécharge, vérifie et met en place une release. Rien n'est touché avant que la somme
/// et `--version` soient vérifiées.
pub async fn install(opts: Install<'_>) -> Result<Value, String> {
    let client = client()?;
    let r = release(&client, opts.source, opts.tag).await?;
    let same = r.version == crate::VERSION;
    let older = !same && !is_newer(&r.version, crate::VERSION);
    if !opts.force && (same || (older && opts.tag.is_none())) {
        return Ok(json!({"up_to_date": true, "current": crate::VERSION, "latest": r.version}));
    }
    if !opts.force && parse_version(&r.version).is_some_and(|v| v < FIRST_SELF_UPGRADING) {
        return Err(format!(
            "{} ne sait ni confirmer ni annuler une mise à jour : `--force` pour l'installer \
             quand même, sans retour arrière automatique",
            r.version
        ));
    }
    let dir = opts.binary.parent().ok_or("binaire sans répertoire")?;
    preflight_writable(dir)?;
    let Fetched {
        release: r,
        fresh,
        work,
        signature,
    } = download(&client, opts.source, r, opts.state_dir).await?;

    // Signature stable avant la bascule : sans elle, macOS redemande l'accès au Trousseau
    // au premier démarrage du nouveau binaire.
    let codesign = match opts.codesign {
        _ if !cfg!(target_os = "macos") => Value::Null,
        Some((identity, identifier)) => {
            penelope_platform::codesign::sign(&fresh, identity, identifier)?;
            json!(format!("re-signé avec « {identity} »"))
        }
        None => json!(
            "non re-signé : macOS redemandera l'accès au Trousseau (upgrade.codesign_identity)"
        ),
    };

    swap_in(&fresh, opts.binary)?;
    let _ = std::fs::remove_dir_all(&work);
    let pending = Pending {
        from_version: crate::VERSION.to_string(),
        to_version: r.version.clone(),
        binary: opts.binary.to_path_buf(),
        previous: previous_path(opts.binary),
        attempts: 0,
        installed_at: opts.now,
        first_boot_ms: None,
        service: None,
    };
    write_pending(opts.state_dir, &pending)?;
    Ok(json!({
        "installed": r.version,
        "tag": r.tag,
        "from": crate::VERSION,
        "binary": opts.binary,
        "previous": pending.previous,
        "signature": signature,
        "codesign": codesign,
    }))
}

/// Sommes (et signature minisign), archive, extraction, `--version` : la release est prête.
async fn download(
    client: &reqwest::Client,
    source: &Source,
    r: Release,
    state_dir: &Path,
) -> Result<Fetched, String> {
    let sums_bytes = fetch(client, &r.sums_url, 64 * 1024).await?;
    let signature = match &source.pubkey {
        Some(key) => {
            let url = r.signature_url.as_deref().ok_or_else(|| {
                format!(
                    "{} n'est pas signée (SHA256SUMS.minisig absent) alors qu'une clé publique \
                     est configurée : mise à jour refusée",
                    r.tag
                )
            })?;
            let minisig = String::from_utf8(fetch(client, url, 16 * 1024).await?)
                .map_err(|_| "SHA256SUMS.minisig illisible".to_string())?;
            verify_signature(&sums_bytes, &minisig, key)?;
            "vérifiée"
        }
        None => "non vérifiée (aucune clé publique)",
    };
    let sums = String::from_utf8(sums_bytes).map_err(|_| "SHA256SUMS illisible".to_string())?;
    let expected = expected_sum(&sums, &r.archive_name)
        .ok_or_else(|| format!("aucune somme pour {}", r.archive_name))?;
    let archive = fetch(client, &r.archive_url, ARCHIVE_MAX_BYTES).await?;
    let actual = penelope_kernel::canonical::sha256_hex(&archive);
    if actual != expected {
        return Err(format!(
            "somme SHA-256 invalide pour {} : attendue {expected}, obtenue {actual}",
            r.archive_name
        ));
    }

    let work = state_dir.join("upgrade").join(&r.version);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive_path = work.join(&r.archive_name);
    std::fs::write(&archive_path, &archive).map_err(|e| e.to_string())?;
    penelope_platform::process::extract_tar_gz(&archive_path, &work).map_err(|e| e.to_string())?;
    let fresh = work.join("penelope");
    if !fresh.is_file() {
        return Err(format!("`penelope` absent de {}", r.archive_name));
    }
    set_executable(&fresh)?;
    let said = penelope_platform::process::binary_version(&fresh).map_err(|e| e.to_string())?;
    if !announces(&said, &r.version) {
        return Err(format!(
            "le binaire téléchargé annonce « {said} » au lieu de {}",
            r.version
        ));
    }
    Ok(Fetched {
        release: r,
        fresh,
        work,
        signature,
    })
}

fn previous_path(binary: &Path) -> PathBuf {
    PathBuf::from(format!("{}.previous", binary.display()))
}

fn preflight_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".penelope-upgrade-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "écriture impossible dans {} ({e}) : installer le binaire dans un répertoire \
             inscriptible par l'utilisateur du service, ou mettre à jour depuis les sources \
             (`make deploy`)",
            dir.display()
        )),
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Copie `from` à côté de `binary`, puis la renomme sur `binary` (atomique).
fn replace_with(from: &Path, binary: &Path) -> Result<(), String> {
    let dir = binary.parent().ok_or("binaire sans répertoire")?;
    let name = binary
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let staged = dir.join(format!(".{name}.staged"));
    std::fs::copy(from, &staged).map_err(|e| format!("{} : {e}", staged.display()))?;
    set_executable(&staged)?;
    std::fs::rename(&staged, binary).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("{} : {e}", binary.display())
    })
}

/// Garde le binaire courant en `.previous` et met le nouveau à sa place.
fn swap_in(fresh: &Path, binary: &Path) -> Result<(), String> {
    if binary.exists() {
        std::fs::copy(binary, previous_path(binary)).map_err(|e| e.to_string())?;
    }
    replace_with(fresh, binary)
}

/// Mise à jour en attente de confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub from_version: String,
    pub to_version: String,
    pub binary: PathBuf,
    pub previous: PathBuf,
    pub attempts: u32,
    pub installed_at: String,
    #[serde(default)]
    pub first_boot_ms: Option<i64>,
    /// Bascule d'une installation source vers les releases : fichier de service réécrit et
    /// sa sauvegarde, remise en place si la santé n'est pas confirmée (issue #33).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ServiceSwitch>,
}

/// Fichier de service réécrit par une bascule, et sa version d'origine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceSwitch {
    pub file: PathBuf,
    pub backup: PathBuf,
}

fn state_file(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade.json")
}

fn rolled_back_note(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade.rolled-back")
}

fn write_pending(state_dir: &Path, p: &Pending) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let raw = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    let tmp = state_dir.join("upgrade.json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, state_file(state_dir)).map_err(|e| e.to_string())
}

/// Mise à jour en attente, s'il y en a une.
pub fn pending(state_dir: &Path) -> Option<Pending> {
    std::fs::read_to_string(state_file(state_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Décision prise au démarrage de `penelope daemon`.
#[derive(Debug, Clone, PartialEq)]
pub enum Boot {
    /// Pas de mise à jour en cours.
    Normal,
    /// Nouveau binaire à l'essai : le chien de garde doit être armé.
    Trial { attempt: u32 },
    /// Nouveau binaire jamais confirmé : l'ancien est remis, le processus doit s'arrêter
    /// pour que le service reparte avec lui. `reload` : fichier de service restauré, à
    /// recharger (bascule vers les releases annulée).
    RolledBack {
        from: String,
        to: String,
        reload: Option<PathBuf>,
    },
}

/// À appeler avant d'ouvrir quoi que ce soit : un binaire qui plante plus loin est compté.
pub fn on_boot(state_dir: &Path, running_version: &str, now_ms: i64) -> Boot {
    let Some(mut p) = pending(state_dir) else {
        return Boot::Normal;
    };
    if running_version != p.to_version {
        // L'ancien binaire tourne : retour arrière fait, ou remplacement à la main.
        let _ = std::fs::remove_file(state_file(state_dir));
        return Boot::Normal;
    }
    p.attempts += 1;
    let first = *p.first_boot_ms.get_or_insert(now_ms);
    let expired = p.attempts > 1 && now_ms - first > HEALTH_WINDOW_MS;
    if expired || p.attempts > MAX_BOOT_ATTEMPTS {
        let _ = std::fs::remove_file(state_file(state_dir));
        let outcome = if p.previous.is_file() {
            replace_with(&p.previous, &p.binary)
        } else {
            Err(format!("{} introuvable", p.previous.display()))
        };
        let note = match &outcome {
            Ok(()) => format!("{}\n{}\n", p.to_version, p.from_version),
            Err(e) => format!("{}\n\n{e}\n", p.to_version),
        };
        let _ = std::fs::write(rolled_back_note(state_dir), note);
        // Bascule annulée : le service retrouve son fichier d'origine, donc le binaire de
        // compilation.
        let reload = p.service.as_ref().and_then(|sw| {
            std::fs::copy(&sw.backup, &sw.file)
                .ok()
                .map(|_| sw.file.clone())
        });
        return match outcome {
            Ok(()) => Boot::RolledBack {
                from: p.to_version,
                to: p.from_version,
                reload,
            },
            Err(_) => Boot::Normal,
        };
    }
    let attempt = p.attempts;
    let _ = write_pending(state_dir, &p);
    Boot::Trial { attempt }
}

/// `on_boot` à l'heure système.
pub fn on_boot_now(state_dir: &Path, running_version: &str) -> Boot {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    on_boot(state_dir, running_version, now_ms)
}

/// Quitte le processus si la santé n'est pas confirmée à temps : un nouveau binaire
/// bloqué finit ainsi en redémarrage, puis en retour arrière.
pub fn arm_watchdog(after: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        if !CONFIRMED.load(Ordering::SeqCst) {
            eprintln!(
                "mise à jour : santé non confirmée après {} s, arrêt pour retour arrière",
                after.as_secs()
            );
            std::process::exit(75);
        }
    });
}

/// Ce qu'il faut annoncer au propriétaire après un démarrage sain.
#[derive(Debug, Clone, PartialEq)]
pub enum Confirmation {
    Upgraded { from: String, to: String },
    RolledBack { from: String, to: String },
    RollbackFailed { from: String, error: String },
}

impl Confirmation {
    pub fn text(&self) -> String {
        match self {
            Confirmation::Upgraded { from, to } => {
                format!("⬆️ Pénélope est passée de {from} à {to}.")
            }
            Confirmation::RolledBack { from, to } => format!(
                "⚠️ La version {from} n'a pas démarré correctement : retour automatique à {to}."
            ),
            Confirmation::RollbackFailed { from, error } => format!(
                "❌ La version {from} ne démarre pas et le retour arrière a échoué ({error}) : \
                 réinstaller à la main."
            ),
        }
    }
}

/// Le daemon est sain : la mise à jour est validée.
pub fn confirm(state_dir: &Path, running_version: &str) -> Option<Confirmation> {
    CONFIRMED.store(true, Ordering::SeqCst);
    if let Ok(note) = std::fs::read_to_string(rolled_back_note(state_dir)) {
        let _ = std::fs::remove_file(rolled_back_note(state_dir));
        let mut lines = note.lines();
        let from = lines.next().unwrap_or("?").to_string();
        let to = lines.next().unwrap_or_default().to_string();
        return Some(if to.is_empty() {
            Confirmation::RollbackFailed {
                from,
                error: lines.collect::<Vec<_>>().join(" ").trim().to_string(),
            }
        } else {
            Confirmation::RolledBack { from, to }
        });
    }
    let p = pending(state_dir)?;
    if p.to_version != running_version {
        return None;
    }
    let _ = std::fs::remove_file(state_file(state_dir));
    Some(Confirmation::Upgraded {
        from: p.from_version,
        to: p.to_version,
    })
}

/// Après la reprise : quelques secondes de fonctionnement, base et journal accessibles,
/// puis confirmation et annonce.
pub async fn confirm_when_healthy(d: Arc<Daemon>) {
    let state_dir = d.services.platform.dirs.state();
    if pending(&state_dir).is_none() && !rolled_back_note(&state_dir).exists() {
        CONFIRMED.store(true, Ordering::SeqCst);
        return;
    }
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        if d.handle.is_shutting_down() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(e) = d.kv_get("upgrade.health").await {
        tracing::error!(error = %e, "mise à jour : base inaccessible, pas de confirmation");
        return;
    }
    let Some(c) = confirm(&state_dir, crate::VERSION) else {
        return;
    };
    let (kind, payload) = match &c {
        Confirmation::Upgraded { from, to } => {
            ("upgrade.confirmed", json!({"from": from, "to": to}))
        }
        Confirmation::RolledBack { from, to } => (
            "upgrade.rolled_back",
            json!({"from": from, "to": to, "automatic": true}),
        ),
        Confirmation::RollbackFailed { from, error } => (
            "upgrade.rollback_failed",
            json!({"from": from, "error": error}),
        ),
    };
    if let Err(e) = d
        .services
        .events
        .append(EventDraft::new(kind, payload))
        .await
    {
        tracing::error!(error = %e, "mise à jour : journal inaccessible");
    }
    tracing::info!("{}", c.text());
    // Telegram peut démarrer après la confirmation : on l'attend un peu pour l'annonce.
    for _ in 0..240 {
        if d.handle.is_shutting_down() {
            return;
        }
        if let Some(m) = d.hooks.messenger() {
            let origin = crate::scheduler::owner_origin(&d);
            let _ = m.send_text(&origin, &c.text()).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Retour manuel au binaire précédent. Le binaire courant devient `.previous` : un second
/// retour arrière revient à la version quittée.
pub fn manual_rollback(binary: &Path, state_dir: &Path) -> Result<Value, String> {
    let previous = previous_path(binary);
    if !previous.is_file() {
        return Err(format!("aucun binaire précédent ({})", previous.display()));
    }
    let target = penelope_platform::process::binary_version(&previous)
        .map_err(|e| format!("binaire précédent inutilisable : {e}"))?;
    let dir = binary.parent().ok_or("binaire sans répertoire")?;
    preflight_writable(dir)?;
    let keep = dir.join(".penelope.rollback");
    std::fs::copy(binary, &keep).map_err(|e| e.to_string())?;
    replace_with(&previous, binary)?;
    std::fs::rename(&keep, &previous).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(state_file(state_dir));
    Ok(json!({"rolled_back": true, "from": crate::VERSION, "to": target, "binary": binary}))
}

// ------------------------------------------------------------------ bascule vers les releases

/// Ce que la bascule touche hors du répertoire d'état : signature et service, remplaçables
/// en test.
pub trait SwitchHost: Send + Sync {
    /// Signe `path` avec l'identité et l'identifiant fixe.
    fn sign(&self, path: &Path, identity: &str, identifier: &str) -> Result<(), String>;
    /// Fichier du service qui lance le daemon ; `None` : service non géré.
    fn service_file(&self) -> Option<PathBuf>;
    /// Programme lancé par le fichier de service.
    fn service_program(&self, content: &str) -> Option<String>;
    /// Contenu du fichier de service lançant `exe`.
    fn service_with_program(&self, content: &str, exe: &Path) -> Option<String>;
    /// Recharge le service depuis son fichier, une fois la réponse partie.
    fn reload_service(&self, file: &Path) -> Result<(), String>;
}

/// Hôte réel : `codesign` et LaunchAgent.
pub struct SystemHost;

impl SwitchHost for SystemHost {
    fn sign(&self, path: &Path, identity: &str, identifier: &str) -> Result<(), String> {
        penelope_platform::codesign::sign(path, identity, identifier)
    }
    fn service_file(&self) -> Option<PathBuf> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        penelope_platform::service::launchd_plist_path().filter(|p| p.is_file())
    }
    fn service_program(&self, content: &str) -> Option<String> {
        penelope_platform::service::launchd_program(content)
    }
    fn service_with_program(&self, content: &str, exe: &Path) -> Option<String> {
        penelope_platform::service::launchd_with_program(content, exe)
    }
    fn reload_service(&self, file: &Path) -> Result<(), String> {
        penelope_platform::service::reload_launchd_detached(file, 2).map_err(|e| e.to_string())
    }
}

/// Une bascule d'installation source vers les releases.
pub struct Switch<'a> {
    pub source: &'a Source,
    pub tag: Option<&'a str>,
    /// Binaire de compilation qui tourne.
    pub current: &'a Path,
    /// Répertoire où installer le binaire de release (`upgrade.install_dir`).
    pub install_dir: &'a Path,
    pub state_dir: &'a Path,
    pub now: String,
    pub codesign: Option<(&'a str, &'a str)>,
    pub host: &'a dyn SwitchHost,
}

/// Préconditions vérifiées : fichier de service et son contenu.
pub struct Preflight {
    pub service_file: PathBuf,
    pub service: String,
}

/// Vérifie tout avant d'agir : identité de signature utilisable depuis le daemon,
/// répertoire cible inscriptible, service géré qui lance bien ce binaire et fichier
/// modifiable. Un message dit quoi configurer.
pub fn switch_preflight(opts: &Switch<'_>) -> Result<Preflight, String> {
    if !is_source_build(opts.current) {
        return Err(format!(
            "{} n'est pas un binaire de compilation : `/upgrade install` suffit",
            opts.current.display()
        ));
    }
    let Some((identity, identifier)) = opts.codesign else {
        return Err(
            "`upgrade.codesign_identity` n'est pas configuré : sans signature stable, macOS \
             redemanderait des autorisations que personne ne pourra accepter. Créer l'identité \
             (docs/install-headless.md, « Signature locale »), puis `penelope config set \
             upgrade.codesign_identity \"Penelope Dev\"`"
                .into(),
        );
    };
    std::fs::create_dir_all(opts.install_dir)
        .map_err(|e| format!("{} : {e}", opts.install_dir.display()))?;
    preflight_writable(opts.install_dir)?;
    // Essai de signature depuis le daemon : une identité dont la clé demande une
    // autorisation échoue ici plutôt qu'au redémarrage.
    let probe = opts
        .install_dir
        .join(format!(".penelope-sign-probe-{}", std::process::id()));
    let sample = ["/usr/bin/true", "/bin/true"]
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| opts.current.to_path_buf());
    std::fs::copy(&sample, &probe).map_err(|e| format!("essai de signature : {e}"))?;
    let signed = opts.host.sign(&probe, identity, identifier);
    let _ = std::fs::remove_file(&probe);
    signed.map_err(|e| {
        format!(
            "l'identité « {identity} » n'est pas utilisable depuis le daemon ({e}) : autoriser \
             `codesign` à utiliser sa clé (« Toujours autoriser », depuis une session graphique)"
        )
    })?;
    let service_file = opts.host.service_file().ok_or(
        "service non géré : la bascule demande le LaunchAgent (`penelope install`)".to_string(),
    )?;
    let service = std::fs::read_to_string(&service_file)
        .map_err(|e| format!("{} : {e}", service_file.display()))?;
    let program = opts.host.service_program(&service).ok_or_else(|| {
        format!(
            "{} ne déclare pas de programme lancé",
            service_file.display()
        )
    })?;
    let launched = std::fs::canonicalize(&program).unwrap_or_else(|_| PathBuf::from(&program));
    let current =
        std::fs::canonicalize(opts.current).unwrap_or_else(|_| opts.current.to_path_buf());
    if launched != current {
        return Err(format!(
            "le service lance {program}, pas ce binaire ({}) : `penelope uninstall && penelope \
             install` depuis le binaire voulu",
            opts.current.display()
        ));
    }
    let dir = service_file
        .parent()
        .ok_or("fichier de service sans répertoire")?;
    preflight_writable(dir)?;
    Ok(Preflight {
        service_file,
        service,
    })
}

/// Bascule vers les releases : release vérifiée, binaire installé et re-signé dans
/// `install_dir`, service réécrit (fichier d'origine sauvegardé) puis rechargé. La fenêtre
/// de santé s'applique : sans confirmation, le fichier d'origine revient avec le binaire de
/// compilation (issue #33).
pub async fn switch_to_releases(opts: Switch<'_>) -> Result<Value, String> {
    let pre = switch_preflight(&opts)?;
    let client = client()?;
    let r = release(&client, opts.source, opts.tag).await?;
    let Fetched {
        release: r,
        fresh,
        work,
        signature,
    } = download(&client, opts.source, r, opts.state_dir).await?;
    let (identity, identifier) = opts.codesign.ok_or("identité de signature absente")?;

    let target = opts.install_dir.join("penelope");
    replace_with(&fresh, &target)?;
    if let Err(e) = opts.host.sign(&target, identity, identifier) {
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("signature du binaire installé : {e}"));
    }
    let _ = std::fs::remove_dir_all(&work);

    let rewritten = opts
        .host
        .service_with_program(&pre.service, &target)
        .ok_or("réécriture du fichier de service impossible")?;
    let backup = PathBuf::from(format!("{}.sources", pre.service_file.display()));
    std::fs::write(&backup, &pre.service).map_err(|e| format!("{} : {e}", backup.display()))?;
    let pending = Pending {
        from_version: crate::VERSION.to_string(),
        to_version: r.version.clone(),
        binary: target.clone(),
        previous: opts.current.to_path_buf(),
        attempts: 0,
        installed_at: opts.now,
        first_boot_ms: None,
        service: Some(ServiceSwitch {
            file: pre.service_file.clone(),
            backup: backup.clone(),
        }),
    };
    write_pending(opts.state_dir, &pending)?;
    let tmp = PathBuf::from(format!("{}.tmp", pre.service_file.display()));
    std::fs::write(&tmp, &rewritten).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &pre.service_file).map_err(|e| e.to_string())?;
    if let Err(e) = opts.host.reload_service(&pre.service_file) {
        // Rien n'est relancé : tout revient comme avant.
        let _ = std::fs::copy(&backup, &pre.service_file);
        let _ = std::fs::remove_file(state_file(opts.state_dir));
        return Err(format!("rechargement du service : {e}"));
    }
    Ok(json!({
        "installed": r.version,
        "switched": true,
        "tag": r.tag,
        "from": crate::VERSION,
        "binary": target,
        "previous": opts.current,
        "service": pre.service_file,
        "signature": signature,
        "codesign": format!("re-signé avec « {identity} »"),
    }))
}

/// Méthode RPC `upgrade` : `check`, `rollback`, ou installation (`tag`, `force`). Une
/// installation ou un retour arrière réussi redémarre le daemon juste après la réponse.
pub async fn rpc(d: &Arc<Daemon>, p: &Value) -> anyhow::Result<Value> {
    let source = Source::from_config(&d.services.config.config());
    if p["check"].as_bool().unwrap_or(false) {
        return check(&source).await.map_err(anyhow::Error::msg);
    }
    if IN_PROGRESS.swap(true, Ordering::SeqCst) {
        anyhow::bail!("une mise à jour est déjà en cours");
    }
    let result = change(d, &source, p).await;
    IN_PROGRESS.store(false, Ordering::SeqCst);
    let mut v = result.map_err(anyhow::Error::msg)?;
    let kind = if v["installed"].is_string() {
        "upgrade.installed"
    } else if v["rolled_back"].as_bool() == Some(true) {
        "upgrade.rolled_back"
    } else {
        return Ok(v);
    };
    let _ = d
        .services
        .events
        .append(EventDraft::new(kind, v.clone()))
        .await;
    // Bascule : le service rechargé arrête et relance le daemon lui-même.
    if v["switched"].as_bool() == Some(true) {
        v["restart"] = json!(true);
        return Ok(v);
    }
    let daemon = d.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        daemon.handle.request_restart();
    });
    v["restart"] = json!(true);
    Ok(v)
}

async fn change(d: &Arc<Daemon>, source: &Source, p: &Value) -> Result<Value, String> {
    if p["switch"].as_bool() == Some(true) {
        let cfg = d.services.config.config();
        let install_dir = d.services.platform.dirs.expand(&cfg.upgrade.install_dir);
        return switch_to_releases(Switch {
            source,
            tag: p["tag"].as_str().filter(|t| !t.trim().is_empty()),
            current: &running_binary()?,
            install_dir: &install_dir,
            state_dir: &d.services.platform.dirs.state(),
            now: d.services.clock.now_rfc3339(),
            codesign: codesign_of(&cfg),
            host: &SystemHost,
        })
        .await;
    }
    let binary = installed_binary()?;
    let state_dir = d.services.platform.dirs.state();
    if p["rollback"].as_bool().unwrap_or(false) {
        return manual_rollback(&binary, &state_dir);
    }
    let cfg = d.services.config.config();
    install(Install {
        source,
        tag: p["tag"].as_str().filter(|t| !t.trim().is_empty()),
        force: p["force"].as_bool().unwrap_or(false),
        binary: &binary,
        state_dir: &state_dir,
        now: d.services.clock.now_rfc3339(),
        codesign: codesign_of(&cfg),
    })
    .await
}

/// Résumé lisible d'une réponse `upgrade`.
pub fn render(v: &Value) -> String {
    if let (Some(to), Some(true)) = (v["installed"].as_str(), v["switched"].as_bool()) {
        return format!(
            "📦 Bascule vers les releases : {to} installée dans `{}` et re-signée, service \
             réécrit (l'original est gardé). Il redémarre ; sans confirmation de santé, retour \
             automatique à l'installation depuis les sources. Les prochaines mises à jour se \
             feront par `/upgrade install`.",
            v["binary"].as_str().unwrap_or("?")
        );
    }
    if let Some(to) = v["installed"].as_str() {
        let codesign = v["codesign"]
            .as_str()
            .map(|c| format!(" Binaire {c}."))
            .unwrap_or_default();
        return format!(
            "⬆️ {to} installée (depuis {}, signature {}). Au redémarrage, retour automatique \
             à l'ancienne version si la nouvelle ne démarre pas.{codesign}",
            v["from"].as_str().unwrap_or("?"),
            v["signature"].as_str().unwrap_or("non vérifiée")
        );
    }
    if v["rolled_back"].as_bool() == Some(true) {
        return format!(
            "⏪ Binaire précédent remis en place ({}).",
            v["to"].as_str().unwrap_or("?")
        );
    }
    let current = v["current"].as_str().unwrap_or(crate::VERSION);
    match (v["up_to_date"].as_bool(), v["latest"].as_str()) {
        (Some(true), Some(latest)) => {
            format!("✅ À jour : {current} (dernière publiée : {latest}).")
        }
        (Some(false), Some(latest)) if v["source_install"].as_bool() == Some(true) => format!(
            "🆕 {latest} est disponible (installée : {current}). Installation depuis les \
             sources : `/upgrade install` propose de basculer vers les releases, `make deploy` \
             reste aux sources."
        ),
        (Some(false), Some(latest)) => format!(
            "🆕 {latest} est disponible (installée : {current}). `penelope upgrade` ou \
             `/upgrade install` pour l'installer."
        ),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn sums_and_versions_are_parsed_strictly() {
        let sums = format!(
            "{}  ./penelope-v1.0.0-macos-universal.tar.gz\n{}  ./penelope-v1.0.0-macos-x86_64.tar.gz\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            expected_sum(&sums, "penelope-v1.0.0-macos-universal.tar.gz"),
            Some("a".repeat(64))
        );
        assert_eq!(expected_sum(&sums, "penelope-v1.0.0-macos.tar.gz"), None);
        assert_eq!(expected_sum("court  ./x", "x"), None, "somme malformée");

        assert_eq!(parse_version("v0.3.10"), Some((0, 3, 10)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(announces("penelope 0.3.1", "0.3.1"));
        assert!(!announces("penelope 0.3.10", "0.3.1"), "pas de préfixe");
        assert!(asset_name("v1.0.0", "linux").is_err());
    }

    #[cfg(unix)]
    fn fake_binary(path: &Path, version: &str) {
        std::fs::write(path, format!("#!/bin/sh\necho \"penelope {version}\"\n")).unwrap();
        set_executable(path).unwrap();
    }

    fn pending_for(bin: &Path) -> Pending {
        Pending {
            from_version: "1.0.0".into(),
            to_version: "1.1.0".into(),
            binary: bin.to_path_buf(),
            previous: previous_path(bin),
            attempts: 0,
            installed_at: "t".into(),
            first_boot_ms: None,
            service: None,
        }
    }

    /// CA 2 : un upgrade volontairement cassé est annulé automatiquement.
    #[cfg(unix)]
    #[test]
    fn ca_2_8_a_broken_upgrade_is_rolled_back_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("penelope");
        let fresh = dir.path().join("fresh");
        let state = dir.path().join("state");
        fake_binary(&bin, "1.0.0");
        fake_binary(&fresh, "1.1.0");

        swap_in(&fresh, &bin).unwrap();
        assert!(std::fs::read_to_string(&bin).unwrap().contains("1.1.0"));
        write_pending(&state, &pending_for(&bin)).unwrap();

        // Plantages rapprochés dans la fenêtre de santé : le binaire garde sa chance.
        assert_eq!(on_boot(&state, "1.1.0", 1_000), Boot::Trial { attempt: 1 });
        assert_eq!(on_boot(&state, "1.1.0", 11_000), Boot::Trial { attempt: 2 });
        // Toujours pas confirmé passé 60 s : retour arrière.
        assert_eq!(
            on_boot(&state, "1.1.0", 1_000 + HEALTH_WINDOW_MS + 1),
            Boot::RolledBack {
                from: "1.1.0".into(),
                to: "1.0.0".into(),
                reload: None,
            }
        );
        assert!(std::fs::read_to_string(&bin).unwrap().contains("1.0.0"));

        // L'ancien binaire redémarre et l'annonce, une seule fois.
        assert_eq!(on_boot(&state, "1.0.0", 80_000), Boot::Normal);
        assert_eq!(
            confirm(&state, "1.0.0"),
            Some(Confirmation::RolledBack {
                from: "1.1.0".into(),
                to: "1.0.0".into()
            })
        );
        assert!(confirm(&state, "1.0.0").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn repeated_fast_crashes_also_roll_back_and_confirmation_is_announced_once() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("penelope");
        let state = dir.path().join("state");
        fake_binary(&bin, "1.0.0");
        std::fs::copy(&bin, previous_path(&bin)).unwrap();
        write_pending(&state, &pending_for(&bin)).unwrap();
        for i in 1..=MAX_BOOT_ATTEMPTS {
            assert_eq!(
                on_boot(&state, "1.1.0", i as i64),
                Boot::Trial { attempt: i }
            );
        }
        assert!(matches!(
            on_boot(&state, "1.1.0", 10),
            Boot::RolledBack { .. }
        ));

        write_pending(&state, &pending_for(&bin)).unwrap();
        let _ = std::fs::remove_file(rolled_back_note(&state));
        assert_eq!(on_boot(&state, "1.1.0", 0), Boot::Trial { attempt: 1 });
        assert_eq!(
            confirm(&state, "1.1.0").map(|c| c.text()),
            Some("⬆️ Pénélope est passée de 1.0.0 à 1.1.0.".to_string())
        );
        assert!(confirm(&state, "1.1.0").is_none());
        assert_eq!(
            on_boot(&state, "1.1.0", 5),
            Boot::Normal,
            "plus rien en attente"
        );
    }

    #[cfg(unix)]
    #[test]
    fn manual_rollback_toggles_between_the_two_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("penelope");
        let state = dir.path().join("state");
        fake_binary(&bin, "1.1.0");
        assert!(manual_rollback(&bin, &state).is_err(), "rien à restaurer");
        fake_binary(&previous_path(&bin), "1.0.0");

        let v = manual_rollback(&bin, &state).unwrap();
        assert_eq!(v["to"], "penelope 1.0.0");
        assert!(std::fs::read_to_string(&bin).unwrap().contains("1.0.0"));
        assert!(
            std::fs::read_to_string(previous_path(&bin))
                .unwrap()
                .contains("1.1.0")
        );
    }

    /// Faux GitHub : chaque chemin sert un contenu fixe, construit une fois l'adresse connue.
    async fn fake_releases(make: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(make(&base));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let files = files.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&buf).to_string();
                    let target = head.split_whitespace().nth(1).unwrap_or("");
                    let path = target.split('?').next().unwrap_or("").to_string();
                    let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                        Some((_, b)) => ("200 OK", b.clone()),
                        None => ("404 Not Found", b"{}".to_vec()),
                    };
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        base
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn install_verifies_the_archive_then_swaps_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        fake_binary(&pack.join("penelope"), "9.9.9");
        let archive = dir.path().join("a.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&pack)
            .arg("penelope")
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        let name = "penelope-v9.9.9-macos-universal.tar.gz";
        let good_sums = format!(
            "{}  ./{name}\n",
            penelope_kernel::canonical::sha256_hex(&bytes)
        );
        let bad_sums = format!("{}  ./{name}\n", "0".repeat(64));

        let server = fake_releases(|base| {
            let meta = |sums: &str| {
                json!([
                    {"tag_name": "v9.9.10", "draft": true, "assets": []},
                    {"tag_name": "v1.0.0", "assets": []},
                    {
                        "tag_name": "v9.9.9",
                        "prerelease": true,
                        "assets": [
                            {"name": name, "browser_download_url": format!("{base}/dl/{name}")},
                            {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/{sums}")},
                        ]
                    },
                ])
                .to_string()
                .into_bytes()
            };
            vec![
                ("/good".to_string(), meta("good-sums")),
                ("/bad".to_string(), meta("bad-sums")),
                (format!("/dl/{name}"), bytes.clone()),
                ("/dl/good-sums".to_string(), good_sums.into_bytes()),
                ("/dl/bad-sums".to_string(), bad_sums.into_bytes()),
            ]
        })
        .await;

        let bin = dir.path().join("bin").join("penelope");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fake_binary(&bin, crate::VERSION);
        let state = dir.path().join("state");
        let source = |prefix: &str| Source {
            releases_url: format!("{server}/{prefix}"),
            os: "macos".into(),
            pubkey: None,
        };

        let bad = source("bad");
        let err = install(Install {
            source: &bad,
            tag: None,
            force: false,
            binary: &bin,
            state_dir: &state,
            now: "t".into(),
            codesign: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert!(
            std::fs::read_to_string(&bin)
                .unwrap()
                .contains(crate::VERSION),
            "rien n'est touché sans somme valide"
        );
        assert!(pending(&state).is_none());

        let good = source("good");
        let v = install(Install {
            source: &good,
            tag: None,
            force: false,
            binary: &bin,
            state_dir: &state,
            now: "t".into(),
            codesign: None,
        })
        .await
        .unwrap();
        assert_eq!(v["installed"], "9.9.9");
        assert_eq!(v["signature"], "non vérifiée (aucune clé publique)");
        assert!(std::fs::read_to_string(&bin).unwrap().contains("9.9.9"));
        assert!(
            std::fs::read_to_string(previous_path(&bin))
                .unwrap()
                .contains(crate::VERSION)
        );
        let p = pending(&state).unwrap();
        assert_eq!(
            (p.from_version.as_str(), p.to_version.as_str()),
            (crate::VERSION, "9.9.9")
        );
        assert_eq!(on_boot(&state, "9.9.9", 0), Boot::Trial { attempt: 1 });

        // Clé publique configurée : une release sans signature est refusée, rien n'est
        // téléchargé au-delà des sommes.
        let _ = std::fs::remove_file(state.join("upgrade.json"));
        let mut signed_only = source("good");
        signed_only.pubkey = Some(TEST_PUBKEY.into());
        let err = install(Install {
            source: &signed_only,
            tag: None,
            force: true,
            binary: &bin,
            state_dir: &state,
            now: "t".into(),
            codesign: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("pas signée"), "{err}");
        assert!(pending(&state).is_none());
    }

    /// Hôte simulé : signataire et service en mémoire.
    #[derive(Default)]
    struct FakeHost {
        sign_fails: bool,
        file: PathBuf,
        signed: std::sync::Mutex<Vec<PathBuf>>,
        reloaded: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl SwitchHost for FakeHost {
        fn sign(&self, path: &Path, _identity: &str, _identifier: &str) -> Result<(), String> {
            if self.sign_fails {
                return Err("errSecInternalComponent".into());
            }
            self.signed.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
        fn service_file(&self) -> Option<PathBuf> {
            self.file.is_file().then(|| self.file.clone())
        }
        fn service_program(&self, content: &str) -> Option<String> {
            penelope_platform::service::launchd_program(content)
        }
        fn service_with_program(&self, content: &str, exe: &Path) -> Option<String> {
            penelope_platform::service::launchd_with_program(content, exe)
        }
        fn reload_service(&self, file: &Path) -> Result<(), String> {
            self.reloaded.lock().unwrap().push(file.to_path_buf());
            Ok(())
        }
    }

    /// Installation source simulée : binaire de compilation, service qui le lance, release
    /// 9.9.9 publiée.
    #[cfg(unix)]
    async fn source_install(dir: &Path) -> (PathBuf, PathBuf, String, String) {
        let current = dir.join("code/penelope/target/release/penelope");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        fake_binary(&current, crate::VERSION);
        let plist = dir.join("LaunchAgents/com.penelope.daemon.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        let original = penelope_platform::service::launchd_plist(
            &current,
            None,
            &dir.join("logs"),
            "/usr/bin:/bin",
        );
        std::fs::write(&plist, &original).unwrap();

        let pack = dir.join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        fake_binary(&pack.join("penelope"), "9.9.9");
        let archive = dir.join("a.tar.gz");
        assert!(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&pack)
                .arg("penelope")
                .status()
                .unwrap()
                .success()
        );
        let bytes = std::fs::read(&archive).unwrap();
        let name = "penelope-v9.9.9-macos-universal.tar.gz";
        let sums = format!(
            "{}  ./{name}\n",
            penelope_kernel::canonical::sha256_hex(&bytes)
        );
        let server = fake_releases(|base| {
            vec![
                (
                    "/r".to_string(),
                    json!([{
                        "tag_name": "v9.9.9",
                        "assets": [
                            {"name": name, "browser_download_url": format!("{base}/dl/{name}")},
                            {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/sums")},
                        ]
                    }])
                    .to_string()
                    .into_bytes(),
                ),
                (format!("/dl/{name}"), bytes.clone()),
                ("/dl/sums".to_string(), sums.into_bytes()),
            ]
        })
        .await;
        (current, plist, original, format!("{server}/r"))
    }

    /// Issue #33 : « Basculer » installe le binaire dans le répertoire cible, le re-signe,
    /// réécrit le service et le recharge ; la santé confirmée supprime `upgrade.json`, et
    /// la mise à jour suivante suit le parcours normal.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_source_install_switches_to_releases() {
        let dir = tempfile::tempdir().unwrap();
        let (current, plist, original, releases_url) = source_install(dir.path()).await;
        let host = FakeHost {
            file: plist.clone(),
            ..Default::default()
        };
        let source = Source {
            releases_url,
            os: "macos".into(),
            pubkey: None,
        };
        let install_dir = dir.path().join("home/.local/bin");
        let state = dir.path().join("state");
        assert!(
            installable_binary(&current)
                .unwrap_err()
                .contains("--switch")
        );

        let v = switch_to_releases(Switch {
            source: &source,
            tag: None,
            current: &current,
            install_dir: &install_dir,
            state_dir: &state,
            now: "t".into(),
            codesign: Some(("Penelope Test", "io.github.edouard-claude.penelope")),
            host: &host,
        })
        .await
        .unwrap();
        assert_eq!(v["switched"], true);
        let target = install_dir.join("penelope");
        assert!(std::fs::read_to_string(&target).unwrap().contains("9.9.9"));
        let signed = host.signed.lock().unwrap().clone();
        assert_eq!(signed.len(), 2, "essai de signature puis binaire installé");
        assert_eq!(signed[1], target);
        let rewritten = std::fs::read_to_string(&plist).unwrap();
        assert_eq!(
            penelope_platform::service::launchd_program(&rewritten).as_deref(),
            Some(target.to_string_lossy().as_ref())
        );
        let backup = PathBuf::from(format!("{}.sources", plist.display()));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        assert_eq!(host.reloaded.lock().unwrap().clone(), vec![plist.clone()]);
        let p = pending(&state).unwrap();
        assert_eq!(p.previous, current);
        assert_eq!(p.binary, target);
        assert!(render(&v).contains("Bascule vers les releases"));

        assert_eq!(on_boot(&state, "9.9.9", 1_000), Boot::Trial { attempt: 1 });
        assert_eq!(
            confirm(&state, "9.9.9"),
            Some(Confirmation::Upgraded {
                from: crate::VERSION.into(),
                to: "9.9.9".into()
            })
        );
        assert!(
            pending(&state).is_none(),
            "santé confirmée : upgrade.json supprimé"
        );
        // Après la bascule, le binaire lancé se met à jour par le parcours normal.
        assert!(!is_source_build(&target));
        assert_eq!(installable_binary(&target).unwrap(), target);
    }

    /// Issue #33 : santé non confirmée, le fichier de service d'origine revient et
    /// l'ancien binaire est relancé.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unconfirmed_switch_restores_the_source_service() {
        let dir = tempfile::tempdir().unwrap();
        let (current, plist, original, releases_url) = source_install(dir.path()).await;
        let host = FakeHost {
            file: plist.clone(),
            ..Default::default()
        };
        let source = Source {
            releases_url,
            os: "macos".into(),
            pubkey: None,
        };
        let install_dir = dir.path().join("bin");
        let state = dir.path().join("state");
        switch_to_releases(Switch {
            source: &source,
            tag: None,
            current: &current,
            install_dir: &install_dir,
            state_dir: &state,
            now: "t".into(),
            codesign: Some(("Penelope Test", "io.github.edouard-claude.penelope")),
            host: &host,
        })
        .await
        .unwrap();

        assert_eq!(on_boot(&state, "9.9.9", 1_000), Boot::Trial { attempt: 1 });
        assert_eq!(
            on_boot(&state, "9.9.9", 1_000 + HEALTH_WINDOW_MS + 1),
            Boot::RolledBack {
                from: "9.9.9".into(),
                to: crate::VERSION.into(),
                reload: Some(plist.clone()),
            }
        );
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), original);
        assert!(
            std::fs::read_to_string(install_dir.join("penelope"))
                .unwrap()
                .contains(crate::VERSION),
            "même relancé par l'ancien fichier, c'est l'ancien code qui tourne"
        );
        assert_eq!(on_boot(&state, crate::VERSION, 80_000), Boot::Normal);
        assert!(matches!(
            confirm(&state, crate::VERSION),
            Some(Confirmation::RolledBack { .. })
        ));
    }

    /// Issue #33 : identité absente ou inutilisable, rien n'est modifié et le message dit
    /// quoi configurer.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_switch_without_a_usable_identity_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (current, plist, original, releases_url) = source_install(dir.path()).await;
        let source = Source {
            releases_url,
            os: "macos".into(),
            pubkey: None,
        };
        let install_dir = dir.path().join("bin");
        let state = dir.path().join("state");
        for (host, codesign, expected) in [
            (
                FakeHost {
                    file: plist.clone(),
                    ..Default::default()
                },
                None,
                "upgrade.codesign_identity",
            ),
            (
                FakeHost {
                    file: plist.clone(),
                    sign_fails: true,
                    ..Default::default()
                },
                Some(("Penelope Test", "io.github.edouard-claude.penelope")),
                "n'est pas utilisable depuis le daemon",
            ),
        ] {
            let err = switch_to_releases(Switch {
                source: &source,
                tag: None,
                current: &current,
                install_dir: &install_dir,
                state_dir: &state,
                now: "t".into(),
                codesign,
                host: &host,
            })
            .await
            .unwrap_err();
            assert!(err.contains(expected), "{err}");
            assert!(!install_dir.join("penelope").exists());
            assert_eq!(std::fs::read_to_string(&plist).unwrap(), original);
            assert!(pending(&state).is_none());
            assert!(host.reloaded.lock().unwrap().is_empty());
        }
    }

    /// Vecteur de la crate `minisign-verify` : « test » signé par sa clé de test.
    const TEST_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    #[test]
    fn release_sums_are_checked_against_the_minisign_key() {
        verify_signature(b"test", TEST_SIGNATURE, TEST_PUBKEY).unwrap();
        let file_form =
            format!("untrusted comment: minisign public key E7620F1842B4E81F\n{TEST_PUBKEY}");
        verify_signature(b"test", TEST_SIGNATURE, &file_form).unwrap();
        let tampered = verify_signature(b"Test", TEST_SIGNATURE, TEST_PUBKEY).unwrap_err();
        assert!(tampered.contains("invalide"), "{tampered}");
        assert!(verify_signature(b"test", "n'importe quoi", TEST_PUBKEY).is_err());
        assert_eq!(release_pubkey("  RWQx  ").as_deref(), Some("RWQx"));
    }
}
