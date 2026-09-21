//! `penelope-store` : accès SQLite (WAL), migrations versionnées, acteur écrivain unique.
//!
//! Ce crate est une **infrastructure pure** : il ne connaît aucun type métier et ne
//! dépend d'aucun autre crate `penelope-*` (§3.1). Les autres crates lui passent des
//! closures qui reçoivent une `Transaction` (écriture) ou une `Connection` (lecture).
//!
//! Garanties :
//! - un **seul** écrivain (thread dédié, file MPSC) : jamais de `SQLITE_BUSY` en écriture ;
//! - lectures concurrentes sur un pool de connexions en lecture seule ;
//! - toute écriture est une transaction : `kill -9` ne laisse jamais d'état partiel (§0.2) ;
//! - une **panique** dans une closure d'écriture annule sa transaction, est journalisée et
//!   comptée, puis renvoyée à son demandeur : l'écrivain survit et les écritures suivantes
//!   passent (issue #44) ;
//! - les écritures ordinaires sont durables contre le **processus** (`synchronous=NORMAL`
//!   en WAL) ; `write_durable` l'est contre la **machine** (coupure, panique noyau) pour
//!   les transitions qui ne doivent jamais être perdues, celles du ledger d'effets
//!   (issue #75).

#![forbid(unsafe_code)]

mod migrations;
mod pool;
mod vector;

pub use migrations::{MIGRATIONS, Migration, applied_versions, current_version};
pub use pool::ReadPool;
pub use vector::{cosine_similarity, decode_embedding, encode_embedding};

/// Ré-export : les crates métier n'ajoutent pas `rusqlite` à leurs dépendances, tout
/// l'accès SQLite transite par `penelope-store` (§3.1).
pub use rusqlite;

use rusqlite::{Connection, OpenFlags, Transaction};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite : {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("entrée-sortie : {0}")]
    Io(#[from] std::io::Error),

    #[error("json : {0}")]
    Json(#[from] serde_json::Error),

    #[error("migration {version} a échoué : {source}")]
    Migration {
        version: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("base corrompue : {0}")]
    Corrupt(String),

    #[error("l'acteur écrivain est arrêté")]
    WriterGone,

    #[error("panique dans l'écrivain : {0}")]
    WriterPanic(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

impl StoreError {
    pub fn other(m: impl Into<String>) -> Self {
        StoreError::Other(m.into())
    }
}

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send>;

/// Paniques survenues dans une closure d'écriture depuis le démarrage du processus
/// (`penelope_store_writer_panics_total`, issue #44).
static WRITER_PANICS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Compteur de paniques de l'écrivain : zéro attendu, contrôlé par `penelope doctor`.
pub fn writer_panics() -> u64 {
    WRITER_PANICS.load(Ordering::SeqCst)
}

/// Message d'une panique, tel que le thread l'aurait affiché.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&'static str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panique sans message".into())
}

/// Exécute une écriture en filet : une panique annule la transaction (rollback au drop),
/// est journalisée et comptée, et repart vers le demandeur au lieu de tuer l'écrivain.
fn guarded<T, F>(conn: &mut Connection, f: F) -> Result<T>
where
    F: FnOnce(&Transaction<'_>) -> Result<T>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_in_transaction(conn, f))) {
        Ok(r) => r,
        Err(payload) => {
            let msg = panic_text(payload.as_ref());
            WRITER_PANICS.fetch_add(1, Ordering::SeqCst);
            tracing::error!(
                panique = %msg,
                "panique dans une écriture : transaction annulée, l'écrivain continue"
            );
            Err(StoreError::WriterPanic(msg))
        }
    }
}

/// Poignée clonable vers la base.
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}

/// Ce que l'intégrité de la base dit, et de quelle bouche (issue #158).
#[derive(Debug, Clone)]
pub struct IntegrityReport {
    /// Le verdict d'un lecteur du pool.
    pub pool: String,
    /// Celui d'une connexion neuve, demandé seulement si le pool a accusé la base. C'est
    /// lui qui fait foi sur l'état du **fichier**.
    pub fresh: Option<String>,
    /// Âge et service de la connexion du pool interrogée.
    pub reader: crate::pool::ReaderStats,
}

impl IntegrityReport {
    /// Le verdict qui fait foi : celui de la connexion neuve dès qu'on l'a demandé.
    pub fn verdict(&self) -> &str {
        self.fresh.as_deref().unwrap_or(&self.pool)
    }

    /// Tout va bien, des deux côtés.
    pub fn sound(&self) -> bool {
        self.verdict() == "ok"
    }

    /// Le pool accuse une base que le fichier dément : c'est la connexion qui ment.
    pub fn reader_lied(&self) -> bool {
        self.pool != "ok" && self.fresh.as_deref() == Some("ok")
    }

    /// Les index dérivés nommés par le verdict qui fait foi, s'il ne parle que d'eux.
    pub fn fts_only(&self) -> Option<Vec<String>> {
        (!self.sound())
            .then(|| fts_tables(self.verdict()))
            .flatten()
    }
}

struct StoreInner {
    path: PathBuf,
    /// Index FTS5 reconstruits à l'ouverture (issue #158).
    repaired_fts: Vec<String>,
    writer_tx: mpsc::UnboundedSender<WriteJob>,
    readers: ReadPool,
    closed: AtomicBool,
    /// Transactions validées par `write_durable` (`penelope_store_durable_commits_total`).
    durable_commits: Arc<std::sync::atomic::AtomicU64>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.inner.path)
            .finish()
    }
}

impl Store {
    /// Ouvre (ou crée) la base, applique les migrations, démarre l'acteur écrivain.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut writer = open_connection(&path, false)?;
        let repaired_fts = ensure_sound(&writer)?;
        migrations::migrate(&mut writer)?;

        let readers = ReadPool::new(&path, 4)?;

        let (tx, mut rx) = mpsc::unbounded_channel::<WriteJob>();
        std::thread::Builder::new()
            .name("penelope-store-writer".into())
            // Pile explicite et généreuse (issue #153) : ce thread porte des travaux de
            // maintenance qui traversent toute une table, et un débordement de pile
            // **abat le processus** — ce n'est pas une panique, le filet de #44 ne le
            // voit pas. Le défaut de 2 Mio a suffi à faire revenir en arrière deux
            // versions de suite.
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    // Second filet : une panique hors transaction (maintenance) ne doit
                    // pas non plus emporter la file d'écriture (#44).
                    if let Err(payload) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut writer)))
                    {
                        WRITER_PANICS.fetch_add(1, Ordering::SeqCst);
                        tracing::error!(
                            panique = %panic_text(payload.as_ref()),
                            "panique dans l'écrivain : travail abandonné, la file continue"
                        );
                    }
                }
                // Fermeture propre : checkpoint WAL pour laisser une base compacte.
                let _ = writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            })
            .map_err(StoreError::Io)?;

        Ok(Store {
            inner: Arc::new(StoreInner {
                path,
                repaired_fts,
                writer_tx: tx,
                readers,
                closed: AtomicBool::new(false),
                durable_commits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            }),
        })
    }

    /// Base en mémoire, pour les tests déterministes.
    pub fn open_memory() -> Result<Store> {
        // Une base fichier dans un répertoire temporaire : `:memory:` interdirait le
        // pool de lecture (chaque connexion aurait sa propre base). Le compteur garantit
        // l'unicité même si deux tests démarrent dans la même nanoseconde.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "penelope-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir)?;
        Store::open(dir.join("penelope.db"))
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Exécute une écriture dans une transaction. Le commit est automatique si la
    /// closure renvoie `Ok`, le rollback si elle renvoie `Err`.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(StoreError::WriterGone);
        }
        let (tx, rx) = oneshot::channel();
        let job: WriteJob = Box::new(move |conn| {
            let res = guarded(conn, f);
            let _ = tx.send(res);
        });
        self.inner
            .writer_tx
            .send(job)
            .map_err(|_| StoreError::WriterGone)?;
        rx.await.map_err(|_| StoreError::WriterGone)?
    }

    /// Écriture **durable contre la machine** (issue #75).
    ///
    /// En WAL, `synchronous=NORMAL` ne synchronise le journal qu'au checkpoint : un commit
    /// survit à `kill -9`, pas à une coupure de courant. Cette variante passe la connexion
    /// en `synchronous=FULL` (et `fullfsync=ON`, sans effet hors macOS, qui force le
    /// vidage jusqu'au média) le temps d'une transaction, puis rétablit `NORMAL`, y compris
    /// après une erreur ou une panique. Le fsync du WAL rend aussi durables les commits
    /// ordinaires qui la précèdent. Réservée aux transitions qu'on ne doit jamais perdre :
    /// un fsync par appel, jamais sur le trafic de fond.
    pub async fn write_durable<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(StoreError::WriterGone);
        }
        let (tx, rx) = oneshot::channel();
        let counter = self.inner.durable_commits.clone();
        let job: WriteJob = Box::new(move |conn| {
            let res = durable(conn, f);
            if res.is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            let _ = tx.send(res);
        });
        self.inner
            .writer_tx
            .send(job)
            .map_err(|_| StoreError::WriterGone)?;
        rx.await.map_err(|_| StoreError::WriterGone)?
    }

    /// Transactions validées par `write_durable` depuis l'ouverture de ce store.
    pub fn durable_commits(&self) -> u64 {
        self.inner.durable_commits.load(Ordering::SeqCst)
    }

    /// Variante synchrone (utilisée par le thread de démarrage et les outils CLI).
    pub fn write_blocking<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let job: WriteJob = Box::new(move |conn| {
            let _ = tx.send(guarded(conn, f));
        });
        self.inner
            .writer_tx
            .send(job)
            .map_err(|_| StoreError::WriterGone)?;
        rx.recv().map_err(|_| StoreError::WriterGone)?
    }

    /// Lecture concurrente sur le pool.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let readers = self.inner.readers.clone();
        tokio::task::spawn_blocking(move || readers.with(f))
            .await
            .map_err(|e| StoreError::other(format!("tâche de lecture annulée : {e}")))?
    }

    pub fn read_blocking<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        self.inner.readers.with(f)
    }

    /// `PRAGMA integrity_check` : utilisé par le mode `recovery` (§17).
    pub fn integrity(&self) -> Result<String> {
        self.read_blocking(|c| check_lines(c, "PRAGMA integrity_check;"))
    }

    /// Les index FTS5 reconstruits à l'ouverture, s'il y en a eu (issue #158).
    pub fn repaired_fts(&self) -> &[String] {
        &self.inner.repaired_fts
    }

    /// `PRAGMA integrity_check`, **et ce qu'en dit une connexion neuve** si le pool
    /// n'est pas d'accord (issue #158).
    ///
    /// Le 21/09, un lecteur ouvert depuis trois heures a rendu « malformed inverted index »
    /// sur une base que sept connexions neuves relisaient intègre, en nommant une table
    /// différente d'un appel à l'autre. C'est le fichier qui fait foi, pas une connexion :
    /// comme `backup_to` (#77), on en ouvre une pour l'occasion. La connexion qui a menti
    /// est fermée et remplacée.
    pub fn integrity_report(&self) -> Result<IntegrityReport> {
        let (pool, reader) = self.inner.readers.with_audit(|c| {
            let v = check_lines(c, "PRAGMA integrity_check;")?;
            // Une connexion qui accuse la base ne revient dans le pool que si le fichier
            // lui donne raison ; le verdict du fichier est demandé juste après.
            let keep = v == "ok";
            Ok((v, keep))
        })?;
        let fresh = if pool == "ok" {
            None
        } else {
            let c = open_connection(&self.inner.path, true)?;
            Some(check_lines(&c, "PRAGMA integrity_check;")?)
        };
        Ok(IntegrityReport {
            pool,
            fresh,
            reader,
        })
    }

    /// Copie cohérente de la base (sauvegarde §15 `penelope backup`), **sans occuper
    /// l'écrivain** (issue #77) : `VACUUM INTO` sur une connexion en lecture seule ouverte
    /// pour l'occasion lit l'état commité (WAL compris) pendant que les écritures
    /// continuent. Bloquant : depuis un runtime async, passer par `snapshot_to`.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<()> {
        let dest = dest.as_ref().to_path_buf();
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        if dest.exists() {
            std::fs::remove_file(&dest)?;
        }
        let conn = open_connection(&self.inner.path, true)?;
        let dest_s = dest.to_string_lossy().replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{dest_s}';"))?;
        Ok(())
    }

    /// `backup_to` sur un thread bloquant, hors du runtime async (#77). Renvoie la durée
    /// de l'instantané.
    pub async fn snapshot_to(&self, dest: PathBuf) -> Result<std::time::Duration> {
        let s = self.clone();
        tokio::task::spawn_blocking(move || {
            let t = std::time::Instant::now();
            s.backup_to(&dest)?;
            Ok(t.elapsed())
        })
        .await
        .map_err(|e| StoreError::other(format!("instantané interrompu : {e}")))?
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
    }
}

/// Transaction sous `synchronous=FULL`, puis retour à `NORMAL` quoi qu'il arrive :
/// `guarded` rattrape une panique, donc le rétablissement s'exécute toujours.
fn durable<T, F>(conn: &mut Connection, f: F) -> Result<T>
where
    F: FnOnce(&Transaction<'_>) -> Result<T>,
{
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON;")?;
    let res = guarded(conn, f);
    if let Err(e) = conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA fullfsync=OFF;") {
        tracing::error!(error = %e, "retour à synchronous=NORMAL impossible");
    }
    res
}

fn run_in_transaction<T, F>(conn: &mut Connection, f: F) -> Result<T>
where
    F: FnOnce(&Transaction<'_>) -> Result<T>,
{
    let tx = conn.transaction()?;
    match f(&tx) {
        Ok(v) => {
            tx.commit()?;
            Ok(v)
        }
        Err(e) => {
            // Le rollback est implicite au drop, mais on le rend explicite.
            let _ = tx.rollback();
            Err(e)
        }
    }
}

pub(crate) fn open_connection(path: &Path, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;
         PRAGMA mmap_size=268435456;
         PRAGMA cache_size=-32000;",
    )?;
    Ok(conn)
}

/// Toutes les lignes d'un `PRAGMA …_check`, pas seulement la première.
///
/// `query_row` n'en lisait qu'une (issue #158). Avec plusieurs griefs, une vraie
/// corruption de données pouvait se cacher derrière une ligne d'index FTS5 — et c'est
/// précisément sur cette distinction que tout le reste repose.
fn check_lines(conn: &Connection, pragma: &str) -> Result<String> {
    let mut st = conn.prepare(pragma)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let lines: Vec<String> = rows.collect::<std::result::Result<_, _>>()?;
    Ok(lines.join("\n"))
}

/// Les tables FTS5 nommées par un verdict qui ne parle **que** d'index dérivés.
///
/// `malformed inverted index for FTS5 table main.messages_fts` désigne un index
/// reconstructible, pas une donnée perdue : aucune donnée première ne vit dans un index
/// FTS — `messages_fts` se refait de `messages`, `mem_fts` du vault. Une seule ligne qui
/// parle d'autre chose, et ce n'est plus vrai : on ne touche alors à rien.
pub fn fts_tables(verdict: &str) -> Option<Vec<String>> {
    const MARK: &str = "malformed inverted index for FTS5 table ";
    let mut out = Vec::new();
    for line in verdict.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let name = line.strip_prefix(MARK)?.trim();
        // Le nom vient de SQLite, mais il finit dans une requête : rien d'autre qu'un
        // identifiant, éventuellement qualifié par son schéma.
        let bare = name.rsplit('.').next().unwrap_or(name);
        if bare.is_empty() || !bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        if !out.contains(&bare.to_string()) {
            out.push(bare.to_string());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Refuse d'ouvrir une base abîmée — **sauf** si seuls des index dérivés le sont, auquel
/// cas ils sont reconstruits et la base rouverte normalement (issue #158).
///
/// Depuis SQLite 3.44, `quick_check` vérifie aussi les index FTS5. Refuser de démarrer
/// pour eux revenait à mettre le daemon en panne pour quelque chose qui se rebâtit en une
/// commande — et, après cinq démarrages ratés, `upgrade` serait revenu au binaire
/// précédent (#153). Rend les tables reconstruites, pour que l'appelant le dise.
fn ensure_sound(conn: &Connection) -> Result<Vec<String>> {
    let tables = match check_lines(conn, "PRAGMA quick_check;") {
        Ok(v) if v == "ok" => return Ok(Vec::new()),
        Ok(v) => fts_tables(&v).ok_or(StoreError::Corrupt(v))?,
        // Un index si abîmé que SQLite ne construit même plus sa table virtuelle : le
        // contrôle n'a pas de verdict à rendre, il échoue. C'est toujours un index
        // dérivé, donc toujours réparable — à condition que ce soit bien de lui qu'on
        // parle, ce que la déclaration de la table confirme.
        Err(e) => match unconstructible_fts(conn, &e) {
            Some(t) => vec![t],
            None => return Err(e),
        },
    };
    for t in &tables {
        repair_fts(conn, t)?;
    }
    let after = check_lines(conn, "PRAGMA quick_check;")?;
    if after != "ok" {
        return Err(StoreError::Corrupt(after));
    }
    Ok(tables)
}

/// La table FTS5 nommée par une erreur « vtable constructor failed », si c'en est bien une.
///
/// Le message vient de SQLite, mais on ne se fie pas qu'à lui : la table doit exister dans
/// le schéma **et** être déclarée `USING fts5`. Sinon on laisse l'erreur remonter.
fn unconstructible_fts(conn: &Connection, err: &StoreError) -> Option<String> {
    const MARK: &str = "vtable constructor failed: ";
    let text = err.to_string();
    let name = text.split(MARK).nth(1)?.trim();
    let name = name.split_whitespace().next()?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .ok()?;
    ddl.to_lowercase()
        .contains("using fts5")
        .then(|| name.to_string())
}

/// Remet un index FTS5 d'aplomb.
///
/// `INSERT INTO t(t) VALUES('rebuild')` suffit quand l'index est lisible. Quand il ne
/// l'est plus, SQLite refuse jusqu'à **construire** la table virtuelle
/// (« vtable constructor failed ») : il n'y a plus rien à reconstruire depuis elle. La
/// table est alors recréée vide à partir de sa propre déclaration, et son contenu revient
/// de la source de vérité — `messages` pour `messages_fts`, le vault pour `mem_fts` —,
/// ce que font `penelope store rebuild` et `penelope mem reindex`.
fn repair_fts(conn: &Connection, table: &str) -> Result<()> {
    if conn
        .execute_batch(&format!("INSERT INTO {table}({table}) VALUES('rebuild');"))
        .is_ok()
    {
        return Ok(());
    }
    let ddl: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    // `DROP TABLE` échoue pour la même raison que `rebuild` : il construit la table
    // virtuelle avant de la détruire. On retire donc sa déclaration du schéma, ce qui
    // laisse ses tables d'ombre (`%_data`, `%_content`, …) en tables ordinaires, puis on
    // les supprime et on recrée la table vide. Aucune donnée première n'y vit.
    let shadows: Vec<String> = {
        let mut st = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE ?1 ESCAPE '\\'",
        )?;
        let like = format!("{}\\_%", table.replace('_', "\\_"));
        let rows = st.query_map([like], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut sql = String::from("PRAGMA writable_schema = ON;\n");
    sql.push_str(&format!(
        "DELETE FROM sqlite_master WHERE type = 'table' AND name = '{table}';\n"
    ));
    // `RESET` relit le schéma : sans lui, la connexion croit encore à la table virtuelle.
    sql.push_str("PRAGMA writable_schema = RESET;\n");
    for sh in &shadows {
        sql.push_str(&format!("DROP TABLE IF EXISTS {sh};\n"));
    }
    sql.push_str(&format!("{ddl};\n"));
    conn.execute_batch(&sql)?;
    Ok(())
}

/// Aide : lit une valeur du magasin clé/valeur générique.
pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut st = conn.prepare_cached("SELECT v FROM kv WHERE k = ?1")?;
    let mut rows = st.query([key])?;
    match rows.next()? {
        Some(r) => Ok(Some(r.get(0)?)),
        None => Ok(None),
    }
}

/// Aide : écrit une valeur du magasin clé/valeur générique.
pub fn kv_set(tx: &Transaction<'_>, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO kv(k, v, ts) VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_migrate_read_write() {
        let s = Store::open_memory().unwrap();
        assert_eq!(s.integrity().unwrap(), "ok");

        s.write(|tx| {
            kv_set(tx, "hello", "world")?;
            Ok(())
        })
        .await
        .unwrap();

        let v = s.read(|c| kv_get(c, "hello")).await.unwrap();
        assert_eq!(v.as_deref(), Some("world"));
    }

    #[tokio::test]
    async fn write_rolls_back_on_error() {
        let s = Store::open_memory().unwrap();
        let r: Result<()> = s
            .write(|tx| {
                kv_set(tx, "k", "v")?;
                Err(StoreError::other("boom"))
            })
            .await;
        assert!(r.is_err());
        let v = s.read(|c| kv_get(c, "k")).await.unwrap();
        assert!(v.is_none(), "le rollback doit annuler l'écriture");
    }

    #[tokio::test]
    async fn concurrent_writes_are_serialised() {
        let s = Store::open_memory().unwrap();
        let mut handles = Vec::new();
        for i in 0..32 {
            let s2 = s.clone();
            handles.push(tokio::spawn(async move {
                s2.write(move |tx| {
                    tx.execute(
                        "INSERT INTO kv(k,v) VALUES(?1,?2)",
                        rusqlite::params![format!("k{i}"), i.to_string()],
                    )?;
                    Ok(())
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let n: i64 = s
            .read(|c| Ok(c.query_row("SELECT count(*) FROM kv", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 32);
    }

    /// #44 : une panique dans une closure d'écriture ne tue plus l'écrivain. La
    /// transaction est annulée, le demandeur reçoit la panique nommée, et l'écriture
    /// suivante passe.
    /// Le compteur de paniques de l'écrivain est global : les tests qui en provoquent
    /// passent l'un après l'autre, sinon l'un voit la panique de l'autre.
    static WRITER_PANIC_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_panicking_write_does_not_kill_the_writer() {
        let _one_at_a_time = WRITER_PANIC_TESTS.lock().await;
        let s = Store::open_memory().unwrap();
        s.write(|tx| kv_set(tx, "avant", "1")).await.unwrap();
        let avant = writer_panics();

        // La panique est attendue : pas de trace sur stderr pendant le test.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let e = s
            .write(|tx| {
                kv_set(tx, "jamais", "2")?;
                let v: Vec<u8> = Vec::new();
                let _ = v[3];
                Ok(())
            })
            .await
            .unwrap_err();
        std::panic::set_hook(hook);

        let msg = e.to_string();
        assert!(matches!(e, StoreError::WriterPanic(_)), "{msg}");
        assert!(msg.contains("index out of bounds"), "{msg}");
        assert_eq!(writer_panics(), avant + 1);

        // L'écrivain est vivant, et le travail qui a paniqué n'a rien commité.
        s.write(|tx| kv_set(tx, "apres", "3")).await.unwrap();
        let (avant_v, apres_v, jamais): (String, String, i64) = s
            .read(|c| {
                Ok((
                    c.query_row("SELECT v FROM kv WHERE k='avant'", [], |r| r.get(0))?,
                    c.query_row("SELECT v FROM kv WHERE k='apres'", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM kv WHERE k='jamais'", [], |r| r.get(0))?,
                ))
            })
            .await
            .unwrap();
        assert_eq!((avant_v.as_str(), apres_v.as_str(), jamais), ("1", "3", 0));
    }

    fn synchronous(c: &Connection) -> i64 {
        c.query_row("PRAGMA synchronous;", [], |r| r.get(0))
            .unwrap()
    }

    /// #75 : une écriture durable s'exécute sous `synchronous=FULL` (2) et la connexion
    /// revient à `NORMAL` (1) ensuite, après un succès, une erreur ou une panique.
    #[tokio::test]
    async fn a_durable_write_runs_under_full_sync_and_restores_normal() {
        let _one_at_a_time = WRITER_PANIC_TESTS.lock().await;
        let s = Store::open_memory().unwrap();
        let dedans = s
            .write_durable(|tx| {
                kv_set(tx, "effet", "dispatching")?;
                Ok(synchronous(tx))
            })
            .await
            .unwrap();
        assert_eq!(dedans, 2, "FULL pendant la transaction durable");
        assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);
        assert_eq!(s.durable_commits(), 1);

        // Erreur : rollback, pas de commit compté, NORMAL rétabli.
        let e = s
            .write_durable(|tx| -> Result<()> {
                kv_set(tx, "perdu", "x")?;
                Err(StoreError::other("refus"))
            })
            .await;
        assert!(e.is_err());
        assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);

        // Panique : rattrapée, NORMAL rétabli, l'écrivain continue.
        let e = s
            .write_durable(|_tx| -> Result<()> {
                panic!("panique voulue dans une écriture durable")
            })
            .await
            .unwrap_err();
        assert!(matches!(e, StoreError::WriterPanic(_)), "{e}");
        assert_eq!(s.write(|tx| Ok(synchronous(tx))).await.unwrap(), 1);
        assert_eq!(s.durable_commits(), 1, "seuls les commits aboutis comptent");

        let perdu: i64 = s
            .read(|c| Ok(c.query_row("SELECT count(*) FROM kv WHERE k='perdu'", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(perdu, 0);
    }

    /// #77 : l'instantané ne gèle plus l'écrivain. Pendant la copie d'une base de
    /// quelques dizaines de Mo, des écritures partent et reviennent ; la copie passe
    /// `integrity_check`. Gelé, l'écrivain n'aboutirait à rien avant la fin de la copie :
    /// c'est `during > 0` qui le prouve. Le plafond de latence reste large, le disque
    /// partagé d'un runner de CI peut suspendre une écriture 200 ms sous la pression de
    /// la copie.
    #[test]
    fn writes_go_on_while_a_snapshot_is_taken() {
        let s = Store::open_memory().unwrap();
        s.write_blocking(|tx| {
            let blob = "x".repeat(4096);
            for i in 0..8000 {
                tx.execute(
                    "INSERT INTO kv(k, v) VALUES(?1, ?2)",
                    rusqlite::params![format!("gros{i}"), blob],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("instantane.db");

        let done = Arc::new(AtomicBool::new(false));
        let snap = {
            let (s, dest, done) = (s.clone(), dest.clone(), done.clone());
            std::thread::spawn(move || {
                s.backup_to(&dest).unwrap();
                done.store(true, Ordering::SeqCst);
            })
        };
        let mut during = 0;
        let mut slowest = std::time::Duration::ZERO;
        let mut i = 0;
        while !done.load(Ordering::SeqCst) {
            let t = std::time::Instant::now();
            let k = format!("pendant{i}");
            s.write_blocking(move |tx| kv_set(tx, &k, "v")).unwrap();
            slowest = slowest.max(t.elapsed());
            if !done.load(Ordering::SeqCst) {
                during += 1;
            }
            i += 1;
        }
        snap.join().unwrap();
        assert!(
            during > 0,
            "aucune écriture n'a abouti pendant l'instantané"
        );
        assert!(
            slowest < std::time::Duration::from_secs(1),
            "écriture la plus lente : {slowest:?}"
        );

        let c = Connection::open(&dest).unwrap();
        let ok: String = c
            .query_row("PRAGMA integrity_check;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        let n: i64 = c
            .query_row("SELECT count(*) FROM kv WHERE k LIKE 'gros%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 8000);
    }

    #[test]
    fn backup_produces_readable_copy() {
        let s = Store::open_memory().unwrap();
        s.write_blocking(|tx| kv_set(tx, "a", "b")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("backup.db");
        s.backup_to(&dest).unwrap();
        let c = Connection::open(&dest).unwrap();
        let v: String = c
            .query_row("SELECT v FROM kv WHERE k='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "b");
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;

    /// #158 : SQLite embarqué à jour. La 3.46.0 (mai 2024) portait un faux positif de
    /// l'`integrity-check` FTS5, corrigé en 3.46.1 ; c'est la source du verdict que le
    /// reste de ce lot apprend à ne pas croire sur parole.
    #[test]
    fn the_bundled_sqlite_is_recent_enough() {
        let v = rusqlite::version();
        let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
        assert!(parts.len() >= 2, "version illisible : {v}");
        let (major, minor) = (parts[0], parts[1]);
        assert!(
            (major, minor) >= (3, 50),
            "SQLite {v} : au moins 3.50 attendu (issue #158)"
        );
    }

    /// #158 : un verdict qui ne parle que d'index FTS5 désigne du **dérivé**, pas des
    /// données. Une seule ligne qui parle d'autre chose, et on ne touche plus à rien.
    #[test]
    fn only_a_verdict_about_derived_indexes_is_repairable() {
        let one = "malformed inverted index for FTS5 table main.messages_fts";
        assert_eq!(fts_tables(one), Some(vec!["messages_fts".to_string()]));

        // Les deux tables vues le 21/09, d'un appel à l'autre.
        let two = "malformed inverted index for FTS5 table main.mem_fts\n\
                   malformed inverted index for FTS5 table main.messages_fts";
        assert_eq!(
            fts_tables(two),
            Some(vec!["mem_fts".to_string(), "messages_fts".to_string()])
        );

        // Une vraie atteinte, seule ou mêlée à une ligne FTS : rien n'est reconstruit.
        assert_eq!(fts_tables("ok"), None);
        assert_eq!(
            fts_tables("row 12 missing from index messages_session"),
            None
        );
        let mixed = "malformed inverted index for FTS5 table main.mem_fts\n\
                     row 12 missing from index messages_session";
        assert_eq!(
            fts_tables(mixed),
            None,
            "une corruption de données cachée derrière une ligne d'index ne passe pas"
        );
        // Un nom qui n'est pas un identifiant n'entre pas dans une requête.
        assert_eq!(
            fts_tables("malformed inverted index for FTS5 table main.x\"; DROP TABLE y--"),
            None
        );
    }

    /// #158 : `quick_check` rend **plusieurs** lignes. N'en lire qu'une cachait une
    /// corruption de données derrière une ligne d'index.
    #[test]
    fn every_line_of_a_verdict_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let c = open_connection(&path, false).unwrap();
        assert_eq!(check_lines(&c, "PRAGMA quick_check;").unwrap(), "ok");
        assert_eq!(check_lines(&c, "PRAGMA integrity_check;").unwrap(), "ok");
    }

    /// #158 : le fichier fait foi, pas une connexion. Sur une base saine, les deux
    /// verdicts concordent et le rapport le dit sans ouvrir de connexion neuve.
    #[test]
    fn a_sound_database_needs_no_second_opinion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("t.db")).unwrap();
        let r = store.integrity_report().unwrap();
        assert_eq!(r.pool, "ok");
        assert!(
            r.fresh.is_none(),
            "connexion neuve inutile quand le pool dit ok"
        );
        assert!(r.sound());
        assert!(!r.reader_lied());
        assert_eq!(r.fts_only(), None);
        assert!(store.repaired_fts().is_empty());
    }

    /// #158 : un lecteur qui accuse une base que le fichier dément ne ment qu'une fois —
    /// il est fermé, et le pool en rouvre un à sa place.
    #[test]
    fn a_lying_reader_is_recognised_and_replaced() {
        let r = IntegrityReport {
            pool: "malformed inverted index for FTS5 table main.mem_fts".into(),
            fresh: Some("ok".into()),
            reader: crate::pool::ReaderStats {
                age_s: 11_000,
                served: 4_200,
            },
        };
        assert!(r.reader_lied(), "le fichier dément le lecteur");
        assert!(r.sound(), "c'est le fichier qui fait foi");
        assert_eq!(r.verdict(), "ok");
        assert_eq!(r.fts_only(), None, "rien à reconstruire : rien n'est cassé");
    }

    /// #158 : quand les deux sont d'accord sur un index dérivé, il y a bien quelque chose
    /// à reconstruire — et toujours rien à restaurer.
    #[test]
    fn a_confirmed_index_fault_names_the_table() {
        let bad = "malformed inverted index for FTS5 table main.messages_fts";
        let r = IntegrityReport {
            pool: bad.into(),
            fresh: Some(bad.into()),
            reader: crate::pool::ReaderStats {
                age_s: 10,
                served: 1,
            },
        };
        assert!(!r.sound());
        assert!(!r.reader_lied());
        assert_eq!(r.fts_only(), Some(vec!["messages_fts".to_string()]));

        // Une atteinte aux données, elle, n'est pas reconstructible.
        let hard = IntegrityReport {
            pool: "row 12 missing from index messages_session".into(),
            fresh: Some("row 12 missing from index messages_session".into()),
            reader: crate::pool::ReaderStats {
                age_s: 10,
                served: 1,
            },
        };
        assert_eq!(hard.fts_only(), None);
    }

    /// #158 : une base dont **seul** un index FTS5 est abîmé s'ouvre, se répare et le
    /// dit. Refuser de démarrer pour ça aurait déclenché le retour arrière de #153.
    #[test]
    fn a_broken_search_index_does_not_stop_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let store = Store::open(&path).unwrap();
            assert!(store.repaired_fts().is_empty());
        }
        // L'index inversé de `messages_fts` est vidé sous les pieds de SQLite : la table
        // `%_data` porte l'index, `%_content` le texte. C'est exactement la panne décrite.
        {
            let c = open_connection(&path, false).unwrap();
            c.execute_batch(
                "INSERT INTO messages_fts(content, session_id, msg_id) VALUES('bonjour', 's1', 1);
                 DELETE FROM messages_fts_data WHERE id > 1;",
            )
            .unwrap();
            let verdict = check_lines(&c, "PRAGMA quick_check;").unwrap();
            assert_ne!(verdict, "ok", "l'index est bien abîmé : {verdict}");
            assert!(fts_tables(&verdict).is_some(), "{verdict}");
        }
        // Et pourtant la base s'ouvre, parce que l'index se reconstruit.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.repaired_fts(), ["messages_fts"]);
        assert_eq!(store.integrity().unwrap(), "ok");
    }
}
