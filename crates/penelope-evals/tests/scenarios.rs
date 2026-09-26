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
    outils_niveau_1_et_compaction => "outils-niveau-1-et-compaction",
    message_pendant_un_lot => "message-pendant-un-lot",
    commandes_systeme => "commandes-systeme",
    commandes_sessions => "commandes-sessions",
    commandes_reglages => "commandes-reglages",
    commandes_approbations => "commandes-approbations",
    commandes_memoire => "commandes-memoire",
    commandes_workflows => "commandes-workflows",
    commandes_extensions => "commandes-extensions",
    // Méthodes RPC (critère 7, R10), une famille par scénario.
    rpc_sessions => "rpc-sessions",
    rpc_config_et_modeles => "rpc-config-et-modeles",
    rpc_approbations => "rpc-approbations",
    rpc_planifications => "rpc-planifications",
    rpc_workflows => "rpc-workflows",
    rpc_memoire => "rpc-memoire",
    rpc_coffre_et_accueil => "rpc-coffre-et-accueil",
    rpc_conversation => "rpc-conversation",
    rpc_mcp_et_import => "rpc-mcp-et-import",
    rpc_skills => "rpc-skills",
    rpc_exploitation => "rpc-exploitation",
    rpc_arret => "rpc-arret",
    outils_fichiers_et_shell => "outils-fichiers-et-shell",
    outils_memoire => "outils-memoire",
    outils_skills => "outils-skills",
    outils_soi => "outils-soi",
    outils_historique => "outils-historique",
    outils_http_garde => "outils-http-garde",
    outils_workflows => "outils-workflows",
    outils_question => "outils-question",
    outils_canal => "outils-canal",
    outils_git => "outils-git",
    outils_planification => "outils-planification",
    outils_historique_resumes => "outils-historique-resumes",
    outils_jobs => "outils-jobs",
    outils_workflow_run => "outils-workflow-run",
    outils_images => "outils-images",
    outils_sous_agent => "outils-sous-agent",
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

/// Le scénario des critères d'acceptation du journal (épopée #208, T22) : un tour avec
/// outil, niveau 1 et compaction d'urgence, puis un tour de plus.
const JOURNAL_CA: &str = "outils-niveau-1-et-compaction";

/// CA 4.5 (`source-de-verite.md` §2.1, invariant 1) : ce que le modèle lit est journalisé.
/// À chaque appel du scénario, la requête reçue par le fournisseur est, octet pour octet,
/// le pliage du journal d'avant sa réponse (`harness/visible.rs`) ; le contrôle tourne à
/// la fin de chaque scénario, celui-ci vérifie qu'il a porté sur le tour qui combine
/// outil, niveau 1 et compaction.
#[tokio::test]
async fn ca_4_5_model_visible_is_logged() {
    let audit = scenario::audit(&root().join(JOURNAL_CA)).await.unwrap();
    let v = &audit.visible;
    // Deux tours simples, trois appels au troisième (outil, dépassement, réponse), un au
    // quatrième.
    assert_eq!(v.compared, 6, "{v:?}");
    assert_eq!(v.transformed, 0, "aucun niveau 0, 2 ou 4 attendu : {v:?}");
    assert_eq!(v.unpinned, 0, "chaque appel cite sa requête : {v:?}");
    assert_eq!(v.within_turn.get("conv.tool_result"), Some(&1), "{v:?}");
    assert_eq!(v.within_turn.get("conv.summary"), Some(&1), "{v:?}");
}

/// CA 5.5 (`source-de-verite.md` §3.2) : aucun remplacement entre deux appels d'un même
/// tour, hors niveau 1 d'un résultat jamais envoyé et résumé d'un dépassement prouvé. Le
/// contrôle tourne à la fin de chaque scénario ; ici, il doit avoir admis les deux cas,
/// et seulement eux.
#[tokio::test]
async fn ca_5_5_replace_only_at_a_turn_boundary() {
    let audit = scenario::audit(&root().join(JOURNAL_CA)).await.unwrap();
    let kinds: Vec<&str> = audit
        .visible
        .within_turn
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(kinds, ["conv.summary", "conv.tool_result"]);
}

/// CA 4.6 (`source-de-verite.md` §2.1, invariant 3) : les tables de messages sont des
/// caches. Toutes les lignes non scellées effacées, `history reindex` les redonne à
/// l'identique, numéros compris, et `history verify` reste à zéro
/// (`harness/journal.rs`, à la fin de chaque scénario).
#[tokio::test]
async fn ca_4_6_reindex_is_lossless() {
    let audit = scenario::audit(&root().join(JOURNAL_CA)).await.unwrap();
    // Neuf messages, un résumé, des contextes figés, le plein texte : bien plus que rien.
    assert!(audit.reindexed >= 20, "{audit:?}");
}
