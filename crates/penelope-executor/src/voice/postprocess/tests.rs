//! Tests du post-traitement (#299) : de faux `resemble-enhance` et `ffmpeg` dans un
//! répertoire temporaire, jamais les vrais ; les poids sont des fichiers factices.

use super::*;
use penelope_kernel::config::{Config, VOICE_STUDIO_SOFT_FILTERS};

#[test]
fn the_section_is_off_by_default_read_from_toml_and_validated() {
    let p = Config::default().voice.postprocess;
    assert!(!p.enabled);
    assert_eq!(p.engine, VOICE_ENGINE_RESEMBLE);
    assert_eq!(p.timeout, "180s");
    assert_eq!(p.ffmpeg_filters, VOICE_STUDIO_SOFT_FILTERS);
    assert!(
        p.ffmpeg_filters.starts_with("highpass=f=70,")
            && p.ffmpeg_filters.contains("loudnorm=I=-16")
    );
    assert_eq!(p.ffmpeg_output_args, "-ac 1 -ar 24000");
    assert_eq!(p.opus_bitrate, "48k");
    assert_eq!(p.resemble_device, "cpu");
    assert!(p.ffmpeg_bin.is_empty() && p.resemble_bin.is_empty() && p.resemble_run_dir.is_empty());

    let cfg = Config::from_toml(
        "[voice.postprocess]\nenabled = true\nengine = \"none\"\ntimeout = \"5s\"\n\
         resemble_run_dir = \"~/poids\"\n",
    )
    .unwrap();
    assert!(cfg.voice.postprocess.enabled);
    assert_eq!(cfg.voice.postprocess.engine, "none");
    assert_eq!(cfg.voice.postprocess.timeout, "5s");
    assert_eq!(cfg.voice.postprocess.resemble_run_dir, "~/poids");
    assert!(Config::from_toml("[voice.postprocess]\nenginee = \"none\"\n").is_err());

    let mut cfg = Config::sample(42);
    cfg.voice.postprocess.engine = "sox".into();
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("voice.postprocess.engine"), "{err}");
    cfg.voice.postprocess.engine = "none".into();
    cfg.voice.postprocess.timeout = "0s".into();
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("voice.postprocess.timeout"), "{err}");
    cfg.voice.postprocess.timeout = "nope".into();
    assert!(cfg.validate().is_err());
    cfg.voice.postprocess.timeout = "5s".into();
    cfg.voice.postprocess.resemble_device = "cuda".into();
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("voice.postprocess.resemble_device"), "{err}");
    cfg.voice.postprocess.resemble_device = "mps".into();
    cfg.validate().unwrap();
}

#[test]
fn an_lfs_pointer_or_a_missing_file_is_named_as_missing_weights() {
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("enhancer_stage2");
    let missing = ResembleEnhance::weights_missing(&run).unwrap();
    assert!(missing.contains("hparams.yaml"), "{missing}");
    weights(&run);
    assert!(ResembleEnhance::weights_missing(&run).is_none());
    std::fs::write(
        run.join(RESEMBLE_WEIGHT_FILES[2]),
        "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 1\n",
    )
    .unwrap();
    let pointer = ResembleEnhance::weights_missing(&run).unwrap();
    assert!(pointer.contains("Git LFS"), "{pointer}");
}

/// Les trois fichiers attendus, factices.
fn weights(run_dir: &Path) {
    for rel in RESEMBLE_WEIGHT_FILES {
        let p = run_dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![0u8; 2048]).unwrap();
    }
}

#[cfg(unix)]
mod with_fake_binaries {
    use super::*;
    use crate::executor::Messenger;
    use penelope_app::bus::Origin;
    use penelope_app::services::Services;
    use penelope_app::testing::MockProviders;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, silent_wav};
    use std::sync::{Arc, Mutex};

    fn fake_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Faux `resemble-enhance` : journalise ses arguments, copie chaque WAV de `in_dir`
    /// dans `out_dir` sous le même nom, comme le vrai.
    fn fake_resemble(dir: &Path, log: &Path, body: Option<&str>) -> PathBuf {
        let default = "for f in \"$1\"/*.wav; do cp \"$f\" \"$2/$(basename \"$f\")\"; done";
        fake_bin(
            dir,
            "resemble-enhance",
            &format!(
                "printf '%s\\n' \"resemble $*\" >> '{}'\n{}",
                log.display(),
                body.unwrap_or(default)
            ),
        )
    }

    /// Faux `ffmpeg` : journalise, copie l'entrée (`-i`) vers le dernier argument.
    fn fake_ffmpeg(dir: &Path, log: &Path) -> PathBuf {
        fake_bin(
            dir,
            "ffmpeg",
            &format!(
                "printf '%s\\n' \"ffmpeg $*\" >> '{}'\n\
                 in=\"\"; prev=\"\"; last=\"\"\n\
                 for a in \"$@\"; do if [ \"$prev\" = \"-i\" ]; then in=\"$a\"; fi; \
                 prev=\"$a\"; last=\"$a\"; done\n\
                 cp \"$in\" \"$last\"",
                log.display()
            ),
        )
    }

    struct Bench {
        dir: tempfile::TempDir,
        log: PathBuf,
        cfg: VoicePostprocess,
        data: PathBuf,
    }

    impl Bench {
        fn new() -> Bench {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let log = dir.path().join("log.txt");
            let data = dir.path().join("data");
            weights(
                &data
                    .join("models")
                    .join("resemble-enhance")
                    .join("enhancer_stage2"),
            );
            let cfg = VoicePostprocess {
                enabled: true,
                timeout: "10s".into(),
                ffmpeg_bin: fake_ffmpeg(&bin, &log).to_string_lossy().into_owned(),
                resemble_bin: fake_resemble(&bin, &log, None)
                    .to_string_lossy()
                    .into_owned(),
                ..VoicePostprocess::default()
            };
            Bench {
                dir,
                log,
                cfg,
                data,
            }
        }

        fn run_dir(&self) -> PathBuf {
            self.data
                .join("models")
                .join("resemble-enhance")
                .join("enhancer_stage2")
        }

        fn wav(&self) -> PathBuf {
            let p = self.dir.path().join("in.wav");
            std::fs::write(&p, silent_wav(1.0)).unwrap();
            p
        }

        fn work(&self) -> PathBuf {
            self.dir.path().join("work")
        }

        fn log_lines(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn pipeline(&self) -> Pipeline {
            Pipeline::from_config(&self.cfg, &self.data).unwrap()
        }
    }

    #[tokio::test]
    async fn the_chain_runs_the_engine_then_the_studio_preset() {
        let b = Bench::new();
        let p = b.pipeline();
        let ready = p.readiness().unwrap();
        assert!(
            ready.contains(&b.run_dir().to_string_lossy().into_owned()),
            "{ready}"
        );
        assert!(ready.contains("calcul `cpu`"), "{ready}");

        let (outcome, finished) = p.process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Applied, "{outcome:?}");
        assert_eq!(outcome.engine, "resemble_enhance");
        assert!(outcome.reason.is_none());
        let finished = finished.unwrap();
        assert_eq!(finished, b.work().join("studio.wav"));
        assert_eq!(std::fs::read(&finished).unwrap(), silent_wav(1.0));

        let log = b.log_lines();
        assert_eq!(log.len(), 2, "{log:?}");
        let work = b.work();
        assert!(
            log[0].starts_with(&format!(
                "resemble {} {} --device cpu --run_dir {}",
                work.join("in").display(),
                work.join("out").display(),
                b.run_dir().display()
            )),
            "{}",
            log[0]
        );
        assert!(
            log[1].contains(&format!(
                "-i {} -af {} -ac 1 -ar 24000 {}",
                work.join("enhanced.wav").display(),
                VOICE_STUDIO_SOFT_FILTERS,
                finished.display()
            )),
            "{}",
            log[1]
        );
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["status"], "applied");
        assert!(json["seconds"].is_number());
        assert!(json.get("reason").is_none());
    }

    #[tokio::test]
    async fn engine_none_applies_the_filters_alone_and_empty_filters_do_nothing() {
        let mut b = Bench::new();
        b.cfg.engine = "none".into();
        let (outcome, finished) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Applied);
        assert_eq!(finished.unwrap(), b.work().join("studio.wav"));
        let log = b.log_lines();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(log[0].starts_with("ffmpeg "), "{}", log[0]);

        b.cfg.ffmpeg_filters = "  ".into();
        let p = b.pipeline();
        assert!(
            p.readiness()
                .unwrap()
                .contains("sans moteur ; sans filtres")
        );
        let wav = b.wav();
        let (outcome, finished) = p.process(&wav, &b.work()).await;
        assert_eq!(outcome.status, Status::Applied);
        assert_eq!(finished.unwrap(), wav);
        assert_eq!(b.log_lines().len(), 1);
    }

    #[tokio::test]
    async fn nothing_is_launched_when_a_binary_or_the_weights_are_missing() {
        let mut b = Bench::new();
        b.cfg.resemble_bin = b.dir.path().join("absent").to_string_lossy().into_owned();
        let (outcome, finished) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Skipped);
        assert!(finished.is_none());
        let reason = outcome.reason.unwrap();
        assert!(
            reason.contains("resemble-enhance") && reason.contains("uv tool install"),
            "{reason}"
        );
        assert!(b.log_lines().is_empty());

        let mut b = Bench::new();
        std::fs::remove_dir_all(b.run_dir()).unwrap();
        let (outcome, _) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Skipped);
        let reason = outcome.reason.unwrap();
        assert!(
            reason.contains("poids") && reason.contains("hparams.yaml"),
            "{reason}"
        );
        assert!(b.log_lines().is_empty());
        assert!(!b.work().exists(), "rien n'est créé quand rien n'est lancé");

        // Un `run_dir` désigné, avec `~` : développé, et vérifié là.
        b.cfg.resemble_run_dir = "~/poids-inexistants-penelope".into();
        let reason = b.pipeline().readiness().unwrap_err();
        assert!(!reason.contains('~'), "{reason}");
        assert!(reason.contains("poids-inexistants-penelope"), "{reason}");

        let mut b = Bench::new();
        b.cfg.ffmpeg_bin = b.dir.path().join("absent").to_string_lossy().into_owned();
        let err = Pipeline::from_config(&b.cfg, &b.data).err().unwrap();
        assert!(err.contains("ffmpeg"), "{err}");
    }

    #[tokio::test]
    async fn a_failing_engine_leaves_the_raw_wav_to_go() {
        let b = Bench::new();
        fake_resemble(
            &b.dir.path().join("bin"),
            &b.log,
            Some("echo 'CUDA boom' >&2; exit 1"),
        );
        let (outcome, finished) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Failed);
        assert!(finished.is_none());
        let reason = outcome.reason.unwrap();
        assert!(reason.contains("CUDA boom"), "{reason}");
        let log = b.log_lines();
        assert_eq!(
            log.len(),
            1,
            "les filtres ne tournent pas après l'échec : {log:?}"
        );

        // Un moteur qui réussit sans rien produire : nommé aussi.
        let b = Bench::new();
        fake_resemble(&b.dir.path().join("bin"), &b.log, Some("exit 0"));
        let (outcome, _) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert_eq!(outcome.status, Status::Failed);
        assert!(outcome.reason.unwrap().contains("aucun fichier"));
    }

    #[tokio::test]
    async fn a_slow_engine_is_stopped_at_the_timeout() {
        let mut b = Bench::new();
        fake_resemble(&b.dir.path().join("bin"), &b.log, Some("exec sleep 30"));
        b.cfg.timeout = "500ms".into();
        let started = std::time::Instant::now();
        let (outcome, finished) = b.pipeline().process(&b.wav(), &b.work()).await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(outcome.status, Status::TimedOut, "{outcome:?}");
        assert!(finished.is_none());
        assert_eq!(
            serde_json::to_value(&outcome).unwrap()["status"],
            "timed_out"
        );
        assert!(
            outcome
                .reason
                .unwrap()
                .contains("voice.postprocess.timeout")
        );
        // Sous charge, `sh` peut ne pas avoir démarré avant le délai : on ne compte pas la
        // ligne du moteur, on vérifie que les filtres n'ont jamais tourné.
        let log = b.log_lines();
        assert!(
            log.iter().all(|l| !l.starts_with("ffmpeg")),
            "les filtres ne tournent pas après le délai : {log:?}"
        );
    }

    #[test]
    fn doctor_names_what_is_missing_and_what_was_found() {
        let b = Bench::new();
        let check = crate::voice::postprocess_check(&b.cfg, &b.data);
        assert!(check.ok, "{check:?}");
        assert_eq!(check.id, "voice.postprocess");
        assert!(
            check.detail.contains("resemble_enhance"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("délai 10 s"), "{}", check.detail);

        let mut b = Bench::new();
        std::fs::remove_dir_all(b.run_dir()).unwrap();
        let check = crate::voice::postprocess_check(&b.cfg, &b.data);
        assert!(!check.ok);
        assert!(
            check.detail.contains("poids") && check.detail.contains("vocal part brut"),
            "{}",
            check.detail
        );
        assert!(check.fix.unwrap().contains("Post-traitement du vocal"));

        b.cfg.ffmpeg_bin = b.dir.path().join("absent").to_string_lossy().into_owned();
        let check = crate::voice::postprocess_check(&b.cfg, &b.data);
        assert!(!check.ok);
        assert!(check.detail.contains("ffmpeg"), "{}", check.detail);
        assert_eq!(check.fix.as_deref(), Some("brew install ffmpeg"));
    }

    /// Canal qui garde les vocaux reçus (chemin, durée) et les textes.
    #[derive(Default)]
    struct VoiceRecorder {
        voices: Mutex<Vec<(PathBuf, u32)>>,
        texts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Messenger for VoiceRecorder {
        async fn send_text(&self, _: &Origin, markdown: &str) -> Result<(), String> {
            self.texts.lock().unwrap().push(markdown.to_string());
            Ok(())
        }
        async fn send_file(&self, _: &Origin, _: &Path, _: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        async fn send_session_voice(
            &self,
            _: &str,
            _: &Origin,
            path: &Path,
            duration_s: u32,
            _: Option<&str>,
        ) -> Result<(), String> {
            assert!(path.is_file(), "{}", path.display());
            self.voices
                .lock()
                .unwrap()
                .push((path.to_path_buf(), duration_s));
            Ok(())
        }
    }

    /// Ce que le répertoire des vocaux contient : seuls les `.ogg` doivent rester.
    fn media_entries(data: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(data.join("media").join("voice"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[tokio::test]
    async fn send_keeps_the_voice_note_going_and_cleans_up_in_every_case() {
        let b = Bench::new();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let root = b.dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let s = Services::for_tests(root, clock).await.unwrap();
        let data = s.platform.dirs.data();
        // Les poids là où `send` les cherche par défaut, sous le répertoire de données.
        weights(
            &data
                .join("models")
                .join("resemble-enhance")
                .join("enhancer_stage2"),
        );
        let cfg = b.cfg.clone();
        s.config
            .mutate("test", move |c| {
                c.voice.postprocess = cfg;
                Ok(vec!["voice.postprocess".into()])
            })
            .unwrap();
        let providers = MockProviders::new(Arc::new(MockProvider::new()));
        let recorder = Arc::new(VoiceRecorder::default());
        let messenger: Arc<dyn Messenger> = recorder.clone();

        // Succès : la chaîne tourne, l'OGG part, il ne reste que lui.
        let out = crate::voice::send(
            &s,
            providers.as_ref(),
            Some(messenger.clone()),
            "s1",
            &Origin::Cli,
            "Bonjour tout le monde.",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["sent"], true);
        assert_eq!(out["postprocess"]["status"], "applied", "{out}");
        assert_eq!(out["postprocess"]["engine"], "resemble_enhance");
        assert_eq!(recorder.voices.lock().unwrap().len(), 1);
        let entries = media_entries(&data);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(entries[0].ends_with(".ogg"), "{entries:?}");
        let log = b.log_lines();
        assert_eq!(log.len(), 3, "moteur, filtres, encodage : {log:?}");
        assert!(log[2].contains("-c:a libopus -b:a 48k"), "{}", log[2]);

        // Échec du moteur : le vocal brut part, le statut le dit, rien ne traîne.
        fake_resemble(
            &b.dir.path().join("bin"),
            &b.log,
            Some("echo 'plus de mémoire' >&2; exit 3"),
        );
        let out = crate::voice::send(
            &s,
            providers.as_ref(),
            Some(messenger.clone()),
            "s1",
            &Origin::Cli,
            "Encore un mot.",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["sent"], true);
        assert_eq!(out["postprocess"]["status"], "failed", "{out}");
        assert!(
            out["postprocess"]["reason"]
                .as_str()
                .unwrap()
                .contains("plus de mémoire")
        );
        assert_eq!(recorder.voices.lock().unwrap().len(), 2);
        let last = b.log_lines().pop().unwrap();
        assert!(
            last.contains("-b:a 32k"),
            "le brut garde son débit : {last}"
        );
        let entries = media_entries(&data);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries.iter().all(|e| e.ends_with(".ogg")), "{entries:?}");

        // Éteint : statut `disabled`, encodage direct du brut.
        s.config
            .mutate("test", |c| {
                c.voice.postprocess.enabled = false;
                Ok(vec!["voice.postprocess.enabled".into()])
            })
            .unwrap();
        let before = b.log_lines().len();
        let out = crate::voice::send(
            &s,
            providers.as_ref(),
            Some(messenger.clone()),
            "s1",
            &Origin::Cli,
            "Et le dernier.",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["postprocess"]["status"], "disabled", "{out}");
        assert_eq!(out["postprocess"]["seconds"], 0.0);
        let log = b.log_lines();
        assert_eq!(
            log.len(),
            before + 1,
            "un seul ffmpeg, l'encodage : {log:?}"
        );
        assert!(media_entries(&data).iter().all(|e| e.ends_with(".ogg")));

        // Le journal porte le statut et la durée du post-traitement à chaque envoi.
        let events = s
            .events
            .session_events_of_kind("s1", "voice.sent")
            .await
            .unwrap();
        let statuses: Vec<&str> = events
            .iter()
            .map(|e| e.payload["postprocess"]["status"].as_str().unwrap_or("?"))
            .collect();
        assert_eq!(statuses, ["applied", "failed", "disabled"], "{events:?}");
        assert!(
            events
                .iter()
                .all(|e| e.payload["postprocess"]["seconds"].is_number())
        );
        assert!(
            recorder.texts.lock().unwrap().is_empty(),
            "aucun repli en texte"
        );
    }
}
