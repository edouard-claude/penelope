//! Redaction des secrets dans les logs, événements, résumés et messages (§13.1).
//!
//! Deux niveaux :
//! - **motifs génériques** (clés, bearer, JWT, clés privées, numéros de carte) ;
//! - **valeurs connues** enregistrées par le `SecretStore` : un secret déjà chargé en
//!   mémoire est masqué même s'il ne correspond à aucun motif.

use regex::Regex;
use std::sync::{Arc, OnceLock, RwLock};

pub const MASK: &str = "[secret masqué]";

struct Patterns {
    rules: Vec<(Regex, &'static str)>,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let mut rules: Vec<(Regex, &'static str)> = Vec::new();
        let mut add = |re: &str, label: &'static str| {
            if let Ok(r) = Regex::new(re) {
                rules.push((r, label));
            }
        };
        // Clés privées PEM (bloc entier).
        add(
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            "clé privée",
        );
        // En-têtes d'autorisation.
        add(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{12,}", "bearer");
        // JWT.
        add(
            r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\b",
            "jwt",
        );
        // Clés de fournisseurs usuels.
        add(r"\bsk-[A-Za-z0-9_-]{16,}\b", "clé api");
        add(r"\bsk-or-v1-[A-Za-z0-9]{16,}\b", "clé openrouter");
        add(r"\bgh[pousr]_[A-Za-z0-9]{16,}\b", "jeton github");
        add(r"\bglpat-[A-Za-z0-9_-]{16,}\b", "jeton gitlab");
        add(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b", "jeton slack");
        add(r"\bAKIA[0-9A-Z]{16}\b", "clé aws");
        // Jeton de bot Telegram : <digits>:<35 chars>.
        add(r"\b\d{6,12}:[A-Za-z0-9_-]{30,}\b", "jeton telegram");
        // Le même, collé au préfixe `bot` d'une URL de la Bot API (issue #26).
        add(r"bot\d{6,12}:[A-Za-z0-9_-]{30,}", "jeton telegram");
        // Affectation explicite d'un secret dans un texte de configuration.
        add(
            r#"(?i)\b(api[_-]?key|secret|password|passwd|token|private[_-]?key)\b\s*[:=]\s*["']?[^\s"',]{8,}"#,
            "affectation de secret",
        );
        Patterns { rules }
    })
}

/// Valeurs exactes à masquer, alimentées par le `SecretStore` au chargement.
static KNOWN: OnceLock<RwLock<Vec<Arc<str>>>> = OnceLock::new();

fn known() -> &'static RwLock<Vec<Arc<str>>> {
    KNOWN.get_or_init(|| RwLock::new(Vec::new()))
}

/// Enregistre une valeur de secret à masquer partout. Les valeurs de moins de
/// 6 caractères sont ignorées (trop de faux positifs).
pub fn register_secret(value: &str) {
    if value.len() < 6 {
        return;
    }
    if let Ok(mut g) = known().write()
        && !g.iter().any(|v| &**v == value)
    {
        g.push(Arc::from(value));
    }
}

pub fn forget_secret(value: &str) {
    if let Ok(mut g) = known().write() {
        g.retain(|v| &**v != value);
    }
}

pub fn registered_count() -> usize {
    known().read().map(|g| g.len()).unwrap_or(0)
}

/// Masque tout secret détecté. Idempotent : appliquer deux fois donne le même texte.
pub fn redact(input: &str) -> String {
    let mut out = input.to_string();

    if let Ok(g) = known().read() {
        for v in g.iter() {
            if out.contains(&**v) {
                out = out.replace(&**v, MASK);
            }
        }
    }

    for (re, _label) in &patterns().rules {
        if re.is_match(&out) {
            out = re.replace_all(&out, MASK).into_owned();
        }
    }
    redact_card_numbers(&out)
}

/// Numéros de carte : détectés par longueur **et** clé de Luhn, pour ne pas masquer
/// un identifiant numérique quelconque.
fn redact_card_numbers(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"\b(?:\d[ -]?){12,18}\d\b").expect("motif de carte valide"));
    re.replace_all(s, |c: &regex::Captures<'_>| {
        let raw = &c[0];
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        if (13..=19).contains(&digits.len()) && luhn(&digits) {
            MASK.to_string()
        } else {
            raw.to_string()
        }
    })
    .into_owned()
}

/// Clé de Luhn.
pub fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for c in digits.chars().rev() {
        let Some(d) = c.to_digit(10) else {
            return false;
        };
        let v = if double {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        sum += v;
        double = !double;
    }
    sum.is_multiple_of(10)
}

/// Vrai si le texte contient quelque chose qui ressemble à un secret. Utilisé par le
/// filtre d'écriture mémoire (§6.10), qui **refuse** l'écriture au lieu de masquer.
pub fn contains_secret(input: &str) -> bool {
    if let Ok(g) = known().read()
        && g.iter().any(|v| input.contains(&**v))
    {
        return true;
    }
    if patterns().rules.iter().any(|(re, _)| re.is_match(input)) {
        return true;
    }
    redact_card_numbers(input) != input
}

/// Secret laissé en clair dans un journal : valeur enregistrée ou jeton reconnaissable.
/// Les affectations génériques (`token = …`) sont ignorées, trop fréquentes dans les
/// traces légitimes.
pub fn leaked_secret_kind(line: &str) -> Option<&'static str> {
    if let Ok(g) = known().read()
        && g.iter().any(|v| line.contains(&**v))
    {
        return Some("secret enregistré");
    }
    patterns()
        .rules
        .iter()
        .filter(|(_, label)| *label != "affectation de secret")
        .find(|(re, _)| re.is_match(line))
        .map(|(_, label)| *label)
}

/// Nature du secret détecté, pour le message d'erreur du filtre d'écriture.
pub fn secret_kind(input: &str) -> Option<&'static str> {
    for (re, label) in &patterns().rules {
        if re.is_match(input) {
            return Some(label);
        }
    }
    if redact_card_numbers(input) != input {
        return Some("numéro de carte");
    }
    None
}

/// Redaction récursive d'une valeur JSON (payloads d'événements, arguments d'outils).
pub fn redact_json(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(redact_json).collect()),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                let sensitive = matches!(
                    k.to_ascii_lowercase().as_str(),
                    "password"
                        | "passwd"
                        | "secret"
                        | "token"
                        | "api_key"
                        | "apikey"
                        | "authorization"
                        | "private_key"
                        | "client_secret"
                        | "access_token"
                        | "refresh_token"
                );
                out.insert(
                    k.clone(),
                    if sensitive && val.is_string() {
                        Value::String(MASK.into())
                    } else {
                        redact_json(val)
                    },
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_bearer_and_jwt() {
        let s = "Authorization: Bearer abcdefghijklmnop1234";
        assert!(!redact(s).contains("abcdefghijklmnop"));
        let jwt = "eyJhbGciOi.eyJzdWIiOjE.SflKxwRJSMeKK";
        assert_eq!(redact(jwt), MASK);
    }

    #[test]
    fn masks_provider_keys() {
        for k in [
            "sk-or-v1-0123456789abcdef0123456789abcdef",
            "ghp_0123456789abcdef0123456789abcdef",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-1234567890-abcdefghij",
        ] {
            let out = redact(&format!("clé = {k} fin"));
            assert!(!out.contains(k), "non masqué : {k} → {out}");
        }
    }

    #[test]
    fn masks_telegram_bot_token() {
        let t = "123456789:AAH-abcdefghijklmnopqrstuvwxyz012345";
        assert!(!redact(t).contains("AAH-"));
    }

    #[test]
    fn card_number_uses_luhn() {
        // Numéro de test Visa valide au sens de Luhn.
        assert_eq!(
            redact("carte 4111 1111 1111 1111 fin"),
            format!("carte {MASK} fin")
        );
        // Une suite de chiffres qui ne passe pas Luhn n'est pas masquée.
        let ident = "1234567890123456";
        assert!(!luhn(ident));
        assert!(redact(&format!("ref {ident}")).contains(ident));
    }

    #[test]
    fn registered_secret_is_masked_even_without_pattern() {
        register_secret("motdepasseordinaire");
        let out = redact("le mot est motdepasseordinaire ici");
        assert!(!out.contains("motdepasseordinaire"), "{out}");
        forget_secret("motdepasseordinaire");
    }

    #[test]
    fn short_values_are_not_registered() {
        let before = registered_count();
        register_secret("abc");
        assert_eq!(registered_count(), before);
    }

    #[test]
    fn redaction_is_idempotent() {
        let s = "Bearer abcdefghijklmnop1234";
        let a = redact(s);
        assert_eq!(redact(&a), a);
    }

    #[test]
    fn json_sensitive_keys_are_masked() {
        let v = json!({"api_key":"quelquechose","nested":{"token":"xyz"},"ok":"visible"});
        let r = redact_json(&v);
        assert_eq!(r["api_key"], MASK);
        assert_eq!(r["nested"]["token"], MASK);
        assert_eq!(r["ok"], "visible");
    }

    #[test]
    fn contains_secret_detects_card_and_key() {
        assert!(contains_secret("ma carte 4111111111111111"));
        assert!(contains_secret("sk-0123456789abcdefgh"));
        assert!(!contains_secret("une phrase tout à fait banale"));
        assert_eq!(
            secret_kind("ma carte 4111111111111111"),
            Some("numéro de carte")
        );
    }

    /// CA 13 : un secret planté n'apparaît dans aucun log, événement ou message.
    #[test]
    fn ca_13_2_planted_secret_never_leaks() {
        let secret = "sk-or-v1-deadbeefdeadbeefdeadbeefdeadbeef";
        register_secret(secret);
        let event = json!({
            "tool": "http_fetch",
            "args": {"headers": {"Authorization": format!("Bearer {secret}")}},
            "log": format!("appel avec {secret}"),
        });
        let out = serde_json::to_string(&redact_json(&event)).unwrap();
        assert!(!out.contains("deadbeef"), "{out}");
        forget_secret(secret);
    }
}
