//! `penelope-observe` : logs JSON, redaction, détection d'injection, métriques, traces.
//!
//! §16 : logs `tracing` en JSON dans `{logs}`, rotation quotidienne, rétention
//! 14 jours, **niveaux par module modifiables à chaud**.

#![forbid(unsafe_code)]

pub mod injection;
pub mod metrics;
pub mod redact;
pub mod trajectory;

pub use injection::{InjectionFinding, Severity, is_suspicious, scan, wrap_untrusted};
pub use redact::{contains_secret, leaked_secret_kind, redact, redact_json, register_secret};

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

/// Poignée de rechargement du filtre de log (§16, niveaux à chaud).
#[derive(Clone)]
pub struct LogControl {
    handle: Arc<reload::Handle<EnvFilter, Registry>>,
}

impl LogControl {
    /// Change le filtre à chaud, par exemple `info,penelope_mcp=debug`.
    pub fn set_filter(&self, directives: &str) -> Result<(), String> {
        let f = EnvFilter::try_new(directives).map_err(|e| e.to_string())?;
        self.handle.reload(f).map_err(|e| e.to_string())
    }
}

static LOG_CONTROL: OnceLock<LogControl> = OnceLock::new();

pub fn log_control() -> Option<LogControl> {
    LOG_CONTROL.get().cloned()
}

/// Écrivain de fichier à rotation quotidienne, avec rétention.
///
/// Implémentation locale (pas de `tracing-appender`) : elle applique aussi la purge de
/// rétention, que le PRD exige, et évite une dépendance de plus.
struct DailyFile {
    dir: PathBuf,
    prefix: String,
    retention_days: u32,
    current: Mutex<Option<(String, std::fs::File)>>,
}

/// Répertoire des journaux en `0700`, fichiers existants en `0600` : un journal peut
/// contenir des extraits de conversation (issue #26).
pub fn restrict_log_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        for e in std::fs::read_dir(dir)?.flatten() {
            if e.path().is_file() {
                let _ = std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

impl DailyFile {
    fn new(dir: &Path, prefix: &str, retention_days: u32) -> std::io::Result<Self> {
        restrict_log_dir(dir)?;
        Ok(DailyFile {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            retention_days,
            current: Mutex::new(None),
        })
    }

    fn today() -> String {
        chrono::Utc::now().format("%Y-%m-%d").to_string()
    }

    fn write_line(&self, buf: &[u8]) -> std::io::Result<()> {
        let day = Self::today();
        let mut guard = match self.current.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let needs_new = match &*guard {
            Some((d, _)) => d != &day,
            None => true,
        };
        if needs_new {
            let path = self.dir.join(format!("{}-{day}.jsonl", self.prefix));
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let f = options.open(path)?;
            *guard = Some((day.clone(), f));
            drop(guard);
            self.purge_old();
            guard = match self.current.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
        }
        if let Some((_, f)) = guard.as_mut() {
            f.write_all(buf)?;
        }
        Ok(())
    }

    fn purge_old(&self) {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(self.retention_days as i64);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(&self.prefix) || !name.ends_with(".jsonl") {
                continue;
            }
            let Some(day) = name
                .strip_prefix(&format!("{}-", self.prefix))
                .and_then(|s| s.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if let Ok(d) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                && d < cutoff.date_naive()
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Writer `tracing` qui applique la redaction **avant** l'écriture sur disque.
#[derive(Clone)]
struct RedactingWriter(Arc<DailyFile>);

impl Write for RedactingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = buf.len();
        let s = String::from_utf8_lossy(buf);
        let cleaned = redact::redact(&s);
        self.0.write_line(cleaned.as_bytes())?;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RedactingWriter {
    type Writer = RedactingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Writer `tracing` vers un flux (stderr), rédigé lui aussi : sous le service, stderr
/// devient `daemon.err.log` (issue #26).
#[derive(Clone)]
pub struct RedactingStream<W: Fn() -> Box<dyn Write> + Clone>(pub W);

/// Un écrit en cours : le texte formaté est rédigé en bloc à la fin.
pub struct Redacted {
    sink: Box<dyn Write>,
    buf: Vec<u8>,
}

impl Write for Redacted {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let cleaned = redact::redact(&String::from_utf8_lossy(&self.buf));
        self.buf.clear();
        self.sink.write_all(cleaned.as_bytes())?;
        self.sink.flush()
    }
}

impl Drop for Redacted {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<'a, W: Fn() -> Box<dyn Write> + Clone + 'a> tracing_subscriber::fmt::MakeWriter<'a>
    for RedactingStream<W>
{
    type Writer = Redacted;
    fn make_writer(&'a self) -> Self::Writer {
        Redacted {
            sink: (self.0)(),
            buf: Vec::new(),
        }
    }
}

fn stderr_sink() -> Box<dyn Write> {
    Box::new(std::io::stderr())
}

/// Initialise `tracing` : JSON sur fichier à rotation, texte lisible sur stderr.
///
/// Appelable une seule fois par processus ; les appels suivants sont ignorés.
pub fn init(log_dir: &Path, level: &str, retention_days: u32, to_stderr: bool) -> LogControl {
    if let Some(c) = LOG_CONTROL.get() {
        return c.clone();
    }
    metrics::register_default_metrics();

    let filter = EnvFilter::try_from_env("PENELOPE_LOG")
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(filter);

    let file = DailyFile::new(log_dir, "penelope", retention_days)
        .map(Arc::new)
        .ok();

    let json_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_writer(RedactingWriter(f))
    });

    let stderr_layer = to_stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_writer(RedactingStream(stderr_sink as fn() -> Box<dyn Write>))
            .compact()
    });

    let _ = Registry::default()
        .with(filter)
        .with(json_layer)
        .with(stderr_layer)
        .try_init();

    let c = LogControl {
        handle: Arc::new(handle),
    };
    let _ = LOG_CONTROL.set(c.clone());
    c
}

/// Initialisation minimale pour les tests et la CLI hors daemon.
/// Abonné JSON en mémoire, réglé comme celui des fichiers journaliers (champs du span
/// courant sur chaque ligne) : de quoi vérifier dans un test ce qu'un tour écrit.
pub fn capture_json() -> (tracing::Dispatch, Arc<Mutex<Vec<u8>>>) {
    #[derive(Clone)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut g) = self.0.lock() {
                g.extend_from_slice(b);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = Shared(buf.clone());
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(move || sink.clone());
    let subscriber = Registry::default().with(EnvFilter::new("info")).with(layer);
    (tracing::Dispatch::new(subscriber), buf)
}

pub fn init_test() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("PENELOPE_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_file_writes_and_rotates_name() {
        let dir = tempfile::tempdir().unwrap();
        let f = DailyFile::new(dir.path(), "penelope", 14).unwrap();
        f.write_line(b"{\"msg\":\"a\"}\n").unwrap();
        f.write_line(b"{\"msg\":\"b\"}\n").unwrap();
        let today = DailyFile::today();
        let content =
            std::fs::read_to_string(dir.path().join(format!("penelope-{today}.jsonl"))).unwrap();
        assert!(content.contains("\"a\"") && content.contains("\"b\""));
    }

    #[test]
    fn retention_deletes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();
        std::fs::write(dir.path().join(format!("penelope-{old}.jsonl")), b"x").unwrap();
        let f = DailyFile::new(dir.path(), "penelope", 14).unwrap();
        f.write_line(b"nouveau\n").unwrap();
        assert!(
            !dir.path().join(format!("penelope-{old}.jsonl")).exists(),
            "un log de 30 jours doit être purgé avec une rétention de 14 jours"
        );
    }

    /// Issue #26 : une erreur de transport dont l'URL porte un secret enregistré sort de la
    /// couche stderr sans le secret.
    #[test]
    fn stderr_is_redacted_too() {
        use tracing_subscriber::layer::SubscriberExt;
        let token = "7123456789:AAHtest_secret_value_for_the_bot_0123";
        redact::register_secret(token);
        let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let c = captured.clone();
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(RedactingStream(move || {
                Box::new(Sink(c.clone())) as Box<dyn Write>
            }))
            .compact();
        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                error = %format!("error sending request for url (https://api.telegram.org/bot{token}/getUpdates)"),
                "getUpdates en échec"
            );
        });
        let out = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(out.contains("getUpdates en échec"), "{out}");
        assert!(!out.contains("AAHtest_secret_value"), "{out}");
        // Même sans enregistrement, la forme d'URL de la Bot API est reconnue.
        assert!(
            !redact::redact(
                "https://api.telegram.org/bot987654321:ZZZ_unregistered_token_abcdefghijklmn/x"
            )
            .contains("ZZZ_unregistered")
        );
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("daemon.err.log"), b"ancien").unwrap();
        std::fs::set_permissions(
            logs.join("daemon.err.log"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let f = DailyFile::new(&logs, "penelope", 14).unwrap();
        f.write_line(b"x\n").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&logs), 0o700);
        assert_eq!(mode(&logs.join("daemon.err.log")), 0o600);
        assert_eq!(
            mode(&logs.join(format!("penelope-{}.jsonl", DailyFile::today()))),
            0o600
        );
    }

    #[test]
    fn writer_redacts_before_disk() {
        let dir = tempfile::tempdir().unwrap();
        let f = Arc::new(DailyFile::new(dir.path(), "penelope", 14).unwrap());
        let mut w = RedactingWriter(f);
        w.write_all(b"{\"token\":\"Bearer abcdefghijklmnop1234\"}\n")
            .unwrap();
        let today = DailyFile::today();
        let content =
            std::fs::read_to_string(dir.path().join(format!("penelope-{today}.jsonl"))).unwrap();
        assert!(!content.contains("abcdefghijklmnop"), "{content}");
    }
}
