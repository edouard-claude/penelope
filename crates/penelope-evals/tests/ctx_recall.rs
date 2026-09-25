//! Suite `ctx-recall` (§5, §20.1, réseau) : un fait donné tôt dans une conversation reste
//! retrouvable après que ce passage a été résumé (compaction niveau 3, modèle réel).
//!
//! ```bash
//! OPENROUTER_API_KEY=… penelope eval ctx-recall
//! ```
//!
//! Chaque fait est planté, noyé sous des échanges sans rapport, puis la session est
//! compactée ; la question finale n'a plus le passage d'origine sous les yeux, seulement
//! le résumé et l'outillage d'historique.

use penelope_app::bus::Origin;
use penelope_conversation::compaction::{Trigger, compact};
use penelope_daemon::compaction::context_of;
use penelope_evals::live;
use penelope_kernel::clock::{SharedClock, SystemClock};
use std::sync::Arc;

/// Fait, question, réponse attendue (sous-chaîne, casse ignorée).
const FACTS: &[(&str, &str, &str)] = &[
    (
        "Pour mémoire : la réunion de lancement avec le client Borealis est fixée au jeudi \
         22 octobre à 14 h 30, salle Caravelle.",
        "Quelle salle avions-nous retenue pour la réunion Borealis ?",
        "caravelle",
    ),
    (
        "Le serveur de préproduction du projet Albatros s'appelle pp-albatros-07.",
        "Comment s'appelle le serveur de préproduction d'Albatros ?",
        "pp-albatros-07",
    ),
];

const FILLER: &[&str] = &[
    "Donne-moi trois idées de titres pour un article sur le compostage urbain.",
    "Résume en deux phrases le principe de la photosynthèse.",
    "Traduis en anglais : « le chat dort sur le radiateur ».",
    "Quelle est la différence entre un tableau et une liste chaînée ?",
    "Propose un nom pour une boulangerie de quartier.",
    "Explique en une phrase ce qu'est un pull request.",
];

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn facts_survive_a_level_3_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let d = live::daemon(dir.path(), clock).await;
    let session = d.chat_session_for(&Origin::Cli).await.unwrap();

    for (fact, _, _) in FACTS {
        live::turn(&d, &session, fact).await;
    }
    for filler in FILLER {
        live::turn(&d, &session, filler).await;
    }
    let report = compact(&context_of(&d), &session, Trigger::Manual, None)
        .await
        .expect("compaction");
    assert!(report.published >= 1, "aucun résumé publié : {report:?}");

    let mut found = 0;
    for (_, question, expected) in FACTS {
        let answer = live::turn(&d, &session, question).await;
        let ok = answer.to_lowercase().contains(expected);
        eprintln!("{} {question}\n   → {answer}", if ok { "✅" } else { "❌" });
        found += usize::from(ok);
    }
    assert_eq!(found, FACTS.len(), "faits perdus après compaction");
}
