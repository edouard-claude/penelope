//! Post-traitement local d'un vocal (issue #299) : restauration par un moteur, puis finition
//! par une chaîne de filtres FFmpeg, entre le WAV assemblé et l'encodage OGG/Opus.
//!
//! ```text
//!  WAV assemblé ─► moteur (Resemble Enhance, CPU) ─► ffmpeg -af <preset studio doux> ─► WAV fini
//!  absent, erreur, délai dépassé ─► avertissement journalisé, le WAV brut part (`Status`)
//! ```
//!
//! Tout est local : aucun poids n'est téléchargé ici. `doctor` vérifie binaires et poids ;
//! un moteur dont il manque quelque chose est écarté avant de lancer le moindre processus.
//! Le moteur est derrière [`Enhancer`] : un autre s'ajoute sans toucher à la chaîne.

use async_trait::async_trait;
use penelope_kernel::config::{VOICE_ENGINE_RESEMBLE, VoicePostprocess, parse_duration};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// Issue du post-traitement, portée par `voice.sent` et par le résultat de l'outil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// `voice.postprocess.enabled` est faux : rien n'a été tenté.
    Disabled,
    /// Chaîne complète appliquée.
    Applied,
    /// Rien lancé : un binaire ou les poids manquent.
    Skipped,
    /// Lancé, en erreur : le vocal brut est parti.
    Failed,
    /// Délai `voice.postprocess.timeout` dépassé : processus arrêté, vocal brut parti.
    TimedOut,
}

/// Ce qui s'est passé, et en combien de temps.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub status: Status,
    pub engine: String,
    /// Durée réelle du post-traitement, en secondes (au dixième).
    pub seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Outcome {
    pub fn disabled(engine: &str) -> Outcome {
        Outcome {
            status: Status::Disabled,
            engine: engine.into(),
            seconds: 0.0,
            reason: None,
        }
    }

    /// Rien lancé : `reason` dit ce qui manque.
    pub fn skipped(engine: &str, reason: String) -> Outcome {
        Outcome {
            status: Status::Skipped,
            engine: engine.into(),
            seconds: 0.0,
            reason: Some(reason),
        }
    }

    fn at(status: Status, engine: &str, started: Instant, reason: Option<String>) -> Outcome {
        Outcome {
            status,
            engine: engine.into(),
            seconds: (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            reason,
        }
    }
}

/// Un moteur de restauration : un WAV entre, un WAV sort. Sans réseau.
#[async_trait]
pub trait Enhancer: Send + Sync {
    /// Prêt à tourner hors ligne ? `Ok` dit ce qui a été trouvé ; `Err` ce qui manque et
    /// comment l'installer.
    fn readiness(&self) -> Result<String, String>;
    /// Restaure `input` dans `output` ; `work` est un répertoire propre à ce vocal, que
    /// l'appelant efface ensuite.
    async fn enhance(&self, input: &Path, output: &Path, work: &Path) -> Result<(), String>;
}

/// `~/` développé ; le reste tel quel.
fn expand_home(raw: &str) -> PathBuf {
    match (raw.strip_prefix("~/"), penelope_platform::dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(raw),
    }
}

/// Binaire désigné par la configuration, sinon `default` dans le PATH étendu (Homebrew,
/// `~/.local/bin`…).
fn resolve_bin(configured: &str, default: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return penelope_platform::which(default);
    }
    let expanded = expand_home(configured);
    penelope_platform::which(&expanded.to_string_lossy())
}

/// `ffmpeg` : `voice.postprocess.ffmpeg_bin`, sinon le PATH étendu.
pub fn ffmpeg_path(cfg: &VoicePostprocess) -> Option<PathBuf> {
    resolve_bin(&cfg.ffmpeg_bin, "ffmpeg")
}

/// Débit Opus de l'encodage final : celui du preset quand la chaîne a abouti, celui
/// d'origine (32k) pour un vocal brut, qu'il soit éteint, écarté, en échec ou hors délai.
pub fn opus_bitrate(cfg: &VoicePostprocess, outcome: &Outcome) -> String {
    let configured = cfg.opus_bitrate.trim();
    if outcome.status == Status::Applied && !configured.is_empty() {
        configured.to_string()
    } else {
        penelope_platform::audio::OPUS_BITRATE.to_string()
    }
}

/// Les derniers caractères d'une sortie d'erreur, sur une ligne.
fn tail(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let s = s.trim();
    let start = s.char_indices().rev().nth(400).map(|(i, _)| i).unwrap_or(0);
    s[start..].replace('\n', " | ")
}

// ------------------------------------------------------------------ Resemble Enhance

/// Resemble Enhance (`resemble-enhance <in_dir> <out_dir>`), poids dans `run_dir`.
pub struct ResembleEnhance {
    bin: Option<PathBuf>,
    run_dir: PathBuf,
    device: String,
}

/// Fichiers attendus dans `enhancer_stage2` : hyperparamètres, pointeur d'étape, poids.
const RESEMBLE_WEIGHT_FILES: [&str; 3] = [
    "hparams.yaml",
    "ds/G/latest",
    "ds/G/default/mp_rank_00_model_states.pt",
];

impl ResembleEnhance {
    pub fn from_config(cfg: &VoicePostprocess, data_dir: &Path) -> ResembleEnhance {
        let run_dir = if cfg.resemble_run_dir.trim().is_empty() {
            data_dir
                .join("models")
                .join("resemble-enhance")
                .join("enhancer_stage2")
        } else {
            expand_home(cfg.resemble_run_dir.trim())
        };
        ResembleEnhance {
            bin: resolve_bin(&cfg.resemble_bin, "resemble-enhance"),
            run_dir,
            device: cfg.resemble_device.trim().to_string(),
        }
    }

    /// Ce qui manque dans `run_dir`, ou rien. Un pointeur Git LFS à la place des poids est
    /// nommé : c'est l'oubli classique d'un `git clone` sans `git lfs install`.
    pub fn weights_missing(run_dir: &Path) -> Option<String> {
        for rel in RESEMBLE_WEIGHT_FILES {
            let path = run_dir.join(rel);
            if !path.is_file() {
                return Some(format!(
                    "poids de Resemble Enhance absents : `{}` manque",
                    path.display()
                ));
            }
        }
        // Seul l'en-tête est lu : les vrais poids pèsent des centaines de mégaoctets.
        let weights = run_dir.join(RESEMBLE_WEIGHT_FILES[2]);
        let mut head = [0u8; 64];
        let read = std::fs::File::open(&weights)
            .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
            .unwrap_or(0);
        if head[..read].starts_with(b"version https://git-lfs.github.com/spec") {
            return Some(format!(
                "`{}` est un pointeur Git LFS, pas les poids : `git lfs install` puis \
                 `git lfs pull` dans le dépôt des poids",
                weights.display()
            ));
        }
        None
    }
}

#[async_trait]
impl Enhancer for ResembleEnhance {
    fn readiness(&self) -> Result<String, String> {
        let bin = self.bin.as_ref().ok_or_else(|| {
            "`resemble-enhance` introuvable : `uv tool install --python 3.11 resemble-enhance` \
             (ou `voice.postprocess.resemble_bin`)"
                .to_string()
        })?;
        if let Some(missing) = Self::weights_missing(&self.run_dir) {
            return Err(missing);
        }
        Ok(format!(
            "`{}`, poids dans `{}`, calcul `{}`",
            bin.display(),
            self.run_dir.display(),
            self.device
        ))
    }

    async fn enhance(&self, input: &Path, output: &Path, work: &Path) -> Result<(), String> {
        let bin = self.bin.as_ref().ok_or("`resemble-enhance` introuvable")?;
        let (in_dir, out_dir) = (work.join("in"), work.join("out"));
        std::fs::create_dir_all(&in_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let name = input.file_name().ok_or("WAV sans nom")?;
        std::fs::copy(input, in_dir.join(name)).map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg(&in_dir)
            .arg(&out_dir)
            .args(["--device", &self.device])
            .arg("--run_dir")
            .arg(&self.run_dir)
            // Jamais de réseau pendant un envoi : les poids sont là ou le moteur est écarté.
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if self.device == "mps" {
            // `aten::_weight_norm_interface` n'est pas porté sur MPS : repli vers le CPU
            // pour ces opérations, au lieu d'un échec.
            cmd.env("PYTORCH_ENABLE_MPS_FALLBACK", "1");
        }
        let out = cmd
            .output()
            .await
            .map_err(|e| format!("resemble-enhance : {e}"))?;
        if !out.status.success() {
            return Err(format!("resemble-enhance : {}", tail(&out.stderr)));
        }
        let produced = out_dir.join(name);
        if !produced.is_file() {
            return Err("resemble-enhance n'a rendu aucun fichier".into());
        }
        std::fs::rename(&produced, output).map_err(|e| e.to_string())
    }
}

// ------------------------------------------------------------------ filtres FFmpeg

/// Finition : `ffmpeg -af <filtres> <options de sortie>` d'un WAV vers un autre.
pub struct FfmpegFilters {
    bin: PathBuf,
    filters: String,
    output_args: Vec<String>,
}

impl FfmpegFilters {
    pub async fn apply(&self, input: &Path, output: &Path) -> Result<(), String> {
        let out = tokio::process::Command::new(&self.bin)
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(input)
            .args(["-af", &self.filters])
            .args(&self.output_args)
            .arg(output)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("ffmpeg : {e}"))?;
        if !out.status.success() {
            return Err(format!("ffmpeg -af : {}", tail(&out.stderr)));
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ la chaîne

/// Moteur puis filtres, bornés par un seul délai.
pub struct Pipeline {
    engine: String,
    enhancer: Option<Box<dyn Enhancer>>,
    filters: Option<FfmpegFilters>,
    timeout: Duration,
}

impl Pipeline {
    /// Lit la configuration. `Err` : `ffmpeg` introuvable, et sans lui aucun vocal ne part.
    pub fn from_config(cfg: &VoicePostprocess, data_dir: &Path) -> Result<Pipeline, String> {
        let ffmpeg = ffmpeg_path(cfg).ok_or_else(|| {
            "`ffmpeg` introuvable : `brew install ffmpeg` (ou `voice.postprocess.ffmpeg_bin`)"
                .to_string()
        })?;
        let engine = cfg.engine.trim().to_string();
        let enhancer: Option<Box<dyn Enhancer>> = match engine.as_str() {
            VOICE_ENGINE_RESEMBLE => Some(Box::new(ResembleEnhance::from_config(cfg, data_dir))),
            // `none` (validé au chargement) : les filtres seuls.
            _ => None,
        };
        let filters = cfg.ffmpeg_filters.trim();
        let filters = (!filters.is_empty()).then(|| FfmpegFilters {
            bin: ffmpeg,
            filters: filters.to_string(),
            output_args: cfg
                .ffmpeg_output_args
                .split_whitespace()
                .map(String::from)
                .collect(),
        });
        Ok(Pipeline {
            engine,
            enhancer,
            filters,
            timeout: parse_duration(&cfg.timeout).unwrap_or(Duration::from_secs(180)),
        })
    }

    pub fn engine(&self) -> &str {
        &self.engine
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Ce qui manque pour tourner ; `Ok` décrit ce qui a été trouvé (`doctor`).
    pub fn readiness(&self) -> Result<String, String> {
        let engine = match &self.enhancer {
            Some(e) => e.readiness()?,
            None => "sans moteur".to_string(),
        };
        Ok(match &self.filters {
            Some(f) => format!(
                "{engine} ; filtres `{}`, sortie `{}`",
                f.filters,
                f.output_args.join(" ")
            ),
            None => format!("{engine} ; sans filtres"),
        })
    }

    /// Traite `wav` dans `work` (créé ici, effacé par l'appelant) ; rend le WAV fini, ou
    /// pourquoi le brut doit partir. Ne lance rien si quelque chose manque.
    pub async fn process(&self, wav: &Path, work: &Path) -> (Outcome, Option<PathBuf>) {
        let started = Instant::now();
        if let Err(reason) = self.readiness() {
            return (
                Outcome::at(Status::Skipped, &self.engine, started, Some(reason)),
                None,
            );
        }
        if let Err(e) = std::fs::create_dir_all(work) {
            return (
                Outcome::at(Status::Failed, &self.engine, started, Some(e.to_string())),
                None,
            );
        }
        match tokio::time::timeout(self.timeout, self.chain(wav, work)).await {
            Ok(Ok(path)) => (
                Outcome::at(Status::Applied, &self.engine, started, None),
                Some(path),
            ),
            Ok(Err(reason)) => (
                Outcome::at(Status::Failed, &self.engine, started, Some(reason)),
                None,
            ),
            Err(_) => (
                Outcome::at(
                    Status::TimedOut,
                    &self.engine,
                    started,
                    Some(format!(
                        "délai de {} s dépassé (`voice.postprocess.timeout`)",
                        self.timeout.as_secs()
                    )),
                ),
                None,
            ),
        }
    }

    async fn chain(&self, wav: &Path, work: &Path) -> Result<PathBuf, String> {
        let mut current = wav.to_path_buf();
        if let Some(e) = &self.enhancer {
            let out = work.join("enhanced.wav");
            e.enhance(&current, &out, work).await?;
            current = out;
        }
        if let Some(f) = &self.filters {
            let out = work.join("studio.wav");
            f.apply(&current, &out).await?;
            current = out;
        }
        Ok(current)
    }
}
