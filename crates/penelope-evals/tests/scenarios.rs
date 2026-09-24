//! Suite `scenarios` (règle R11, tâche T0) : chaque répertoire de
//! `crates/penelope-evals/scenarios/` est rejoué sans clé, sur des services de test, et
//! comparé à `expected.jsonl` (le monde après le run) et `surface.jsonl` (chaque requête
//! vue par le modèle).
//!
//! ```bash
//! cargo test -p penelope-evals --test scenarios                  # rejeu
//! UPDATE_SCENARIOS=1 cargo test -p penelope-evals --test scenarios   # régénère les attendus
//! RECORD_SCENARIO=tour-simple OPENROUTER_API_KEY=… cargo test -p penelope-evals --test scenarios
//! ```
//!
//! Un cas par répertoire ; `every_scenario_directory_has_a_case` refuse un répertoire
//! sans cas et un cas sans répertoire.

use penelope_evals::scenario;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

macro_rules! scenario_cases {
    ($($case:ident => $dir:literal),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                if let Err(e) = scenario::check(&root().join($dir)).await {
                    panic!("{e}");
                }
            }
        )*
        const REGISTERED: &[&str] = &[$($dir),*];
    };
}

scenario_cases! {
    tour_simple => "tour-simple",
    outil_lecture => "outil-lecture",
    lectures_paralleles => "lectures-paralleles",
    niveau_1_et_groupe => "niveau-1-et-groupe",
    messages_fusionnes => "messages-fusionnes",
    reponse_vide_relancee => "reponse-vide-relancee",
    flux_coupe => "flux-coupe",
    compaction_puis_prolongation => "compaction-puis-prolongation",
    session_froide => "session-froide",
    depassement_prouve => "depassement-prouve",
    fork_puis_divergence => "fork-puis-divergence",
    rewind => "rewind",
    purge => "purge",
    crash_deux_vies => "crash-deux-vies",
    approbation_apres_redemarrage => "approbation-apres-redemarrage",
    redemarrages_en_serie => "redemarrages-en-serie",
}

#[test]
fn every_scenario_directory_has_a_case() {
    let dirs: Vec<String> = scenario::list(&root())
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    for d in &dirs {
        assert!(
            REGISTERED.contains(&d.as_str()),
            "scénario `{d}` sans cas dans tests/scenarios.rs : l'ajouter à `scenario_cases!`"
        );
    }
    for r in REGISTERED {
        assert!(
            dirs.iter().any(|d| d == r),
            "cas `{r}` sans répertoire scenarios/{r}/"
        );
    }
}

/// Le test des jetons de normalisation : deux rejeux de chaque scénario, dans le même
/// processus, donnent le même monde et la même surface, octet pour octet.
#[tokio::test]
async fn two_replays_of_every_scenario_are_identical() {
    let text = |lines: &[serde_json::Value]| {
        lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for dir in scenario::list(&root()) {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let first = scenario::replay(&dir).await.unwrap();
        let second = scenario::replay(&dir).await.unwrap();
        assert_eq!(
            text(&first.0),
            text(&second.0),
            "scénario {name} : le monde diffère entre deux rejeux"
        );
        assert_eq!(
            text(&first.1),
            text(&second.1),
            "scénario {name} : la surface diffère entre deux rejeux"
        );
    }
}

/// Critère de T14 (épopée #208) : chaque scénario envoie au modèle les mêmes requêtes,
/// octet pour octet, que la conversation se relise dans les tables ou dans le journal.
#[tokio::test]
async fn both_history_sources_send_the_same_requests() {
    if std::env::var_os("PENELOPE_HISTORY_SOURCE").is_some() {
        eprintln!("PENELOPE_HISTORY_SOURCE impose la source : comparaison sans objet");
        return;
    }
    for dir in scenario::list(&root()) {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let tables = scenario::replay_from(&dir, "tables").await.unwrap();
        let journal = scenario::replay_from(&dir, "journal").await.unwrap();
        assert!(!tables.is_empty(), "scénario {name} : aucune requête");
        if let Err(e) = scenario::compare_surface(&name, &tables, &journal) {
            panic!("tables contre journal : {e}");
        }
        // L'égalité des valeurs ignore l'ordre des clés : les octets aussi.
        for (i, (t, j)) in tables.iter().zip(&journal).enumerate() {
            let (t, j) = (t.to_string(), j.to_string());
            assert_eq!(t, j, "scénario {name}, appel {} : octets différents", i + 1);
        }
    }
}
