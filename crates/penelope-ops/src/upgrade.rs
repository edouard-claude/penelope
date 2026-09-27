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
use penelope_app::services::Services;
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
    // HTTPS, ou HTTP vers la boucle locale exacte, jamais d'identifiants dans l'URL.
    crate::helpers::check_endpoint(url)?;
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

/// Release retenue, avant d'y chercher l'archive de l'OS.
#[derive(Debug, Clone)]
pub struct Published {
    pub tag: String,
    pub version: String,
    /// La release telle que l'API la rend (`assets`).
    pub meta: Value,
}

/// Retient une release (`tag` absent : la plus haute version publiée) sans exiger son
/// archive : `check` dit qu'une version existe sur tout OS, même sans artefact publié
/// pour lui (sous Linux, `check` échouait sur « pas d'artefact publié »). Les versions
/// 0.x sont publiées en pre-release, que `releases/latest` ignore : on lit la liste.
pub async fn latest_tag(
    client: &reqwest::Client,
    source: &Source,
    tag: Option<&str>,
) -> Result<Published, String> {
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
    Ok(Published { tag, version, meta })
}

/// Résout une release et son archive pour l'OS : ce qu'une installation télécharge.
pub async fn release(
    client: &reqwest::Client,
    source: &Source,
    tag: Option<&str>,
) -> Result<Release, String> {
    let Published { tag, version, meta } = latest_tag(client, source, tag).await?;
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
    let r = latest_tag(&client()?, source, None).await?;
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

mod boot;
pub use boot::*;
mod switch;
pub use switch::*;

/// Méthode RPC `upgrade` : `check`, `rollback`, ou installation (`tag`, `force`). Une
/// installation ou un retour arrière réussi redémarre le daemon juste après la réponse.
pub async fn rpc(s: &Arc<Services>, handle: &Handle, p: &Value) -> anyhow::Result<Value> {
    let source = Source::from_config(&s.config.config());
    if p["check"].as_bool().unwrap_or(false) {
        return check(&source).await.map_err(anyhow::Error::msg);
    }
    if IN_PROGRESS.swap(true, Ordering::SeqCst) {
        anyhow::bail!("une mise à jour est déjà en cours");
    }
    let result = change(s, &source, p).await;
    IN_PROGRESS.store(false, Ordering::SeqCst);
    let mut v = result.map_err(anyhow::Error::msg)?;
    let kind = if v["installed"].is_string() {
        "upgrade.installed"
    } else if v["rolled_back"].as_bool() == Some(true) {
        "upgrade.rolled_back"
    } else {
        return Ok(v);
    };
    let _ = s.events.append(EventDraft::new(kind, v.clone())).await;
    // Bascule : le service rechargé arrête et relance le daemon lui-même.
    if v["switched"].as_bool() == Some(true) {
        v["restart"] = json!(true);
        return Ok(v);
    }
    let handle = handle.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        handle.request_restart();
    });
    v["restart"] = json!(true);
    Ok(v)
}

async fn change(s: &Services, source: &Source, p: &Value) -> Result<Value, String> {
    if p["switch"].as_bool() == Some(true) {
        let cfg = s.config.config();
        let install_dir = s.platform.dirs.expand(&cfg.upgrade.install_dir);
        return switch_to_releases(Switch {
            source,
            tag: p["tag"].as_str().filter(|t| !t.trim().is_empty()),
            current: &running_binary()?,
            install_dir: &install_dir,
            state_dir: &s.platform.dirs.state(),
            now: s.clock.now_rfc3339(),
            codesign: codesign_of(&cfg),
            host: &SystemHost,
        })
        .await;
    }
    let binary = installed_binary()?;
    let state_dir = s.platform.dirs.state();
    if p["rollback"].as_bool().unwrap_or(false) {
        return manual_rollback(&binary, &state_dir);
    }
    let cfg = s.config.config();
    let mut v = install(Install {
        source,
        tag: p["tag"].as_str().filter(|t| !t.trim().is_empty()),
        force: p["force"].as_bool().unwrap_or(false),
        binary: &binary,
        state_dir: &state_dir,
        now: s.clock.now_rfc3339(),
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
