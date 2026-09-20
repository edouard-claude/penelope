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
        add(
            r"(?i)\b(?:bearer|basic)\s+(?P<v>[A-Za-z0-9._~+/=-]{12,})",
            "bearer",
        );
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
        // Clés secrètes et restreintes Stripe (`sk_test_…`, `rk_live_…`).
        add(r"\b[sr]k_(?:test|live)_[A-Za-z0-9]{10,}\b", "clé stripe");
        // Jeton de bot Telegram : <digits>:<35 chars>.
        add(r"\b\d{6,12}:[A-Za-z0-9_-]{30,}\b", "jeton telegram");
        // Le même, collé au préfixe `bot` d'une URL de la Bot API (issue #26).
        add(r"bot\d{6,12}:[A-Za-z0-9_-]{30,}", "jeton telegram");
        // Affectation explicite d'un secret dans un texte de configuration ou du code :
        // `password = x`, `"password": "x"`, `'x-api-key': 'x'`, `KEY = "x"` (issue #134).
        add(
            r#"(?i)\b(?:(?:x[_-])?api[_-]?key|apikey|key|client[_-]?secret|secret|password|passwd|pwd|access[_-]?token|auth[_-]?token|token|private[_-]?key)\b["']?\s*[:=]\s*["']?(?P<v>[^\s"',)}]{8,})"#,
            "affectation de secret",
        );
        Patterns { rules }
    })
}

/// Référence à un secret rangé, `${SECRET:nom}` : un nom, jamais une valeur.
fn reference_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\$\{SECRET:[A-Za-z0-9_.-]+\}").expect("motif de référence valide")
    })
}

/// Le texte, références `${SECRET:nom}` blanchies à longueur égale : les positions restent
/// valables et une référence n'est jamais prise pour un secret (issue #37).
fn without_references(input: &str) -> std::borrow::Cow<'_, str> {
    if !input.contains("${SECRET:") {
        return std::borrow::Cow::Borrowed(input);
    }
    std::borrow::Cow::Owned(
        reference_re()
            .replace_all(input, |c: &regex::Captures<'_>| " ".repeat(c[0].len()))
            .into_owned(),
    )
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

/// Valeurs apprises dans ce que l'agent a lu (issue #134), bornées : les plus anciennes
/// cèdent la place, jamais un secret enregistré.
static LEARNED: OnceLock<RwLock<std::collections::VecDeque<Arc<str>>>> = OnceLock::new();
const LEARNED_MAX: usize = 2_000;

fn learned() -> &'static RwLock<std::collections::VecDeque<Arc<str>>> {
    LEARNED.get_or_init(|| RwLock::new(std::collections::VecDeque::new()))
}

/// Secrets enregistrés et valeurs apprises, à masquer ou à reconnaître.
fn known_values() -> Vec<Arc<str>> {
    let mut v: Vec<Arc<str>> = known().read().map(|g| g.clone()).unwrap_or_default();
    if let Ok(g) = learned().read() {
        v.extend(g.iter().cloned());
    }
    v
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
    // Les références `${SECRET:nom}` traversent la redaction intactes.
    if input.contains("${SECRET:") {
        let mut out = String::with_capacity(input.len());
        let mut last = 0;
        for m in reference_re().find_iter(input) {
            out.push_str(&redact(&input[last..m.start()]));
            out.push_str(m.as_str());
            last = m.end();
        }
        out.push_str(&redact(&input[last..]));
        return out;
    }
    let mut out = input.to_string();

    for v in known_values() {
        if out.contains(&*v) {
            out = out.replace(&*v, MASK);
        }
    }

    for (re, _label) in &patterns().rules {
        if re.is_match(&out) {
            out = re.replace_all(&out, MASK).into_owned();
        }
    }
    redact_random_tokens(&redact_card_numbers(&out))
}

/// Jeton long et aléatoire (clé sans préfixe connu, recopiée d'un fichier) : 40
/// caractères au moins, trois familles de caractères, entropie élevée. Masqué dans les
/// journaux, les événements et les demandes stockées ; jamais dans ce qui s'exécute, et
/// pas dans le filtre d'écriture de la mémoire (#132). Un chemin ou un identifiant court
/// ne passent pas ces tests (issue #134).
///
/// Une longue suite **hexadécimale** est masquée à part : elle n'a que deux familles de
/// caractères, donc les règles ci-dessus la laissaient passer, et c'est sous cette forme
/// qu'un `Grant` de 4 Ko est parti en clair sur Telegram (issue #148). Seule exception,
/// une suite de **64 caractères exactement** : c'est une empreinte SHA-256, qui a sa place
/// dans un journal.
fn redact_random_tokens(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+=_*~!$-]{40,}").expect("motif de jeton valide"));
    re.replace_all(s, |c: &regex::Captures<'_>| {
        let t = &c[0];
        if looks_random(t) || looks_hex_blob(t) {
            MASK.to_string()
        } else {
            t.to_string()
        }
    })
    .into_owned()
}

/// Longueur d'une empreinte SHA-256 en hexadécimal : la seule suite hexadécimale longue
/// qu'on laisse passer.
const SHA256_HEX_LEN: usize = 64;

/// Suite hexadécimale assez longue pour être une valeur encodée, pas une empreinte.
fn looks_hex_blob(t: &str) -> bool {
    let n = t.len();
    // Strictement plus long qu'une empreinte : 64 caractères exactement restent lisibles.
    n > SHA256_HEX_LEN && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Trois familles parmi minuscules, majuscules, chiffres et symboles (hors `-` et `_`,
/// qui font les identifiants lisibles), et une entropie élevée. Un chemin, une URL ou un
/// nom en `snake_case` n'y ressemblent pas.
fn looks_random(t: &str) -> bool {
    let classes = [
        t.chars().any(|c| c.is_ascii_lowercase()),
        t.chars().any(|c| c.is_ascii_uppercase()),
        t.chars().any(|c| c.is_ascii_digit()),
        t.chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_'),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if classes < 3 {
        return false;
    }
    let mut counts = std::collections::HashMap::new();
    for c in t.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = t.chars().count() as f64;
    let entropy: f64 = counts
        .values()
        .map(|k| {
            let p = *k as f64 / n;
            -p * p.log2()
        })
        .sum();
    entropy >= 4.3
}

/// Retient comme secrets connus les valeurs repérées par motif dans un texte lu par
/// l'agent (fichier, sortie de commande) : recopiées ensuite dans une commande, un
/// message ou une demande, elles sont masquées partout où la rédaction passe (issue
/// #134). Rien n'est rangé ni écrit : la liste vit le temps du processus.
pub fn learn_secrets(text: &str) {
    let clean = &*without_references(text);
    for span in secret_spans(clean) {
        let value = &clean[span.start..span.end];
        if span.kind == "secret enregistré" || value.len() < 6 {
            continue;
        }
        if let Ok(mut g) = learned().write()
            && !g.iter().any(|v| &**v == value)
        {
            if g.len() >= LEARNED_MAX {
                g.pop_front();
            }
            g.push_back(Arc::from(value));
        }
    }
}

fn card_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:\d[ -]?){12,18}\d\b").expect("motif de carte valide"))
}

/// Numéros de carte : détectés par longueur **et** clé de Luhn, pour ne pas masquer
/// un identifiant numérique quelconque. Masquage des journaux et des événements (#26) :
/// tout nombre qui passe les deux tests est masqué, identifiant ou non ; un faux positif
/// y coûte peu, un faux négatif fuirait.
fn redact_card_numbers(s: &str) -> String {
    let re = card_re();
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

/// Premier numéro de carte d'un texte, pour le **refus d'écriture** (issue #132) : même
/// longueur, même clé de Luhn que le masquage, mais un nombre collé à un identifiant
/// (`command-output:38228-1743576040856618`, `id=…`, `run/…`, `…_x`) n'en est pas un,
/// sauf si le mot collé désigne une carte (`carte:…`, `cb=…`). Rend la plage du nombre.
fn card_to_refuse(input: &str) -> Option<std::ops::Range<usize>> {
    const GLUE: &[char] = &[':', '-', '_', '/', '=', '#', '@'];
    const CARD_WORDS: &[&str] = &[
        "carte",
        "card",
        "cb",
        "cc",
        "visa",
        "mastercard",
        "amex",
        "pan",
        "numero",
        "numéro",
    ];
    card_re().find_iter(input).find_map(|m| {
        let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
        if !(13..=19).contains(&digits.len()) || !luhn(&digits) {
            return None;
        }
        let before = input[..m.start()].chars().next_back();
        let after = input[m.end()..].chars().next();
        let glued = |c: Option<char>| c.is_some_and(|c| GLUE.contains(&c) || c.is_alphabetic());
        if glued(before) || glued(after) {
            // `carte:4539…` reste une carte : le mot collé la nomme.
            let word: String = input[..m.start()]
                .trim_end_matches(GLUE)
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
                .to_lowercase();
            if !CARD_WORDS.contains(&word.as_str()) {
                return None;
            }
        }
        Some(m.range())
    })
}

/// Fragment d'un secret détecté, masqué au milieu, pour qu'un refus dise quoi retirer
/// sans l'exposer (issue #132) : les quatre derniers chiffres d'une carte, les quatre
/// premiers caractères d'une clé.
pub fn secret_fragment(input: &str) -> Option<String> {
    let clean = &*without_references(input);
    if let Some(span) = secret_spans(clean).first() {
        let value = &clean[span.start..span.end];
        let head: String = value.chars().take(4).collect();
        return Some(format!("{head}…"));
    }
    card_to_refuse(clean).map(|r| {
        let digits: String = clean[r].chars().filter(|c| c.is_ascii_digit()).collect();
        format!("…{}", &digits[digits.len() - 4..])
    })
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
    let input = &*without_references(input);
    if known_values().iter().any(|v| input.contains(&**v)) {
        return true;
    }
    if patterns().rules.iter().any(|(re, _)| re.is_match(input)) {
        return true;
    }
    card_to_refuse(input).is_some()
}

/// Secret laissé en clair dans un journal : valeur enregistrée ou jeton reconnaissable.
/// Les affectations génériques (`token = …`) sont ignorées, trop fréquentes dans les
/// traces légitimes.
pub fn leaked_secret_kind(line: &str) -> Option<&'static str> {
    let line = &*without_references(line);
    if known_values().iter().any(|v| line.contains(&**v)) {
        return Some("secret enregistré");
    }
    patterns()
        .rules
        .iter()
        .filter(|(_, label)| *label != "affectation de secret")
        .find(|(re, _)| re.is_match(line))
        .map(|(_, label)| *label)
}

/// Secret resté en clair dans ce qui est stocké (demandes d'approbation, file Telegram) :
/// valeur connue, motif (affectations comprises) ou jeton long et aléatoire. Pour
/// `doctor` (issue #134).
pub fn stored_secret_kind(text: &str) -> Option<&'static str> {
    let text = &*without_references(text);
    if known_values().iter().any(|v| text.contains(&**v)) {
        return Some("secret enregistré");
    }
    if let Some((_, label)) = patterns().rules.iter().find(|(re, _)| re.is_match(text)) {
        return Some(label);
    }
    (redact_random_tokens(text) != text).then_some("jeton aléatoire")
}

/// Secret repéré dans un texte : position de la **valeur** (sans le mot-clé qui la
/// précède) et nature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSpan {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
}

/// Valeurs de secrets d'un texte, dans l'ordre, sans chevauchement (issue #37) : de quoi
/// ranger chaque valeur dans le magasin et ne garder en mémoire qu'une référence. Les
/// numéros de carte n'en font pas partie : ils ne se rangent pas, ils se refusent.
pub fn secret_spans(input: &str) -> Vec<SecretSpan> {
    let input = &*without_references(input);
    let mut found: Vec<SecretSpan> = Vec::new();
    // Motifs d'abord : à position égale, la nature reconnue l'emporte sur « secret
    // enregistré » (le tri qui suit est stable).
    for (re, label) in &patterns().rules {
        for c in re.captures_iter(input) {
            let m = c.name("v").or_else(|| c.get(0)).expect("capture complète");
            found.push(SecretSpan {
                start: m.start(),
                end: m.end(),
                kind: label,
            });
        }
    }
    {
        for v in known_values() {
            for (start, _) in input.match_indices(&*v) {
                found.push(SecretSpan {
                    start,
                    end: start + v.len(),
                    kind: "secret enregistré",
                });
            }
        }
    }
    // Le plus long l'emporte à position égale ; un secret contenu dans un autre disparaît.
    found.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<SecretSpan> = Vec::new();
    for f in found {
        match out.last_mut() {
            Some(last) if f.start < last.end => {
                last.end = last.end.max(f.end);
            }
            _ => out.push(f),
        }
    }
    out
}

/// Nature du secret détecté, pour le message d'erreur du filtre d'écriture.
pub fn secret_kind(input: &str) -> Option<&'static str> {
    let input = &*without_references(input);
    for (re, label) in &patterns().rules {
        if re.is_match(input) {
            return Some(label);
        }
    }
    if card_to_refuse(input).is_some() {
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

    /// #148 : le `Grant` Codex est parti en clair parce qu'il était **en hexadécimal** :
    /// deux familles de caractères seulement, donc invisible aux règles d'entropie. Une
    /// empreinte SHA-256, elle, reste lisible.
    #[test]
    fn a_long_hex_run_is_masked_but_a_sha256_digest_is_not() {
        let grant = "7b22616363657373".repeat(200);
        assert!(grant.len() > 3_000);
        let red = redact(&format!("security: unknown command \"{grant}"));
        assert!(
            !red.contains("7b22616363657373"),
            "hexadécimal en clair : {red}"
        );
        assert!(red.contains(MASK), "{red}");

        // 64 caractères exactement : une empreinte, on la garde.
        let digest = "a3f1".repeat(16);
        assert_eq!(digest.len(), 64);
        let line = format!("skill revue-de-code body_hash {digest}");
        assert_eq!(redact(&line), line, "une empreinte reste lisible");

        // 65 et plus : ce n'est plus une empreinte.
        let long = format!("{digest}b");
        assert!(redact(&long).contains(MASK), "{long}");

        // Ce qui n'est pas de l'hexadécimal n'est pas concerné par cette règle.
        let path = "/Users/edouard/Library/Application-Support/Penelope/secret-names.json";
        assert_eq!(redact(path), path);
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

    /// #134 : une clé sans préfixe connu, recopiée d'un fichier dans une commande, est
    /// masquée quand elle est stockée ou journalisée : par son affectation (`KEY = "…"`,
    /// `'x-api-key': '…'`, `"password": "…"`), par sa forme (jeton long et aléatoire), ou
    /// parce qu'elle a été lue plus tôt. Un chemin, une URL, un hachage, un identifiant
    /// lisible restent intacts.
    #[test]
    fn a_key_copied_from_a_file_is_masked_where_it_is_stored() {
        let key = "Zx9kQ2mV7pLr4TbW1nHs8YcD3fGa6JuE0oIq5*RtKyNw2BvXe7LmPz4SdHj1Ua";
        let command = format!("python3 - <<'EOF'\nKEY = \"{key}\"\nprint(1)\nEOF");
        assert!(!redact(&command).contains(key), "{}", redact(&command));
        let js = "headers: { 'x-api-key': 'dev-secret-aaaa1111' }";
        assert!(
            !redact(js).contains("dev-secret-aaaa1111"),
            "{}",
            redact(js)
        );
        let py = r#"login({"email": "a@b.fr", "password": "Motdepasse2026"})"#;
        assert!(!redact(py).contains("Motdepasse2026"), "{}", redact(py));
        // Une valeur lue plus tôt, recopiée seule, est masquée aussi.
        learn_secrets("const cfg = { apiKey: 'ab12cd34ef56gh78' };");
        assert!(!redact(r#"{"p": "ab12cd34ef56gh78"}"#).contains("ab12cd34ef56gh78"));
        for kept in [
            "https://github.com/edouard-claude/penelope/releases/tag/v0.17.10",
            "/Users/essai/Code/agent/penelope/crates/penelope-daemon/src/executor.rs",
            "a_failing_command_of_41_lines_is_returned_whole_and_more",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "s_01M2TAQFBTN50F030S73RWZFME",
        ] {
            assert_eq!(redact(kept), kept, "{kept}");
        }
    }

    /// #132 : un nombre collé à un identifiant n'est pas une carte pour le filtre
    /// d'écriture ; une vraie carte, isolée ou nommée, l'est toujours ; les journaux
    /// masquent toujours tout nombre qui passe Luhn et la longueur (#26).
    #[test]
    fn a_number_inside_an_identifier_is_not_a_card() {
        let id = "command-output:38228-1743576040856618";
        assert!(luhn("1743576040856618"), "le cas vécu passe bien Luhn");
        assert!(!contains_secret(id), "{id}");
        assert_eq!(secret_kind(id), None);
        assert_eq!(secret_kind("id=4539148803436467"), None);
        assert_eq!(secret_kind("run/4539148803436467/log"), None);
        for card in [
            "4539 1488 0343 6467",
            "ma carte 4539148803436467.",
            "carte 4539148803436467",
            "carte:4539148803436467",
            "cb=4539-1488-0343-6467",
        ] {
            assert_eq!(secret_kind(card), Some("numéro de carte"), "{card}");
        }
        assert_eq!(
            secret_fragment("ma carte 4539 1488 0343 6467").as_deref(),
            Some("…6467")
        );
        assert_eq!(
            secret_fragment("clé sk-0123456789abcdefgh").as_deref(),
            Some("sk-0…")
        );
        assert!(redact(id).contains(MASK), "journaux : masqué quand même");
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

    #[test]
    fn secret_spans_point_at_values_only() {
        let t = "Stripe de test : sk_test_FauxCle0123456, et password: Hunter2Hunter2 ; \
                 jeton ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.";
        let spans = secret_spans(t);
        let values: Vec<&str> = spans.iter().map(|s| &t[s.start..s.end]).collect();
        assert_eq!(
            values,
            vec![
                "sk_test_FauxCle0123456",
                "Hunter2Hunter2",
                "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ]
        );
        assert_eq!(spans[0].kind, "clé stripe");
        // Clé OpenRouter : deux motifs, une seule valeur.
        let k = "clé sk-or-v1-0123456789abcdef0123456789";
        assert_eq!(secret_spans(k).len(), 1);
        assert!(secret_spans("carte 4111 1111 1111 1111").is_empty());
        assert!(secret_spans("client cus_NffrFeUfNV2Hib").is_empty());
        // Une référence à un secret rangé n'est pas un secret, et survit à la redaction.
        let r = "clé Stripe : ${SECRET:cle-stripe-test-1f2e3d4c}, password: Hunter2Hunter2";
        assert_eq!(secret_kind("${SECRET:cle-stripe-test-1f2e3d4c}"), None);
        assert!(!contains_secret("${SECRET:cle-stripe-test-1f2e3d4c}"));
        let spans = secret_spans(r);
        assert_eq!(spans.len(), 1);
        assert_eq!(&r[spans[0].start..spans[0].end], "Hunter2Hunter2");
        assert_eq!(
            redact(r),
            format!("clé Stripe : ${{SECRET:cle-stripe-test-1f2e3d4c}}, {MASK}")
        );
    }
}
