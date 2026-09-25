//! Filet de migration depuis une vraie base 0.17 (lot B, épopée #208 ; tâche T10 de
//! `design/v1/gel-et-outillage.md`).
//!
//! `upgrade_from_each_previous_version` (`src/migrations.rs`) ne rejoue que `0001` puis
//! migre, sans base réelle. Ici, une base **produite par la 0.17 elle-même** (`Store::open`
//! puis semis de données représentatives) est commitée dans `tests/fixtures/`, et chaque
//! `Store::open` de la V1 doit la migrer entière et la relire.
//!
//! Deux tests :
//! - `generate_fixture` (`#[ignore]`) : le générateur. Avec `UPDATE_FIXTURE=1`, il écrit
//!   `tests/fixtures/penelope-<version du workspace>.db` ; sans la variable, il travaille
//!   dans un répertoire temporaire et ne touche pas au dépôt.
//! - `a_real_0_17_database_migrates_and_reads_back` : le filet, sur une copie de chaque
//!   fixture présente.
//!
//! Le semis passe par SQL direct pour les tables métier, et par les primitives du noyau
//! pour ce qui a une signature : la chaîne d'événements (`EventLog`) et le ledger
//! d'effets (`EffectLedger`). `tests/fixtures/README.md` dit quand régénérer, et
//! pourquoi pas à chaque migration.

use penelope_kernel::clock::{Clock, SharedClock, TestClock};
use penelope_kernel::effects::{EffectKind, EffectLedger, EffectSpec, Planned};
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_store::rusqlite::{self, Connection, Transaction, params};
use penelope_store::{MIGRATIONS, Store, applied_versions};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------- données semées
//
// Les identifiants sont partagés par le générateur et par le test : la fixture se relit
// par ces valeurs. Les changer impose de régénérer la fixture.

/// 2026-09-20T08:00:00Z, l'instant où la « 0.17 » a écrit la base.
const START_MS: i64 = 1_789_891_200_000;
const SESSION_ID: &str = "s_01K5N0Q5T3B9V8X2M4R7C6A1E0";
const CHAT_ID: i64 = 123_456_789;
const MODEL_ID: &str = "openrouter:deepseek/deepseek-v4-pro";
const TURN_ID: &str = "turn_01K5N0Q5T3B9V8X2M4R7C6A1E3";
const TOOL_CALL_ID: &str = "call_fs_list_01";
const LCM_ACTIVE_NODE: &str = "lcm_01K5N0Q5T3B9V8X2M4R7C6A1E1";
const LCM_LEAF_NODE: &str = "lcm_01K5N0Q5T3B9V8X2M4R7C6A1E2";
const MEM_UID: &str = "m_01K5N0Q5T3B9V8X2M4R7C6A1E4";
const SCHEDULE_ID: &str = "sch_01K5N0Q5T3B9V8X2M4R7C6A1E5";
const OUTBOX_ID: &str = "out_01K5N0Q5T3B9V8X2M4R7C6A1E6";
const APPROVAL_ID: &str = "apr_01K5N0Q5T3B9V8X2M4R7C6A1E7";
const POLICY_ID: &str = "pol_01K5N0Q5T3B9V8X2M4R7C6A1E8";
const LLM_REQUEST_ID: &str = "llm_01K5N0Q5T3B9V8X2M4R7C6A1E9";
const SYSTEM_HASH: &str = "3f1c9a7e5b2d4068c1e3a5b7d9f0246813579bdf02468ace13579bdf02468ace";
/// Événements écrits par `seed_kernel`.
const EVENT_COUNT: u64 = 7;

/// La conversation : (rôle, contenu, `tool_call_id`, `tool_name`). Le contenu suit
/// `serialise_content` de `penelope-context` : blocs typés et appels d'outils.
const MESSAGES: &[(&str, &str, Option<&str>, Option<&str>)] = &[
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Peux-tu lister les fichiers du projet ?"}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[],"tool_calls":[{"id":"call_fs_list_01","name":"fs_list","arguments":{"path":"."}}]}"#,
        None,
        None,
    ),
    (
        "tool",
        r#"{"blocks":[{"type":"text","text":"Cargo.toml\nREADME.md\nsrc/\ntests/"}],"tool_calls":[]}"#,
        Some(TOOL_CALL_ID),
        Some("fs_list"),
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Quatre entrées : Cargo.toml, README.md, src/ et tests/."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Lance les tests."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[],"tool_calls":[{"id":"call_shell_02","name":"shell_exec","arguments":{"cmd":"cargo test"}}]}"#,
        None,
        None,
    ),
    (
        "tool",
        r#"{"blocks":[{"type":"text","text":"test result: ok. 12 passed; 0 failed"}],"tool_calls":[]}"#,
        Some("call_shell_02"),
        Some("shell_exec"),
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Les 12 tests passent."}],"tool_calls":[],"reasoning":"La sortie ne montre aucun échec."}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Merci. Rappelle-moi de relire les issues chaque matin de semaine."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"C'est planifié : à 8 h du lundi au vendredi."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Parfait, réponses courtes à l'avenir."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Noté."}],"tool_calls":[]}"#,
        None,
        None,
    ),
];

// ---------------------------------------------------------------- chemins

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Le nom dit d'où vient la base : la version du workspace qui l'a écrite.
fn fixture_name() -> String {
    format!("penelope-{}.db", env!("CARGO_PKG_VERSION"))
}

/// Instant `secs` secondes après le début du semis, au format du noyau.
fn at(secs: i64) -> String {
    TestClock::new(START_MS + secs * 1000).now_rfc3339()
}

#[path = "migration_from_0_17/checks.rs"]
mod checks;
#[path = "migration_from_0_17/seed.rs"]
mod seed;
use checks::*;
use seed::*;

// ---------------------------------------------------------------- tests

/// Générateur de la fixture. Ignoré : ne tourne que sur demande.
///
/// ```bash
/// UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored
/// ```
///
/// Sans `UPDATE_FIXTURE=1`, la base est produite et vérifiée dans un répertoire
/// temporaire, sans rien écrire dans le dépôt.
#[tokio::test]
#[ignore = "générateur : UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored"]
async fn generate_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let produced = generate(dir.path()).await;
    let size = std::fs::metadata(&produced).unwrap().len();
    // Le générateur relit ce qu'il vient d'écrire, sur une copie : la fixture reste
    // intacte jusqu'à la copie finale.
    check_fixture(&produced).await;

    if std::env::var("UPDATE_FIXTURE").as_deref() == Ok("1") {
        std::fs::create_dir_all(fixtures_dir()).unwrap();
        let dest = fixtures_dir().join(fixture_name());
        std::fs::copy(&produced, &dest).unwrap();
        eprintln!("fixture écrite : {} ({size} octets)", dest.display());
    } else {
        eprintln!(
            "fixture produite dans {} ({size} octets), non copiée dans le dépôt : \
             UPDATE_FIXTURE=1 pour l'écrire dans tests/fixtures/",
            produced.display()
        );
    }
}

/// Le filet (gel-et-outillage.md §5.2 point 4) : chaque fixture du répertoire, copiée,
/// migrée par `Store::open` du code courant, puis relue. La fixture est censée être plus
/// ancienne que le code : c'est l'écart entre les deux que le test mesure.
#[tokio::test]
async fn a_real_0_17_database_migrates_and_reads_back() {
    let fixtures = fixture_files();
    assert!(
        !fixtures.is_empty(),
        "aucune fixture penelope-*.db dans {} : \
         UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored",
        fixtures_dir().display()
    );
    let code = version_triple(env!("CARGO_PKG_VERSION"));
    for fixture in fixtures {
        let name = fixture.file_name().unwrap().to_string_lossy().into_owned();
        let written_by = name.trim_start_matches("penelope-").trim_end_matches(".db");
        assert!(
            version_triple(written_by) <= code,
            "{name} vient d'une version plus récente que le code ({})",
            env!("CARGO_PKG_VERSION")
        );
        check_fixture(&fixture).await;
    }
}
