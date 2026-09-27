//! Bascule d'une installation depuis les sources vers les releases (issue #33).

use super::*;

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
