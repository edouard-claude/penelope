//! Générations à chaud de la configuration (§4.4).

use super::*;

/// Une génération immuable de la configuration.
#[derive(Debug, Clone)]
pub struct Generation {
    pub generation: u64,
    pub config: Arc<Config>,
    pub changed: Vec<String>,
    pub source: String,
    pub ts: String,
}

/// Résultat d'application par un sous-système (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ApplyResult {
    AppliedLive {
        #[serde(rename = "gen")]
        generation: u64,
    },
    Rejected {
        #[serde(rename = "gen")]
        generation: u64,
        reason: String,
    },
    RequiresRestart {
        #[serde(rename = "gen")]
        generation: u64,
        reason: String,
    },
}

impl ApplyResult {
    pub fn generation(&self) -> u64 {
        match self {
            ApplyResult::AppliedLive { generation }
            | ApplyResult::Rejected { generation, .. }
            | ApplyResult::RequiresRestart { generation, .. } => *generation,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            ApplyResult::AppliedLive { .. } => "applied_live",
            ApplyResult::Rejected { .. } => "rejected",
            ApplyResult::RequiresRestart { .. } => "requires_restart",
        }
    }
}

/// Chemins pour lesquels `RequiresRestart` est acceptable (§4.4).
pub const RESTART_ONLY_PATHS: &[&str] = &[
    "store.path",
    "rpc.socket",
    "telegram.token",
    "observability.runtime_stream_bind",
    "observability.runtime_consumers",
];

pub fn restart_allowed(path: &str) -> bool {
    RESTART_ONLY_PATHS.iter().any(|p| path.starts_with(p))
}

/// Publication et historique des générations.
pub struct ConfigStore {
    current: ArcSwap<Generation>,
    counter: AtomicU64,
    path: PathBuf,
    store: Option<Store>,
    clock: SharedClock,
    results: std::sync::Mutex<BTreeMap<String, ApplyResult>>,
    tx: tokio::sync::broadcast::Sender<Arc<Generation>>,
    /// Sérialise les mutations : lecture de l'instantané, écriture du fichier et
    /// publication sous un seul verrou, sinon deux mutations concurrentes s'écrasent et
    /// se volent leur fichier temporaire (issue #45).
    writing: std::sync::Mutex<()>,
    /// Clés du fichier que ce binaire ne connaît pas, à la dernière lecture (#76).
    unknown: std::sync::Mutex<Vec<String>>,
}

/// Les racines absolues existantes prennent la forme réelle du volume. Les chemins
/// encore absents et les gabarits (`~`, `{data}`) restent tels quels ; ils seront
/// résolus au moment de l'usage. Aucune comparaison de casse artificielle (#164).
fn canonicalise_workspace_paths(config: &mut Config) {
    for raw in &mut config.sandbox.workspaces {
        let path = Path::new(raw);
        if !path.is_absolute() {
            continue;
        }
        match std::fs::canonicalize(path) {
            Ok(real) => *raw = real.to_string_lossy().into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(workspace = %raw, "workspace configuré inexistant");
            }
            Err(e) => tracing::warn!(workspace = %raw, error = %e, "workspace non canonicalisé"),
        }
    }
}

impl ConfigStore {
    pub fn new(
        mut config: Config,
        path: impl AsRef<Path>,
        store: Option<Store>,
        clock: SharedClock,
    ) -> Self {
        canonicalise_workspace_paths(&mut config);
        let boot = Generation {
            generation: 1,
            config: Arc::new(config),
            changed: Vec::new(),
            source: "boot".into(),
            ts: clock.now_rfc3339(),
        };
        let (tx, _) = tokio::sync::broadcast::channel(32);
        ConfigStore {
            current: ArcSwap::from_pointee(boot),
            counter: AtomicU64::new(1),
            path: path.as_ref().to_path_buf(),
            store,
            clock,
            results: std::sync::Mutex::new(BTreeMap::new()),
            tx,
            writing: std::sync::Mutex::new(()),
            unknown: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Charge depuis le disque, ou crée le fichier à partir des valeurs par défaut.
    pub fn load_or_create(
        path: impl AsRef<Path>,
        store: Option<Store>,
        clock: SharedClock,
        owner_id: i64,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut unknown = Vec::new();
        let cfg = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let (mut c, u) = Config::parse(&raw)?;
            if !u.is_empty() {
                tracing::warn!(
                    cles = %u.join(", "),
                    "clés de configuration inconnues de cette version : ignorées"
                );
            }
            unknown = u;
            // L'ancien défaut d'embeddings visait un serveur local désactivé : la recherche
            // restait lexicale sans le dire (issue #11).
            if !c.providers.local.enabled
                && let Some(alias) = c.models.aliases.get_mut("embedding")
                && alias == LEGACY_EMBEDDING_MODEL
            {
                tracing::warn!(
                    ancien = LEGACY_EMBEDDING_MODEL,
                    nouveau = DEFAULT_EMBEDDING_MODEL,
                    "alias `embedding` sans serveur local : bascule sur OpenRouter"
                );
                *alias = DEFAULT_EMBEDDING_MODEL.to_string();
            }
            c
        } else {
            let c = Config::sample(owner_id);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            atomic_write(&path, Config::sample_toml(owner_id)?.as_bytes())?;
            c
        };
        let cs = ConfigStore::new(cfg, path, store, clock);
        cs.set_unknown(unknown);
        Ok(cs)
    }

    /// Clés du fichier ignorées à la dernière lecture : `doctor` les nomme (#76).
    pub fn unknown_keys(&self) -> Vec<String> {
        match self.unknown.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    fn set_unknown(&self, keys: Vec<String>) {
        match self.unknown.lock() {
            Ok(mut g) => *g = keys,
            Err(p) => *p.into_inner() = keys,
        }
    }

    /// Instantané courant : c'est ce que lit un tour à son démarrage (§4.4).
    pub fn snapshot(&self) -> Arc<Generation> {
        self.current.load_full()
    }

    pub fn config(&self) -> Arc<Config> {
        self.current.load().config.clone()
    }

    pub fn generation(&self) -> u64 {
        self.current.load().generation
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<Generation>> {
        self.tx.subscribe()
    }

    /// Applique une mutation : valide, persiste, publie la génération N+1.
    ///
    /// Une mutation qui échoue à la validation **ne publie rien** : la génération
    /// courante reste en place (§12.6 pour les workflows, même principe ici).
    /// Deux mutations concurrentes s'exécutent l'une après l'autre : la seconde part de
    /// ce que la première a publié, jamais d'un instantané périmé (issue #45).
    pub fn mutate<F>(&self, source: &str, f: F) -> Result<Arc<Generation>>
    where
        F: FnOnce(&mut Config) -> Result<Vec<String>>,
    {
        let _writing = self.lock_writing();
        self.apply(source, true, f)
    }

    fn lock_writing(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.writing.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Corps d'une mutation, verrou déjà tenu. `persist` : écrire le fichier (faux pour
    /// une relecture, dont le fichier est la source).
    fn apply<F>(&self, source: &str, persist: bool, f: F) -> Result<Arc<Generation>>
    where
        F: FnOnce(&mut Config) -> Result<Vec<String>>,
    {
        let cur = self.current.load_full();
        let mut next = (*cur.config).clone();
        let changed = f(&mut next)?;
        canonicalise_workspace_paths(&mut next);
        next.validate()?;

        if persist {
            // Seules les clés modifiées sont réécrites (#76) : le fichier garde ses
            // commentaires, son ordre, ses clés inconnues, et n'acquiert pas les clés
            // nouvelles de cette version. Sans fichier, on part des valeurs par défaut.
            let text = match std::fs::read_to_string(&self.path) {
                Ok(existing) => edit_toml(&existing, &cur.config, &next)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    format!(
                        "{SAMPLE_HEADER}{}",
                        edit_toml("", &Config::default(), &next)?
                    )
                }
                Err(e) => return Err(e.into()),
            };
            atomic_write(&self.path, text.as_bytes())?;
        }

        let gen_no = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let g = Arc::new(Generation {
            generation: gen_no,
            config: Arc::new(next),
            changed: changed.clone(),
            source: source.to_string(),
            ts: self.clock.now_rfc3339(),
        });
        self.current.store(g.clone());

        if let Some(store) = &self.store {
            let snapshot = serde_json::to_string(&*g.config).unwrap_or_default();
            let changed_s = serde_json::to_string(&changed).unwrap_or_default();
            let ts = g.ts.clone();
            let src = g.source.clone();
            let _ = store.write_blocking(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO config_generations(gen, ts, source, changed, snapshot)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![gen_no as i64, ts, src, changed_s, snapshot],
                )?;
                Ok(())
            });
        }

        let _ = self.tx.send(g.clone());
        Ok(g)
    }

    /// Recharge depuis le fichier (watcher du §2.9).
    ///
    /// La lecture est sous le même verrou que les mutations : un `config set` qui écrit
    /// entre la lecture et la publication ne se fait pas écraser par une relecture
    /// périmée (issue #45).
    pub fn reload_from_disk(&self) -> Result<Arc<Generation>> {
        let _writing = self.lock_writing();
        let raw = std::fs::read_to_string(&self.path)?;
        let (parsed, unknown) = Config::parse(&raw)?;
        let g = self.apply("file", false, move |c| {
            let changed = diff_paths(c, &parsed);
            *c = parsed;
            Ok(changed)
        })?;
        self.set_unknown(unknown);
        Ok(g)
    }

    /// Enregistre le résultat d'application d'un sous-système.
    ///
    /// Un résultat pour une génération **ancienne** ne peut pas écraser celui d'une
    /// génération plus récente (§4.4).
    pub fn record_apply(&self, subsystem: &str, result: ApplyResult) -> bool {
        let mut guard = match self.results.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(existing) = guard.get(subsystem)
            && existing.generation() > result.generation()
        {
            return false;
        }
        if let ApplyResult::RequiresRestart { generation, reason } = &result
            && !restart_allowed(reason)
        {
            tracing::warn!(
                subsystem,
                generation,
                reason,
                "RequiresRestart refusé : ce chemin doit s'appliquer à chaud"
            );
        }
        if let Some(store) = &self.store {
            let sub = subsystem.to_string();
            let (kind, reason, gen_no) = match &result {
                ApplyResult::AppliedLive { generation } => ("applied_live", None, *generation),
                ApplyResult::Rejected { generation, reason } => {
                    ("rejected", Some(reason.clone()), *generation)
                }
                ApplyResult::RequiresRestart { generation, reason } => {
                    ("requires_restart", Some(reason.clone()), *generation)
                }
            };
            let ts = self.clock.now_rfc3339();
            let _ = store.write_blocking(move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO subsystem_apply_results(gen, subsystem, result, reason, ts)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![gen_no as i64, sub, kind, reason, ts],
                )?;
                Ok(())
            });
        }
        guard.insert(subsystem.to_string(), result);
        true
    }

    pub fn apply_results(&self) -> BTreeMap<String, ApplyResult> {
        match self.results.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}
