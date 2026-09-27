//! Scellement d'une vraie base 0.17 (épopée #208, T11, `design/v1/source-de-verite.md`
//! §4.5).
//!
//! La fixture de `penelope-store` (`tests/fixtures/penelope-*.db`, décrite par son
//! `README.md`) est copiée, migrée par `Store::open`, puis scellée par le code que le
//! démarrage exécute (`HistoryStore::seal_legacy`). Le test vit ici pour que le store ne
//! dépende pas de `penelope-context`, même en développement ; `migration_from_0_17`
//! reste le filet de la migration seule.

use penelope_context::HistoryStore;
use penelope_context::derive::derive;
use penelope_context::store::seal::SealReport;
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::event::EventLog;
use penelope_store::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Constantes de `penelope-store/tests/migration_from_0_17.rs`, qui a semé la fixture.
const START_MS: i64 = 1_789_891_200_000;
const SESSION_ID: &str = "s_01K5N0Q5T3B9V8X2M4R7C6A1E0";
const LCM_ACTIVE_NODE: &str = "lcm_01K5N0Q5T3B9V8X2M4R7C6A1E1";
const MESSAGES: i64 = 12;
const EVENT_COUNT: u64 = 7;
/// Empreinte du préfixe de chaque fixture, telle que les alphas l'ont posée dans son
/// `conv.import` : une base déjà scellée doit continuer à vérifier. Une ligne par
/// fixture, et chacune doit être présente : la 0.17.59 (lot B) et la 0.17.62, dernière
/// 0.17 avant la bascule. Le semis étant le même, l'empreinte l'est aussi.
const SEALED_DIGESTS: &[(&str, &str)] = &[
    (
        "penelope-0.17.59.db",
        "ce06951cc2add7e3cea1d128d0d52b9f138c8e44e055d31408ade20e986433fd",
    ),
    (
        "penelope-0.17.62.db",
        "ce06951cc2add7e3cea1d128d0d52b9f138c8e44e055d31408ade20e986433fd",
    ),
];

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../penelope-store/tests/fixtures");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} : {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("penelope-") && n.ends_with(".db"))
        })
        .collect();
    out.sort();
    out
}

/// Un `conv.import` pour la session de la fixture, lignes marquées, second passage sans
/// effet, empreinte relue à l'identique, et la dérivation redonne la projection V0 : le
/// nœud actif (qui couvre 1 à 8 sans que les lignes portent le drapeau), puis les
/// messages 9 à 12. La chaîne compte un maillon de plus et se vérifie.
#[tokio::test]
async fn a_real_0_17_database_is_sealed_once() {
    let fixtures = fixtures();
    let names: Vec<String> = fixtures
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<&str> = SEALED_DIGESTS.iter().map(|(f, _)| *f).collect();
    assert_eq!(names, expected, "une fixture par ligne de SEALED_DIGESTS");
    for fixture in fixtures {
        let name = fixture.file_name().unwrap().to_string_lossy().into_owned();
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("penelope.db");
        std::fs::copy(&fixture, &copy).unwrap();
        let store = Store::open(&copy).unwrap();
        let clock: SharedClock = Arc::new(TestClock::new(START_MS + 86_400_000));
        let log = EventLog::new(store.clone(), clock.clone());
        let history = HistoryStore::new(store.clone(), clock, log.clone());

        let report = history.seal_legacy().await.unwrap();
        assert_eq!(
            report.sealed,
            [(SESSION_ID.to_string(), MESSAGES)],
            "{name}"
        );
        assert_eq!(
            history.seal_legacy().await.unwrap(),
            SealReport::default(),
            "{name}"
        );

        let (import, prefix) = history.sealed_prefix(SESSION_ID).await.unwrap().unwrap();
        assert_eq!(import.digest, prefix.digest(), "{name}");
        let (_, digest) = SEALED_DIGESTS.iter().find(|(f, _)| *f == name).unwrap();
        assert_eq!(
            import.digest, *digest,
            "{name} : empreinte d'une alpha antérieure"
        );
        assert_eq!((import.messages, import.contexts), (MESSAGES, 1), "{name}");
        assert_eq!(import.lcm_active.len(), 1, "{name}");
        assert_eq!(import.lcm_active[0].node, LCM_ACTIVE_NODE, "{name}");

        let events = log.session_events(SESSION_ID, 0).await.unwrap();
        let surface = derive(&prefix.sealed(), &events).unwrap();
        let projected = surface.projected_entries();
        assert_eq!(projected.len(), 5, "{name} : résumé puis messages 9 à 12");
        assert!(
            projected[0]
                .message
                .text()
                .contains("cargo test : 12 tests verts"),
            "{name}"
        );
        assert_eq!(projected[4].message.text(), "Noté.", "{name}");

        // Le nœud actif de la fixture porte des ancres et un `tokens_src` (900) distinct
        // de `tokens_self` (80) : la relecture du préfixe scellé doit les rendre.
        let verify = history.verify(None, None).await.unwrap();
        assert!(verify.ok, "{name} : {:#?}", verify.divergences);

        let chain = log.verify().await.unwrap();
        assert!(chain.ok, "{name} : {:?}", chain.detail);
        assert_eq!(chain.checked, EVENT_COUNT + 1, "{name}");
        store.close();
    }
}
