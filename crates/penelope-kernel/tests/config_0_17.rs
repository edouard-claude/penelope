//! Filet de configuration (lot B, épopée #208 ; tâche T10 de
//! `design/v1/gel-et-outillage.md`, §5.2 point 5).
//!
//! `tests/fixtures/config-0.17.toml` porte **chaque** clé de la référence générée de
//! `docs/install-headless.md` (bloc `reference:config`), avec une valeur valide. La V1
//! doit le relire sans erreur, sans clé inconnue, sans refus ni avertissement de
//! cohérence ; et le fichier ne doit pas prendre de retard sur la référence : une clé
//! nouvelle est nommée par le test.

use penelope_kernel::clock::TestClock;
use penelope_kernel::coherence::{Gravity, contradictions};
use penelope_kernel::config::ConfigStore;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const OWNER_ID: i64 = 123_456_789;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-0.17.toml")
}

fn reference_doc() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/install-headless.md")
}

/// Les clés du bloc généré `reference:config` d'`install-headless.md`, que le test `docs`
/// de `penelope-evals` tient égal aux structures de `config.rs`.
fn reference_keys() -> BTreeSet<String> {
    let path = reference_doc();
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()));
    let start = doc
        .find("<!-- reference:config:debut")
        .expect("début du bloc reference:config");
    let end = doc
        .find("<!-- reference:config:fin -->")
        .expect("fin du bloc reference:config");
    let re = regex::Regex::new(r"^\| `([^`]+)` \|").unwrap();
    doc[start..end]
        .lines()
        .filter_map(|l| re.captures(l).map(|c| c[1].to_string()))
        .collect()
}

/// Les clés feuilles d'un document TOML, en `a.b.c` ; une liste est une feuille.
fn leaf_keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in table {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => leaf_keys(t, &path, out),
            _ => out.push(path),
        }
    }
}

/// Ramène une clé du fichier à sa forme de référence : le segment libre d'une table à
/// clés libres (`providers.extra.<nom>`, `context.model_thresholds.<nom>`) devient
/// `<nom>`. Une clé sans correspondance reste telle quelle, et sera nommée.
fn normalise(key: &str, reference: &BTreeSet<String>) -> String {
    if reference.contains(key) {
        return key.to_string();
    }
    let segs: Vec<&str> = key.split('.').collect();
    reference
        .iter()
        .filter(|r| r.contains("<nom>"))
        .find(|r| {
            let rs: Vec<&str> = r.split('.').collect();
            rs.len() == segs.len() && rs.iter().zip(&segs).all(|(a, b)| *a == "<nom>" || a == b)
        })
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Le fichier complet se charge par l'API publique, sans clé inconnue, valide, sans
/// contradiction ; ses valeurs se relisent ; la lecture ne le réécrit pas.
#[test]
fn the_complete_0_17_configuration_loads_without_error_or_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::copy(fixture(), &path).unwrap();

    let store = ConfigStore::load_or_create(&path, None, Arc::new(TestClock::default()), OWNER_ID)
        .unwrap_or_else(|e| panic!("config-0.17.toml : {e}"));
    assert!(
        store.unknown_keys().is_empty(),
        "clés inconnues de cette version : {:?}",
        store.unknown_keys()
    );
    let cfg = store.config();
    cfg.validate()
        .unwrap_or_else(|e| panic!("config-0.17.toml : validation : {e}"));
    let found = contradictions(&cfg);
    let refused: Vec<_> = found
        .iter()
        .filter(|c| c.gravity == Gravity::Refus)
        .collect();
    assert!(refused.is_empty(), "refus de cohérence : {refused:?}");
    assert!(found.is_empty(), "avertissements de cohérence : {found:?}");

    assert_eq!(cfg.owner.telegram_user_id, OWNER_ID);
    assert_eq!(cfg.telegram.allowed_chats, vec![-1_001_234_567_890]);
    assert_eq!(cfg.telegram.home.chat, -1_001_234_567_890);
    assert!(cfg.providers.local.enabled);
    assert!(cfg.providers.extra.contains_key("ollama"));
    assert_eq!(
        cfg.context
            .model_thresholds
            .get("openrouter:deepseek/deepseek-v4-pro"),
        Some(&0.6)
    );
    assert_eq!(cfg.observability.runtime_consumers.len(), 1);
    assert_eq!(cfg.observability.runtime_consumers[0].name, "tableau");
    assert_eq!(cfg.tools.shell_allow, vec!["cargo test", "npm run lint"]);
    assert_eq!(cfg.tools.http_allowlist, vec!["api.github.com"]);
    assert_eq!(cfg.models.roles.len(), 11);
    assert_eq!(cfg.models.aliases.len(), 9);

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        std::fs::read_to_string(fixture()).unwrap(),
        "la lecture ne réécrit pas le fichier"
    );
}

/// Le fichier porte chaque clé de la référence, et rien d'autre : une clé qui apparaît
/// dans `config.rs` (donc dans la référence, par le test `docs`) sans être ajoutée ici
/// est nommée ; une clé retirée aussi.
#[test]
fn the_fixture_carries_every_key_of_the_reference() {
    let reference = reference_keys();
    assert!(
        reference.len() > 200,
        "référence lue : {} clés seulement",
        reference.len()
    );

    let raw = std::fs::read_to_string(fixture()).unwrap();
    let table: toml::Table = toml::from_str(&raw).unwrap();
    let mut leaves = Vec::new();
    leaf_keys(&table, "", &mut leaves);
    let file: BTreeSet<String> = leaves.iter().map(|k| normalise(k, &reference)).collect();

    let missing: Vec<&String> = reference.difference(&file).collect();
    let extra: Vec<&String> = file.difference(&reference).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "config-0.17.toml ne suit pas la référence de docs/install-headless.md :\n  \
         absentes du fichier : {missing:?}\n  inconnues de la référence : {extra:?}"
    );
    assert_eq!(
        leaves.len(),
        reference.len(),
        "{} clés dans le fichier, {} dans la référence",
        leaves.len(),
        reference.len()
    );
}
