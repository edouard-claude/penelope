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

pub use crate::helpers::{is_source_build, running_binary};
use crate::ports::{Handle, Slot};
use crate::runtime::{Daemon, Services};
use penelope_kernel::event::EventDraft;
use penelope_platform::handoff::HandOff;
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

/// `0.3.1`, `v0.3.1` : les trois nombres. Une version à suffixe (`1.0.0-alpha.1`,
/// `0.17.60-rc1`, `1.2.3+build`) rend `None` : elle n'est jamais candidate à une mise à
/// jour automatique, ni retenue par `release` comme plus haute version publiée ; seul un
/// tag explicite l'installe. Issue #212 : le suffixe était coupé, donc une
/// `v1.0.0-alpha.1` publiée par erreur valait `1.0.0 > 0.17.59` et toutes les instances
/// 0.17 l'auraient installée à leur prochaine vérification.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let (numbers, suffixed) = version_parts(v)?;
    (!suffixed).then_some(numbers)
}

/// Les trois nombres, et si la version porte un suffixe (`-alpha.1`, `-rc1`, `+build`).
fn version_parts(v: &str) -> Option<((u64, u64, u64), bool)> {
    let v = v.trim().trim_start_matches('v');
    let (core, suffixed) = match v.find(['-', '+']) {
        Some(i) => (&v[..i], true),
        None => (v, false),
    };
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        (n.next()??, n.next()??, n.next().flatten().unwrap_or(0)),
        suffixed,
    ))
}

/// `candidate` remplace-t-elle `current` sans être demandée par son tag ? Jamais pour une
/// pré-release, ni pour une version illisible. Quand le binaire courant est lui-même une
/// pré-release (`1.0.0-alpha.7` sur la branche v1), sa version pleine le dépasse, une
/// 0.17 non (#212).
fn is_newer(candidate: &str, current: &str) -> bool {
    let Some(a) = parse_version(candidate) else {
        return false;
    };
    match version_parts(current) {
        Some((b, false)) => a > b,
        Some((b, true)) => a >= b,
        None => false,
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

/// Binaire qu'une mise à jour peut remplacer. Un binaire de compilation ne se remplace
/// pas : il se met à jour par `make deploy`, ou bascule vers les releases (issue #33).
pub fn installable_binary(exe: &Path) -> Result<PathBuf, String> {
    if is_source_build(exe) {
        return Err(format!(
            "{} est un binaire de compilation : `penelope upgrade --switch` (ou `/upgrade \
             install` sur Telegram) bascule vers les releases au chemin stable, `make deploy` y \
             installe une compilation",
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
}

pub fn state_file(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade.json")
}

pub fn rolled_back_note(state_dir: &Path) -> PathBuf {
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
    /// Nouveau binaire jamais confirmé : l'ancien est remis au même chemin, le processus
    /// doit s'arrêter pour que le service reparte avec lui (`KeepAlive`), sans toucher au
    /// fichier de service (issue #36).
    RolledBack { from: String, to: String },
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
        // Pourquoi l'essai a échoué (issue #153) : la carte disait seulement « n'a pas
        // démarré correctement », et il a fallu lire `daemon.err.log` à la main pour
        // trouver un débordement de pile. La dernière ligne utile y est reprise.
        let why = last_error_line(state_dir).unwrap_or_default();
        let note = match &outcome {
            Ok(()) => format!("{}\n{}\n{why}\n", p.to_version, p.from_version),
            Err(e) => format!("{}\n\n{e}\n", p.to_version),
        };
        let _ = std::fs::write(rolled_back_note(state_dir), note);
        return match outcome {
            Ok(()) => Boot::RolledBack {
                from: p.to_version,
                to: p.from_version,
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
    Upgraded {
        from: String,
        to: String,
    },
    RolledBack {
        from: String,
        to: String,
        /// Dernière ligne d'erreur du binaire à l'essai, quand on a pu la lire (#153).
        why: Option<String>,
    },
    RollbackFailed {
        from: String,
        error: String,
    },
}

/// Dernière ligne parlante de la sortie d'erreur du daemon : c'est là qu'un abandon
/// écrit ce qu'il a à dire (`stack overflow`, `fatal runtime error`), sans passer par le
/// journal JSON que le processus meurt avant d'écrire (issue #153).
fn last_error_line(state_dir: &Path) -> Option<String> {
    // `<state>/../Logs/Penelope/daemon.err.log` sur macOS, `<state>/daemon.err.log`
    // ailleurs : les deux sont tentés, le premier lisible gagne.
    let candidates = [
        state_dir.join("daemon.err.log"),
        state_dir.join("../Logs/Penelope/daemon.err.log"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Les dernières lignes seulement : le fichier grossit sans fin.
        let line = text
            .lines()
            .rev()
            .take(40)
            .map(str::trim)
            .find(|l| {
                !l.is_empty()
                    && (l.contains("overflow")
                        || l.contains("fatal")
                        || l.contains("panic")
                        || l.contains("erreur")
                        || l.contains("error"))
            })
            .map(|l| l.chars().take(200).collect::<String>());
        if line.is_some() {
            // La ligne part dans un message : elle passe par le rédacteur (#148).
            return line.map(|l| penelope_observe::redact(&l));
        }
    }
    None
}

impl Confirmation {
    pub fn text(&self) -> String {
        match self {
            Confirmation::Upgraded { from, to } => {
                format!("⬆️ Pénélope est passée de {from} à {to}.")
            }
            Confirmation::RolledBack { from, to, why } => match why {
                Some(w) if !w.is_empty() => format!(
                    "⚠️ La version {from} n'a pas démarré : retour automatique à {to}.\n\n\
                     Dernière erreur du binaire à l'essai :\n`{w}`"
                ),
                _ => format!(
                    "⚠️ La version {from} n'a pas démarré correctement : retour automatique \
                     à {to}."
                ),
            },
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
        let rest = lines.collect::<Vec<_>>().join(" ").trim().to_string();
        return Some(if to.is_empty() {
            Confirmation::RollbackFailed { from, error: rest }
        } else {
            Confirmation::RolledBack {
                from,
                to,
                why: (!rest.is_empty()).then_some(rest),
            }
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
pub async fn confirm_when_healthy(
    s: Arc<Services>,
    handle: Handle,
    messenger: Slot<dyn crate::executor::Messenger>,
) {
    let state_dir = s.platform.dirs.state();
    if pending(&state_dir).is_none() && !rolled_back_note(&state_dir).exists() {
        CONFIRMED.store(true, Ordering::SeqCst);
        return;
    }
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        if handle.is_shutting_down() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(e) = s.kv_get("upgrade.health").await {
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
        Confirmation::RolledBack { from, to, why } => (
            "upgrade.rolled_back",
            json!({"from": from, "to": to, "automatic": true, "why": why}),
        ),
        Confirmation::RollbackFailed { from, error } => (
            "upgrade.rollback_failed",
            json!({"from": from, "error": error}),
        ),
    };
    if let Err(e) = s.events.append(EventDraft::new(kind, payload)).await {
        tracing::error!(error = %e, "mise à jour : journal inaccessible");
    }
    tracing::info!("{}", c.text());
    // Telegram peut démarrer après la confirmation : on l'attend un peu pour l'annonce.
    for _ in 0..240 {
        if handle.is_shutting_down() {
            return;
        }
        if let Some(m) = messenger.get() {
            let origin = crate::helpers::owner_origin_of(&s);
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
    /// Domaine launchd du service (`gui/<uid>`).
    fn domain(&self) -> Result<String, String>;
    /// Confie le rechargement et le garde-fou à un job launchd à part (issue #36) : lancé
    /// depuis le daemon, un rechargement mourrait avec lui.
    fn hand_off(&self, h: &HandOff) -> Result<(), String>;
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
    fn domain(&self) -> Result<String, String> {
        penelope_platform::handoff::gui_domain().map_err(|e| e.to_string())
    }
    fn hand_off(&self, h: &HandOff) -> Result<(), String> {
        h.launch().map_err(|e| e.to_string())
    }
}

/// Relais d'une mise à jour : rechargement éventuel, puis garde-fou qui remet `previous`
/// au chemin stable si le nouveau binaire ne démarre jamais.
fn relay(
    host: &dyn SwitchHost,
    service_plist: &Path,
    p: &Pending,
    state_dir: &Path,
    reload: Option<&Path>,
) -> Result<HandOff, String> {
    Ok(HandOff {
        domain: host.domain()?,
        service_label: penelope_platform::service::SERVICE_LABEL.to_string(),
        service_plist: service_plist.to_path_buf(),
        backup_plist: reload.map(Path::to_path_buf),
        reload: reload.is_some(),
        binary: p.binary.clone(),
        previous: Some(p.previous.clone()),
        state_file: state_file(state_dir),
        rolled_back_note: rolled_back_note(state_dir),
        to_version: p.to_version.clone(),
        from_version: p.from_version.clone(),
        delay_s: if reload.is_some() { 2 } else { 0 },
        guard_s: penelope_platform::handoff::GUARD_S,
        work_dir: state_dir.join("upgrade").join("relay"),
        launchctl: PathBuf::from(penelope_platform::handoff::LAUNCHCTL),
    })
}

/// Après une mise à jour ordinaire, un garde-fou indépendant du nouveau binaire : s'il ne
/// démarre jamais (tué au lancement, signature refusée…), le précédent revient. Rien à
/// garder quand le service n'est pas géré ou lance un autre binaire. Rend vrai si le relais
/// est parti.
pub fn guard_install(host: &dyn SwitchHost, state_dir: &Path) -> Result<bool, String> {
    let Some(p) = pending(state_dir) else {
        return Ok(false);
    };
    let Some(file) = host.service_file() else {
        return Ok(false);
    };
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let launched = host
        .service_program(&content)
        .map(|prog| std::fs::canonicalize(&prog).unwrap_or_else(|_| PathBuf::from(prog)));
    let binary = std::fs::canonicalize(&p.binary).unwrap_or_else(|_| p.binary.clone());
    if launched.as_deref() != Some(binary.as_path()) {
        return Ok(false);
    }
    host.hand_off(&relay(host, &file, &p, state_dir, None)?)?;
    Ok(true)
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
/// `install_dir` (le chemin stable), service réécrit une dernière fois (fichier d'origine
/// sauvegardé), puis rechargé par un relais launchd hors du job du daemon. Si le nouveau
/// binaire ne démarre pas, le binaire de compilation est copié au chemin stable : le
/// service ne change plus de programme (issues #33 et #36).
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
    };
    let relay = relay(
        opts.host,
        &pre.service_file,
        &pending,
        opts.state_dir,
        Some(&backup),
    )?;
    write_pending(opts.state_dir, &pending)?;
    let tmp = PathBuf::from(format!("{}.tmp", pre.service_file.display()));
    std::fs::write(&tmp, &rewritten).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &pre.service_file).map_err(|e| e.to_string())?;
    if let Err(e) = opts.host.hand_off(&relay) {
        // Rien n'est relancé : tout revient comme avant.
        let _ = std::fs::copy(&backup, &pre.service_file);
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_file(state_file(opts.state_dir));
        return Err(format!("relais de rechargement du service : {e}"));
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
    let mut v = install(Install {
        source,
        tag: p["tag"].as_str().filter(|t| !t.trim().is_empty()),
        force: p["force"].as_bool().unwrap_or(false),
        binary: &binary,
        state_dir: &state_dir,
        now: d.services.clock.now_rfc3339(),
        codesign: codesign_of(&cfg),
    })
    .await?;
    if v["installed"].is_string() && cfg!(target_os = "macos") {
        match guard_install(&SystemHost, &state_dir) {
            Ok(armed) => v["guard"] = json!(armed),
            Err(e) => tracing::warn!(error = %e, "mise à jour : garde-fou non lancé"),
        }
    }
    Ok(v)
}

/// Résumé lisible d'une réponse `upgrade`.
pub fn render(v: &Value) -> String {
    if let (Some(to), Some(true)) = (v["installed"].as_str(), v["switched"].as_bool()) {
        return format!(
            "📦 Bascule vers les releases : {to} installée dans `{}` et re-signée, service \
             réécrit vers ce chemin stable. Un relais launchd le recharge ; si la nouvelle \
             version ne démarre pas, le binaire de compilation revient au même chemin. Les \
             prochaines mises à jour se feront par `/upgrade install`.",
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
             installe une compilation au chemin stable."
        ),
        (Some(false), Some(latest)) => format!(
            "🆕 {latest} est disponible (installée : {current}). `penelope upgrade` ou \
             `/upgrade install` pour l'installer."
        ),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests;
