use super::*;
use crate::clock::TestClock;

fn cfg() -> Config {
    Config::sample(123)
}

#[test]
fn default_config_is_valid() {
    cfg().validate().unwrap();
}

/// #204 : un job d'outil a un seuil de proposition et deux plafonds. Un plafond nul
/// interdirait tout job sans le dire : refusé à la validation.
#[test]
fn tool_jobs_have_a_threshold_and_two_caps() {
    let c = cfg();
    assert_eq!(c.tools.background_after, "120s");
    assert_eq!(c.tools.jobs_per_session, 3);
    assert_eq!(c.tools.jobs_total, 10);
    parse_duration(&c.tools.background_after).unwrap();

    let mut bad = cfg();
    bad.tools.background_after = "deux minutes".into();
    assert!(bad.validate().is_err());

    let mut zero = cfg();
    zero.tools.jobs_per_session = 0;
    let e = zero.validate().unwrap_err().to_string();
    assert!(e.contains("tools.jobs_per_session"), "{e}");

    let mut narrow = cfg();
    narrow.tools.jobs_total = 2;
    let e = narrow.validate().unwrap_err().to_string();
    assert!(e.contains("tools.jobs_total"), "{e}");
}

#[test]
fn owner_is_required() {
    let c = Config::default();
    let e = c.validate().unwrap_err().to_string();
    assert!(e.contains("telegram_user_id"), "{e}");
}

/// #159 : seule une adresse de bouclage convient, et Slack n'enregistre que
/// `localhost` dans les URL de rappel d'une app.
#[test]
fn the_oauth_callback_host_must_be_a_loopback_name() {
    for host in ["127.0.0.1", "localhost"] {
        let mut c = cfg();
        c.mcp.callback_host = host.into();
        c.validate().expect(host);
    }
    for host in ["0.0.0.0", "penelope.example", "127.0.0.2", ""] {
        let mut c = cfg();
        c.mcp.callback_host = host.into();
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains("mcp.callback_host"), "{host} : {e}");
    }
}

/// #142 : un préfixe de fournisseur inconnu est une faute de frappe, refusée en
/// nommant le préfixe — avant, elle partait en silence chez OpenRouter, identifiant
/// complet en nom de modèle. Une variante OpenRouter (`:free`) reste acceptée.
#[test]
fn an_unknown_provider_prefix_is_refused_by_name() {
    check_model_id("codex:gpt-6-astra").unwrap();
    check_model_id("openrouter:deepseek/deepseek-v4-pro").unwrap();
    check_model_id("x-ai/grok-4:free").expect("une variante, pas un fournisseur");
    for (id, said) in [
        ("openroutr:a/b", "openroutr"),
        ("codx:gpt-6", "codx"),
        ("gpt-6-astra", "fournisseur:modèle"),
        ("codex:", "aucun modèle"),
    ] {
        let e = check_model_id(id).expect_err(id);
        assert!(e.contains(said), "{id} : {e}");
    }
    let mut c = cfg();
    c.models.aliases.insert("code".into(), "codx:gpt-6".into());
    let e = c.validate().unwrap_err().to_string();
    assert!(e.contains("codx") && e.contains("code"), "{e}");
}

/// #142 : les seuils de quota Codex sont des parts, et alerter après s'être mis en
/// retrait n'avertit de rien.
#[test]
fn codex_quota_ratios_are_checked() {
    let mut c = cfg();
    c.providers.codex.quota_alert_ratio = 1.5;
    assert!(
        c.validate()
            .unwrap_err()
            .to_string()
            .contains("entre 0 et 1")
    );
    let mut c = cfg();
    c.providers.codex.quota_alert_ratio = 0.99;
    c.providers.codex.quota_stop_ratio = 0.5;
    assert!(
        c.validate()
            .unwrap_err()
            .to_string()
            .contains("au plus quota_stop_ratio")
    );
}

#[test]
fn role_pointing_to_unknown_alias_is_rejected() {
    let mut c = cfg();
    c.models.roles.insert("code".into(), "nexistepas".into());
    assert!(c.validate().is_err());
}

/// #43 : un battement plus lent que la moitié du bail fait expirer les tours en cours.
#[test]
fn a_heartbeat_slower_than_half_the_lease_is_rejected() {
    let mut c = cfg();
    c.runners.heartbeat = "2m".into();
    c.runners.lease_ttl = "60s".into();
    let e = c.validate().unwrap_err().to_string();
    assert!(e.contains("runners.heartbeat"), "{e}");
    c.runners.heartbeat = "30s".into();
    c.validate().unwrap();
}

#[test]
fn toml_roundtrip() {
    let c = cfg();
    let s = c.to_toml().unwrap();
    let back = Config::from_toml(&s).unwrap();
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        serde_json::to_value(&back).unwrap()
    );
}

#[test]
fn unknown_key_is_rejected() {
    let e = Config::from_toml("[owner]\ntelegram_user_id = 1\nnimporte = 2\n").unwrap_err();
    assert!(e.to_string().contains("nimporte"), "{e}");
}

/// #76 : le fichier écrit par une version plus récente se relit. Section et clé
/// inconnues sont ignorées et nommées, pas fatales.
#[test]
fn unknown_sections_and_keys_are_tolerated_and_named() {
    let raw = "[owner]\ntelegram_user_id = 1\n\n[budget]\ndaily_usd = 9.0\nnouveau_plafond = 3\n\n[futur]\nactif = true\n\n[models.aliases]\nmain = \"x/y\"\n";
    let (c, unknown) = Config::parse(raw).unwrap();
    assert_eq!(c.owner.telegram_user_id, 1);
    assert_eq!(c.budget.daily_usd, 9.0);
    assert_eq!(
        c.models.aliases.get("main").map(String::as_str),
        Some("x/y")
    );
    assert_eq!(unknown, vec!["budget.nouveau_plafond", "futur"]);
    // Une valeur mal typée reste une erreur.
    assert!(Config::parse("[budget]\ndaily_usd = \"beaucoup\"\n").is_err());
}

/// #76 : une mutation ne touche que la clé visée ; commentaires, ordre, clés
/// inconnues et omissions restent, aucune clé nouvelle n'apparaît.
#[test]
fn a_mutation_edits_only_the_changed_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "# réglé à la main\n[owner]\ntelegram_user_id = 5 # moi\n\n[futur]\nactif = true\n\n[budget]\n# plafond du jour\ndaily_usd = 20.0\n";
    std::fs::write(&path, original).unwrap();
    let cs = ConfigStore::load_or_create(&path, None, std::sync::Arc::new(TestClock::default()), 0)
        .unwrap();
    assert_eq!(cs.unknown_keys(), vec!["futur"]);

    cs.mutate("cli", |c| {
        c.budget.daily_usd = 30.0;
        Ok(vec!["budget.daily_usd".into()])
    })
    .unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after,
        original.replace("daily_usd = 20.0", "daily_usd = 30.0"),
        "seule la valeur change"
    );

    // Une clé absente du fichier est ajoutée dans sa section, rien d'autre.
    cs.mutate("cli", |c| {
        c.telegram.quiet_hours = "23:00-06:00".into();
        Ok(vec!["telegram.quiet_hours".into()])
    })
    .unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with(&original.replace("daily_usd = 20.0", "daily_usd = 30.0")),
        "{after}"
    );
    assert!(
        after.ends_with("[telegram]\nquiet_hours = \"23:00-06:00\"\n"),
        "{after}"
    );
    let (relu, unknown) = Config::parse(&after).unwrap();
    assert_eq!(relu.telegram.quiet_hours, "23:00-06:00");
    assert_eq!(relu.budget.daily_usd, 30.0);
    assert_eq!(unknown, vec!["futur"]);
}

/// #76 : une entrée retirée d'une table libre disparaît du fichier.
#[test]
fn a_removed_map_entry_leaves_the_file() {
    let before = cfg();
    let mut after = before.clone();
    after.models.aliases.insert("essai".into(), "a/b".into());
    let text = edit_toml("", &before, &after).unwrap();
    assert_eq!(text, "[models.aliases]\nessai = \"a/b\"\n");
    let text2 = edit_toml(&text, &after, &before).unwrap();
    assert!(!text2.contains("essai"), "{text2}");
}

/// #76 : le fichier de premier démarrage ne porte que ce qui s'écarte des défauts.
#[test]
fn the_first_file_is_short_and_reads_back() {
    let text = Config::sample_toml(42).unwrap();
    assert!(text.starts_with("# Configuration de Pénélope."), "{text}");
    assert!(text.ends_with("[owner]\ntelegram_user_id = 42\n"), "{text}");
    let (c, unknown) = Config::parse(&text).unwrap();
    assert!(unknown.is_empty());
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        serde_json::to_value(Config::sample(42)).unwrap()
    );
}

/// #76 : les fichiers complets écrits par les versions publiées se relisent sans
/// clé inconnue. Une clé retirée plus tard sera signalée, jamais fatale.
#[test]
fn files_written_by_released_versions_still_load() {
    // Une entrée par version publiée dont le schéma a changé.
    const RELEASED: &[(&str, &str)] = &[(
        "0.17.0",
        include_str!("../../tests/fixtures/config-0.17.0.toml"),
    )];
    for (version, raw) in RELEASED {
        let (c, unknown) = Config::parse(raw).unwrap_or_else(|e| panic!("{version} : {e}"));
        assert!(unknown.is_empty(), "{version} : {unknown:?}");
        c.validate().unwrap_or_else(|e| panic!("{version} : {e}"));
    }
}

/// T16 (épopée #208) : un fichier écrit par une alpha porte encore `history.source`. Il
/// se charge sans erreur, et la clé n'est pas signalée comme inconnue : elle est retirée,
/// avec sa raison dans l'avertissement.
#[test]
fn a_file_with_the_retired_history_source_still_loads() {
    for value in ["journal", "tables"] {
        let raw = format!("[owner]\ntelegram_user_id = 1\n\n[history]\nsource = \"{value}\"\n");
        let (c, unknown) = Config::parse(&raw).unwrap();
        assert_eq!(c.owner.telegram_user_id, 1);
        assert!(unknown.is_empty(), "{value} : {unknown:?}");
    }
    let why = retired("history.source").unwrap();
    assert!(why.contains("journal"), "{why}");
    assert_eq!(retired("history"), Some(why));
    assert!(retired("historique").is_none());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[owner]\ntelegram_user_id = 5\n\n[history]\nsource = \"tables\"\n",
    )
    .unwrap();
    let cs = ConfigStore::load_or_create(&path, None, std::sync::Arc::new(TestClock::default()), 0)
        .unwrap();
    assert!(cs.unknown_keys().is_empty(), "{:?}", cs.unknown_keys());
    assert_eq!(cs.config().owner.telegram_user_id, 5);
}

/// #106 : une instance en service garde le réseau qu'elle a écrit ; un fichier sans
/// la clé prend le nouveau défaut, fermé.
#[test]
fn an_explicit_shell_network_survives_the_new_default() {
    let (old, _) = Config::parse(include_str!("../../tests/fixtures/config-0.17.0.toml")).unwrap();
    assert!(old.sandbox.shell_network, "valeur écrite par 0.17.0");
    let (fresh, _) = Config::parse("[owner]\nname = \"Anne\"\n").unwrap();
    assert!(!fresh.sandbox.shell_network, "défaut fermé");
    assert!(!Config::default().sandbox.shell_network);
}

/// #138 : une valeur seule vaut une liste d'un élément, sans découpage ; une liste
/// passe ; un autre type est refusé en nommant la forme attendue.
#[test]
fn a_single_value_fills_a_list_field() {
    let list = serde_json::json!(["x"]);
    assert_eq!(
        list_value("roots", &list, serde_json::json!("/a")).unwrap(),
        serde_json::json!(["/a"])
    );
    assert_eq!(
        list_value("args", &list, serde_json::json!("a b")).unwrap(),
        serde_json::json!(["a b"])
    );
    assert_eq!(
        list_value("roots", &list, serde_json::json!(["/a"])).unwrap(),
        serde_json::json!(["/a"])
    );
    let e = list_value("roots", &list, serde_json::json!(42)).unwrap_err();
    assert!(e.contains("une liste") && e.contains("`roots`"), "{e}");
    assert_eq!(
        list_value("timeout", &serde_json::json!("30s"), serde_json::json!(42)).unwrap(),
        serde_json::json!(42)
    );
}

/// #128 : chaque table déclarée libre est bien une table de la configuration.
#[test]
fn map_paths_are_maps() {
    let v = serde_json::to_value(Config::default()).unwrap();
    for path in MAP_PATHS {
        let node = path.split('.').fold(&v, |n, k| &n[k]);
        assert!(node.is_object(), "{path} : {node}");
    }
}

#[test]
fn durations_parse() {
    assert_eq!(parse_duration("500ms").unwrap().as_millis(), 500);
    assert_eq!(parse_duration("30s").unwrap().as_secs(), 30);
    assert_eq!(parse_duration("15m").unwrap().as_secs(), 900);
    assert_eq!(parse_duration("6h").unwrap().as_secs(), 21_600);
    assert_eq!(parse_duration("90d").unwrap().as_secs(), 7_776_000);
    assert!(parse_duration("12").is_err());
    assert!(parse_duration("3y").is_err());
}

#[test]
fn quiet_hours_cross_midnight() {
    let r = TimeRange::parse("22:00-07:00").unwrap();
    assert!(r.contains(23 * 60));
    assert!(r.contains(3 * 60));
    assert!(!r.contains(12 * 60));
    let r = TimeRange::parse("09:00-17:00").unwrap();
    assert!(r.contains(10 * 60));
    assert!(!r.contains(20 * 60));
}

#[test]
fn model_threshold_override() {
    let mut c = cfg();
    c.context
        .model_thresholds
        .insert("z-ai/glm-5.2".into(), 0.60);
    assert_eq!(c.compaction_threshold_for("openrouter:z-ai/glm-5.2"), 0.60);
    assert_eq!(c.compaction_threshold_for("autre/modele"), 0.70);
}

/// #45 : 800 mutations lancées par 4 threads. Aucune n'est refusée, aucune n'est
/// perdue, et le fichier sur disque porte la dernière génération.
#[test]
fn concurrent_mutations_never_lose_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cs = std::sync::Arc::new(ConfigStore::new(
        cfg(),
        &path,
        None,
        std::sync::Arc::new(TestClock::default()),
    ));
    let depart = cs.config().budget.daily_usd;

    std::thread::scope(|s| {
        for _ in 0..4 {
            let cs = cs.clone();
            s.spawn(move || {
                for _ in 0..200 {
                    cs.mutate("essai", |c| {
                        c.budget.daily_usd += 1.0;
                        Ok(vec!["budget.daily_usd".into()])
                    })
                    .expect("aucune mutation refusée");
                }
            });
        }
    });

    assert_eq!(cs.config().budget.daily_usd, depart + 800.0);
    assert_eq!(cs.generation(), 801);
    let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        on_disk.budget.daily_usd,
        cs.config().budget.daily_usd,
        "le fichier reflète la dernière génération"
    );
}

/// #45 : une relecture du fichier pendant une mutation. Les deux aboutissent, la
/// génération publiée est la dernière écrite, et le fichier la reflète.
#[test]
fn a_reload_racing_a_mutation_leaves_a_consistent_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cs = std::sync::Arc::new(ConfigStore::new(
        cfg(),
        &path,
        None,
        std::sync::Arc::new(TestClock::default()),
    ));
    // Le fichier porte une modification faite à la main, hors du daemon.
    let mut edite = (*cs.config()).clone();
    edite.runners.count = 7;
    std::fs::write(&path, edite.to_toml().unwrap()).unwrap();

    std::thread::scope(|s| {
        let a = cs.clone();
        s.spawn(move || a.reload_from_disk().expect("relecture"));
        let b = cs.clone();
        s.spawn(move || {
            b.mutate("cli", |c| {
                c.budget.daily_usd = 123.0;
                Ok(vec!["budget.daily_usd".into()])
            })
            .expect("mutation")
        });
    });

    let fin = cs.config();
    assert_eq!(cs.generation(), 3, "les deux générations sont publiées");
    assert_eq!(fin.budget.daily_usd, 123.0, "la mutation survit");
    let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&*fin).unwrap(),
        serde_json::to_value(&on_disk).unwrap(),
        "le fichier porte la dernière génération"
    );

    // La relecture voit toujours ce que le fichier contient à son tour de verrou : une
    // édition manuelle relue après la mutation entre elle aussi.
    let mut edite = (*cs.config()).clone();
    edite.runners.count = 9;
    std::fs::write(&path, edite.to_toml().unwrap()).unwrap();
    cs.reload_from_disk().unwrap();
    assert_eq!(cs.config().runners.count, 9);
    assert_eq!(cs.config().budget.daily_usd, 123.0);
}

/// CA 4 : générations strictement croissantes, sans perte d'écriture.
#[test]
fn ca_4_3_generations_are_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cs = ConfigStore::new(
        cfg(),
        &path,
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    assert_eq!(cs.generation(), 1);

    let g2 = cs
        .mutate("cli", |c| {
            c.budget.daily_usd = 50.0;
            Ok(vec!["budget.daily_usd".into()])
        })
        .unwrap();
    assert_eq!(g2.generation, 2);

    let g3 = cs
        .mutate("telegram", |c| {
            c.runners.count = 8;
            Ok(vec!["runners.count".into()])
        })
        .unwrap();
    assert_eq!(g3.generation, 3);
    assert_eq!(
        cs.config().budget.daily_usd,
        50.0,
        "pas de perte d'écriture"
    );
    assert_eq!(cs.config().runners.count, 8);

    // Le fichier sur disque reflète la dernière génération.
    let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk.runners.count, 8);
    assert_eq!(on_disk.budget.daily_usd, 50.0);
}

#[test]
fn invalid_mutation_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cs = ConfigStore::new(
        cfg(),
        &path,
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    let before = cs.generation();
    let r = cs.mutate("cli", |c| {
        c.runners.count = 0;
        Ok(vec!["runners.count".into()])
    });
    assert!(r.is_err());
    assert_eq!(cs.generation(), before);
    assert_eq!(cs.config().runners.count, 4);
}

#[test]
fn snapshot_is_frozen_for_the_reader() {
    let dir = tempfile::tempdir().unwrap();
    let cs = ConfigStore::new(
        cfg(),
        dir.path().join("c.toml"),
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    let snap = cs.snapshot();
    cs.mutate("cli", |c| {
        c.budget.daily_usd = 99.0;
        Ok(vec![])
    })
    .unwrap();
    assert_eq!(
        snap.config.budget.daily_usd, 20.0,
        "le tour garde son instantané"
    );
    assert_eq!(cs.config().budget.daily_usd, 99.0);
}

#[test]
fn stale_apply_result_cannot_overwrite_newer() {
    let dir = tempfile::tempdir().unwrap();
    let cs = ConfigStore::new(
        cfg(),
        dir.path().join("c.toml"),
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    assert!(cs.record_apply("mcp", ApplyResult::AppliedLive { generation: 5 }));
    assert!(!cs.record_apply(
        "mcp",
        ApplyResult::Rejected {
            generation: 3,
            reason: "vieux".into()
        }
    ));
    assert_eq!(
        cs.apply_results().get("mcp"),
        Some(&ApplyResult::AppliedLive { generation: 5 })
    );
}

#[test]
fn reload_from_disk_reports_changed_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cs = ConfigStore::new(
        cfg(),
        &path,
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    let mut edited = cfg();
    edited.budget.daily_usd = 7.5;
    std::fs::write(&path, edited.to_toml().unwrap()).unwrap();
    let g = cs.reload_from_disk().unwrap();
    assert!(
        g.changed.iter().any(|p| p == "budget.daily_usd"),
        "{:?}",
        g.changed
    );
}

/// #164 : une racine existante est conservée sous la forme réelle du volume,
/// au chargement puis lors de `config_set` (mutation persistée).
#[test]
fn workspace_paths_are_canonicalised_at_load_and_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let actual = dir.path().join("penelope");
    let alias = dir.path().join("Penelope");
    std::fs::create_dir(&actual).unwrap();
    if std::fs::canonicalize(&alias).is_err() {
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
    }
    let mut initial = cfg();
    initial.sandbox.workspaces = vec![alias.to_string_lossy().into_owned()];
    let cs = ConfigStore::new(
        initial,
        dir.path().join("config.toml"),
        None,
        std::sync::Arc::new(TestClock::default()),
    );
    let expected = actual.canonicalize().unwrap().to_string_lossy().to_string();
    assert_eq!(cs.config().sandbox.workspaces, vec![expected.clone()]);
    cs.mutate("test", |c| {
        c.sandbox.workspaces = vec![alias.to_string_lossy().into_owned()];
        Ok(vec!["sandbox.workspaces".into()])
    })
    .unwrap();
    assert_eq!(cs.config().sandbox.workspaces, vec![expected]);
}

#[test]
fn runtime_stream_requires_loopback_and_per_consumer_token() {
    let mut config = cfg();
    config.observability.runtime_consumers = vec![RuntimeConsumer {
        name: "watchdog".into(),
        token_secret: "runtime_watchdog_token".into(),
        kinds: vec!["runtime.tool".into()],
    }];
    assert!(config.validate().is_ok());
    config.observability.runtime_stream_bind = "0.0.0.0:9465".into();
    assert!(config.validate().is_err());
    config.observability.runtime_stream_bind = "127.0.0.1:9465".into();
    config.observability.runtime_consumers[0]
        .token_secret
        .clear();
    assert!(config.validate().is_err());
}

#[test]
fn restart_only_paths_are_limited() {
    assert!(restart_allowed("telegram.token"));
    assert!(restart_allowed("observability.runtime_stream_bind"));
    assert!(restart_allowed("observability.runtime_consumers"));
    assert!(restart_allowed("store.path"));
    assert!(!restart_allowed("runners.count"));
}

#[test]
fn atomic_write_replaces_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.toml");
    atomic_write(&p, b"a").unwrap();
    atomic_write(&p, b"bb").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "bb");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "aucun fichier temporaire ne doit rester"
    );
}
