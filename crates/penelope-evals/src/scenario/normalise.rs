//! Normalisation du monde et des requêtes : identifiants, horodatages, chemins et
//! hachages remplacés par des jetons stables entre deux rejeux.
//!
//! - un identifiant ULID préfixé (`s_`, `t_`, `e_`, `a_`, `art_`, `n_`, `q_`, `r_`, `d_`)
//!   devient `{{session:1}}`, `{{turn:1}}`, `{{effect:1}}`, `{{approval:1}}`,
//!   `{{artifact:1}}`, `{{node:1}}`, `{{llm:1}}`, `{{run:1}}`, `{{dream:1}}`, numéroté
//!   dans l'ordre de première apparition ; un autre préfixe garde son nom (`sch_…` en
//!   `{{sch:1}}`) ; un ULID nu devient `{{ulid:1}}` ;
//! - la version du workspace devient `{{version}}` : un bump ne réécrit pas les attendus ;
//! - un ULID en minuscules, marqueur tiré au sort (balise `<commande-…>` du juge des
//!   commandes shell), devient `{{marker:1}}` ;
//! - le fichier de notes d'une session (`notes/<titre>-<6 derniers caractères de son
//!   identifiant, en minuscules>.md`, `session_notes`) devient `<titre>-{{session:1}}.md` ;
//! - un horodatage RFC 3339 devient `{{ts}}` à l'instant de départ du scénario, sinon
//!   `{{ts+600s}}` ou `{{ts+1500ms}}` : l'horloge de test rend l'écart exact ;
//! - la racine temporaire des services (brute et canonique) devient `{{home}}` ;
//! - un hachage (clé `*hash*`, `sha256`, `idem_key`, ou 64 hexadécimaux dans un texte)
//!   devient `{{hash}}` ; une durée mesurée (`duration_ms`, `durationMs`) devient `{{ms}}` ;
//! - le jeton d'un bouton Telegram (`callback_data`) devient `{{action}}` : il est tiré
//!   au hasard à chaque envoi ;
//! - un texte qui répond à un motif `masks` du scénario (mémoire du processus, contrôles
//!   propres à l'hôte) devient `{{masked}}` ;
//! - une estimation de jetons (`tokens_est`) devient `{{tokens}}` quand l'objet qui la
//!   porte cite la racine temporaire : sa longueur change d'une machine à l'autre.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Clés dont la valeur est un hachage, quelle que soit sa forme.
const HASH_KEYS: &[&str] = &[
    "hash",
    "prev_hash",
    "system_hash",
    "tools_hash",
    "request_hash",
    "body_hash",
    "idem_key",
    "fingerprint",
    "sha256",
];

/// Clés mesurées sur l'horloge murale, jamais reproductibles.
const DURATION_KEYS: &[&str] = &[
    "duration_ms",
    // Résultat de `shell_exec`.
    "durationMs",
    "elapsed_ms",
    "latency_ms",
    // Réponses RPC : essai d'un serveur MCP, latences mesurées, instantané de la base.
    "ms",
    "p50_ms",
    "p95_ms",
    "snapshot_ms",
];

/// Clés comptées sur un texte qui peut contenir la racine temporaire.
const SIZE_KEYS: &[&str] = &["tokens_est"];

/// Clés dont la valeur est un jeton aléatoire.
const RANDOM_KEYS: &[&str] = &["callback_data"];

pub struct Normaliser {
    start_ms: i64,
    homes: Vec<String>,
    ids: HashMap<String, String>,
    counters: HashMap<String, usize>,
    /// Suffixes de fichiers de notes de session (`-xa9cdw.md`) et leur jeton.
    note_suffixes: Vec<(String, String)>,
    ulid: Regex,
    marker: Regex,
    /// Une durée mesurée dans un JSON rendu en texte (résultat d'outil relu par le modèle),
    /// une ou plusieurs fois échappé.
    duration: Regex,
    stamp: Regex,
    hex: Regex,
    masks: Vec<Regex>,
}

impl Normaliser {
    pub fn new(start_ms: i64, root: &Path) -> Self {
        let mut homes = vec![root.to_string_lossy().to_string()];
        if let Ok(canonical) = std::fs::canonicalize(root) {
            let c = canonical.to_string_lossy().to_string();
            if !homes.contains(&c) {
                homes.push(c);
            }
        }
        // Le plus long d'abord : la forme canonique contient parfois la forme brute.
        homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
        Normaliser {
            start_ms,
            homes,
            ids: HashMap::new(),
            counters: HashMap::new(),
            note_suffixes: Vec::new(),
            marker: Regex::new(r"\b[0-9a-hjkmnp-tv-z]{26}\b").expect("regex marqueur"),
            duration: Regex::new(&format!(
                r#"(\\*")({})(\\*":\s*)\d+"#,
                DURATION_KEYS.join("|")
            ))
            .expect("regex durée"),
            ulid: Regex::new(r"\b(?:([a-z]+)_)?([0-9A-HJKMNP-TV-Z]{26})\b").expect("regex ULID"),
            stamp: Regex::new(
                r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})",
            )
            .expect("regex RFC 3339"),
            hex: Regex::new(r"\b[0-9a-f]{64}\b").expect("regex sha256"),
            masks: Vec::new(),
        }
    }

    /// Motifs `masks` du scénario, appliqués avant tout autre remplacement.
    pub fn with_masks(mut self, masks: &[String]) -> anyhow::Result<Self> {
        for m in masks {
            self.masks
                .push(Regex::new(m).map_err(|e| anyhow::anyhow!("motif `{m}` : {e}"))?);
        }
        Ok(self)
    }

    /// Normalise une valeur JSON entière.
    pub fn value(&mut self, v: Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.text(&s)),
            Value::Array(items) => Value::Array(items.into_iter().map(|x| self.value(x)).collect()),
            Value::Object(map) => {
                let mentions_home = self.mentions_home(&map);
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    let v = if HASH_KEYS.contains(&k.as_str()) && v.is_string() {
                        Value::String("{{hash}}".into())
                    } else if RANDOM_KEYS.contains(&k.as_str()) && v.is_string() {
                        Value::String("{{action}}".into())
                    } else if DURATION_KEYS.contains(&k.as_str()) && v.is_number() {
                        Value::String("{{ms}}".into())
                    } else if mentions_home && SIZE_KEYS.contains(&k.as_str()) && v.is_number() {
                        Value::String("{{tokens}}".into())
                    } else {
                        self.value(v)
                    };
                    out.insert(k, v);
                }
                Value::Object(out)
            }
            other => other,
        }
    }

    /// Vrai si l'objet cite la racine temporaire, dont la longueur dépend de la machine
    /// (`/tmp` sous Linux, `/var/folders/…` sous macOS).
    fn mentions_home(&self, map: &serde_json::Map<String, Value>) -> bool {
        let raw = Value::Object(map.clone()).to_string();
        self.homes
            .iter()
            .any(|h| !h.is_empty() && raw.contains(h.as_str()))
    }

    /// Normalise un texte : chemins, identifiants, horodatages, hachages.
    pub fn text(&mut self, s: &str) -> String {
        let mut out = s.to_string();
        for m in &self.masks {
            out = m.replace_all(&out, "{{masked}}").into_owned();
        }
        for home in &self.homes {
            if !home.is_empty() {
                out = out.replace(home, "{{home}}");
            }
        }
        out = out.replace(env!("CARGO_PKG_VERSION"), "{{version}}");
        let ulid = self.ulid.clone();
        let out = ulid
            .replace_all(&out, |caps: &regex::Captures| {
                let kind = match caps.get(1).map(|m| m.as_str()) {
                    Some("art") => "artifact",
                    Some("s") => "session",
                    Some("t") => "turn",
                    Some("e") => "effect",
                    Some("a") => "approval",
                    Some("n") => "node",
                    Some("q") => "llm",
                    Some("r") => "run",
                    Some("d") => "dream",
                    Some(other) => other,
                    None => "ulid",
                }
                .to_string();
                self.token(&kind, &caps[0])
            })
            .into_owned();
        let marker = self.marker.clone();
        let mut out = marker
            .replace_all(&out, |caps: &regex::Captures| {
                self.token("marker", &caps[0])
            })
            .into_owned();
        for (suffix, token) in &self.note_suffixes {
            out = out.replace(suffix.as_str(), token.as_str());
        }
        let out = self
            .duration
            .replace_all(&out, "$1$2$3$1{{ms}}$1")
            .into_owned();
        let stamp = self.stamp.clone();
        let out = stamp
            .replace_all(&out, |caps: &regex::Captures| self.stamp_token(&caps[0]))
            .into_owned();
        self.hex.replace_all(&out, "{{hash}}").into_owned()
    }

    fn token(&mut self, kind: &str, raw: &str) -> String {
        if let Some(t) = self.ids.get(raw) {
            return t.clone();
        }
        let n = self.counters.entry(kind.to_string()).or_insert(0);
        *n += 1;
        let t = format!("{{{{{kind}:{n}}}}}");
        self.ids.insert(raw.to_string(), t.clone());
        if kind == "session" {
            // `penelope_vault::session_notes` nomme le fichier par la fin de l'identifiant.
            let tail: String = raw
                .chars()
                .skip(raw.chars().count().saturating_sub(6))
                .collect();
            self.note_suffixes
                .push((format!("-{}.md", tail.to_lowercase()), format!("-{t}.md")));
        }
        t
    }

    fn stamp_token(&self, raw: &str) -> String {
        let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) else {
            return "{{ts?}}".into();
        };
        let delta = parsed.timestamp_millis() - self.start_ms;
        match delta {
            0 => "{{ts}}".into(),
            d if d % 1000 == 0 => format!("{{{{ts{:+}s}}}}", d / 1000),
            d => format!("{{{{ts{d:+}ms}}}}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const START: i64 = 1_767_225_600_000;

    #[test]
    fn identifiers_are_numbered_in_order_of_appearance() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-x"));
        let a = "s_01JZZZZZZZZZZZZZZZZZZZZZZZ";
        let b = "s_01JAAAAAAAAAAAAAAAAAAAAAAA";
        let t = "t_01JBBBBBBBBBBBBBBBBBBBBBBB";
        assert_eq!(
            n.text(&format!("{b} {a} {b}")),
            "{{session:1}} {{session:2}} {{session:1}}"
        );
        assert_eq!(
            n.text(&format!("turn.intents.{t}")),
            "turn.intents.{{turn:1}}"
        );
        assert_eq!(n.text("art_01JCCCCCCCCCCCCCCCCCCCCCCC"), "{{artifact:1}}");
        assert_eq!(n.text("d_01JEEEEEEEEEEEEEEEEEEEEEEE"), "{{dream:1}}");
        assert_eq!(n.text("01JDDDDDDDDDDDDDDDDDDDDDDD"), "{{ulid:1}}");
        assert_eq!(n.text("sch_01JEEEEEEEEEEEEEEEEEEEEEEE"), "{{sch:1}}");
        assert_eq!(
            n.text(concat!("v", env!("CARGO_PKG_VERSION"))),
            "v{{version}}"
        );
        // Un préfixe court collé par `_` n'est jamais laissé en clair ; il garde son nom.
        assert_eq!(
            n.text("i_01JEEEEEEEEEEEEEEEEEEEEEEE tj_01JFFFFFFFFFFFFFFFFFFFFFFF"),
            "{{i:1}} {{tj:1}}"
        );
        assert_eq!(n.text("zz_01JGGGGGGGGGGGGGGGGGGGGGGG"), "{{zz:1}}");
        assert_eq!(
            n.text("shell-01JHHHHHHHHHHHHHHHHHHHHHHH"),
            "shell-{{ulid:2}}"
        );
        assert_eq!(n.text("rien à voir"), "rien à voir");
    }

    #[test]
    fn a_lowercase_marker_is_masked() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-u"));
        assert_eq!(
            n.text("<commande-01m3f01ve1mdke4h36yc7xbz2q>"),
            "<commande-{{marker:1}}>"
        );
        assert_eq!(n.text("intracommunautaire"), "intracommunautaire");
    }

    #[test]
    fn a_session_notes_file_is_named_by_its_session_token() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-v"));
        let s = "s_01JZZZZZZZZZZZZZZZZZXA9CDW";
        assert_eq!(n.text(s), "{{session:1}}");
        assert_eq!(n.text("notes/cli-xa9cdw.md"), "notes/cli-{{session:1}}.md");
        assert_eq!(n.text("notes/cli-abcdef.md"), "notes/cli-abcdef.md");
    }

    #[test]
    fn stamps_become_offsets_from_the_start() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-y"));
        assert_eq!(n.text("à 2026-01-01T00:00:00.000Z"), "à {{ts}}");
        assert_eq!(n.text("2026-01-01T00:10:00.000Z"), "{{ts+600s}}");
        assert_eq!(n.text("2026-01-01T00:00:01.500Z"), "{{ts+1500ms}}");
        assert_eq!(n.text("2025-12-31T23:59:59Z"), "{{ts-1s}}");
        assert_eq!(n.text("2026-01-01T02:00:00+02:00"), "{{ts}}");
    }

    #[test]
    fn paths_hashes_and_durations_are_masked() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-z"));
        let v = n.value(json!({
            "path": "/tmp/racine-z/data/workspace/a.txt",
            "system_hash": "abc",
            "sha256": "def",
            "duration_ms": 12,
            "durationMs": 44,
            "texte": format!("empreinte {}", "0".repeat(64)),
            "n": 3,
        }));
        assert_eq!(v["path"], "{{home}}/data/workspace/a.txt");
        assert_eq!(v["system_hash"], "{{hash}}");
        assert_eq!(v["sha256"], "{{hash}}");
        assert_eq!(v["duration_ms"], "{{ms}}");
        assert_eq!(v["durationMs"], "{{ms}}");
        // La même durée, dans un JSON rendu en texte (job relu par le modèle).
        assert_eq!(
            n.text("{\n  \"durationMs\": 22,\n  \"exitCode\": 0\n}"),
            "{\n  \"durationMs\": \"{{ms}}\",\n  \"exitCode\": 0\n}"
        );
        // Échappée une fois de plus (résultat d'un job rendu dans le résultat de job_wait).
        assert_eq!(
            n.text(r#"{"text": "{\"durationMs\": 2014}"}"#),
            r#"{"text": "{\"durationMs\": \"{{ms}}\"}"}"#
        );
        assert_eq!(v["texte"], "empreinte {{hash}}");
        assert_eq!(v["n"], 3);
    }

    #[test]
    fn button_tokens_and_masked_texts_are_replaced() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-v"))
            .with_masks(&[r"Mémoire : \d+ Mo".into()])
            .unwrap();
        let v = n.value(json!({"callback_data": "a:oOC3riTSHISF", "text": "• Mémoire : 35 Mo"}));
        assert_eq!(v["callback_data"], "{{action}}");
        assert_eq!(v["text"], "• {{masked}}");
    }

    #[test]
    fn a_token_estimate_is_masked_only_next_to_the_home() {
        let mut n = Normaliser::new(START, Path::new("/tmp/racine-w"));
        let near = n.value(json!({"text": "/tmp/racine-w/a.txt", "tokens_est": 93}));
        let far = n.value(json!({"text": "rien", "tokens_est": 12}));
        assert_eq!(near["tokens_est"], "{{tokens}}");
        assert_eq!(far["tokens_est"], 12);
    }
}
