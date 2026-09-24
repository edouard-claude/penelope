//! Secrets dans ce qui est à retenir (issue #37) : la valeur part dans le magasin de
//! secrets, la mémoire ne garde qu'une référence `${SECRET:nom}`.
//!
//! ```text
//!  « la clé Stripe de test c'est sk_test_FauxCle… »
//!        │ secret_spans (penelope-observe)
//!        ▼
//!  magasin : cle-stripe-test-1f2e3d4c = sk_test_FauxCle…
//!  candidat : « la clé Stripe de test c'est ${SECRET:cle-stripe-test-1f2e3d4c} »
//! ```
//!
//! Le nom dérive du contexte et d'une empreinte de la valeur : la même valeur redite
//! reprend le même nom. Un numéro de carte ne se range pas : le filtre d'écriture le refuse.

use penelope_app::services::Services;

/// Mots qui ne disent rien du secret.
const STOP: &[&str] = &[
    "avec", "dans", "pour", "sans", "sous", "leur", "notre", "votre", "cest", "est", "les", "des",
    "une", "mon", "mes", "son", "ses", "nos", "vos", "clé", "cle", "cles", "clés", "mot", "mots",
    "passe", "token", "jeton", "secret", "password", "passwd", "key", "api", "the", "and", "voici",
    "c'est", "sont", "valeur", "code",
];

/// Préfixe du nom selon la nature du secret.
fn kind_prefix(kind: &str) -> &str {
    match kind {
        "affectation de secret" | "secret enregistré" => "secret",
        other => other,
    }
}

fn fold(word: &str) -> String {
    word.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Nom du secret : nature, deux mots de contexte au plus (les plus proches avant la
/// valeur), empreinte de la valeur.
pub fn secret_name(kind: &str, text: &str, start: usize, end: usize) -> String {
    let value = &text[start..end];
    let hash = penelope_kernel::canonical::sha256_hex(value.as_bytes());
    let prefix: Vec<String> = kind_prefix(kind).split_whitespace().map(fold).collect();
    let context: Vec<String> = text[..start]
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && w.chars().all(char::is_alphabetic))
        .map(fold)
        .filter(|w| !STOP.contains(&w.as_str()) && !prefix.contains(w))
        .collect();
    let mut parts = prefix;
    let mut picked: Vec<String> = Vec::new();
    for w in context.iter().rev() {
        if !picked.contains(w) {
            picked.push(w.clone());
        }
        if picked.len() == 2 {
            break;
        }
    }
    picked.reverse();
    parts.extend(picked);
    parts.push(hash[..8].to_string());
    let name = penelope_platform::slugify(&parts.join("-"));
    name.chars().take(96).collect()
}

/// Range les secrets d'un texte et le rend avec des références ; renvoie aussi les noms.
/// Un secret qu'on ne peut pas ranger fait échouer l'ensemble : jamais de valeur en clair.
pub fn shelve(s: &Services, text: &str) -> Result<(String, Vec<String>), String> {
    let spans = penelope_observe::redact::secret_spans(text);
    if spans.is_empty() {
        return Ok((text.to_string(), Vec::new()));
    }
    let mut out = String::with_capacity(text.len());
    let mut names = Vec::new();
    let mut last = 0;
    for span in spans {
        let name = secret_name(span.kind, text, span.start, span.end);
        penelope_platform::validate_secret_name(&name).map_err(|e| e.to_string())?;
        let value = &text[span.start..span.end];
        s.platform
            .secrets
            .set(&name, value)
            .map_err(|e| format!("secret `{name}` non rangé : {e}"))?;
        penelope_observe::register_secret(value);
        out.push_str(&text[last..span.start]);
        out.push_str(&format!("${{SECRET:{name}}}"));
        names.push(name);
        last = span.end;
    }
    out.push_str(&text[last..]);
    Ok((out, names))
}

/// Noms des secrets référencés dans un texte.
pub fn references(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("${SECRET:") {
        let after = &rest[i + "${SECRET:".len()..];
        let Some(end) = after.find('}') else { break };
        let name = after[..end].to_string();
        if !out.contains(&name) {
            out.push(name);
        }
        rest = &after[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_context_and_the_value() {
        let t = "le client Stripe de test c'est cus_NffrFeUfNV2Hib, sa clé sk_test_FauxCle0123456";
        let spans = penelope_observe::redact::secret_spans(t);
        assert_eq!(spans.len(), 1);
        let name = secret_name(spans[0].kind, t, spans[0].start, spans[0].end);
        assert!(name.starts_with("cle-stripe-client-test-"), "{name}");
        assert_eq!(
            name,
            secret_name(spans[0].kind, t, spans[0].start, spans[0].end),
            "stable"
        );
        let wifi = "le wifi invité de l'agence, password: SuperWifi2026";
        let s = &penelope_observe::redact::secret_spans(wifi)[0];
        let n = secret_name(s.kind, wifi, s.start, s.end);
        assert!(n.starts_with("secret-invite-agence-"), "{n}");
        penelope_platform::validate_secret_name(&n).unwrap();
    }

    #[test]
    fn references_are_listed_once() {
        assert_eq!(
            references("a ${SECRET:x-1} b ${SECRET:y} c ${SECRET:x-1}"),
            vec!["x-1".to_string(), "y".to_string()]
        );
        assert!(references("rien").is_empty());
    }
}
