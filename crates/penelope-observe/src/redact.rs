//! Redaction des secrets dans les logs, événements, résumés et messages (§13.1).
//!
//! Deux niveaux :
//! - **motifs génériques** (clés, bearer, JWT, clés privées, numéros de carte) ;
//! - **valeurs connues** enregistrées par le `SecretStore` : un secret déjà chargé en
//!   mémoire est masqué même s'il ne correspond à aucun motif.
//!
//! Ces valeurs vivent dans un [`Redactor`]. Le processus en a un, derrière les fonctions
//! libres de ce module ; un test en crée un neuf, pour ne voir ni les secrets ni les
//! valeurs apprises des tests voisins (issue #258).

use regex::Regex;
use std::collections::VecDeque;
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
            r#"(?i)\b(?:(?:x[_-])?api[_-]?key|apikey|key|client[_-]?secret|secret|password|passwd|pwd|access[_-]?token|auth[_-]?token|token|private[_-]?key)\b["'`]?\s*[:=]\s*["'`]?(?P<v>[^\s"'`,)}]{8,})"#,
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

/// Secrets enregistrés par le `SecretStore` et valeurs apprises dans ce que l'agent a lu
/// (issue #134). Le processus en tient un seul, [`Redactor::global`] ; un test en crée un
/// neuf (issue #258).
#[derive(Default)]
pub struct Redactor {
    /// Valeurs exactes à masquer, alimentées par le `SecretStore` au chargement.
    known: RwLock<Vec<Arc<str>>>,
    /// Valeurs apprises, bornées : les plus anciennes cèdent la place, jamais un secret
    /// enregistré.
    learned: RwLock<VecDeque<Arc<str>>>,
}

const LEARNED_MAX: usize = 2_000;

impl Redactor {
    /// Le rédacteur du processus, derrière les fonctions libres de ce module.
    pub fn global() -> &'static Redactor {
        static R: OnceLock<Redactor> = OnceLock::new();
        R.get_or_init(Redactor::default)
    }

    /// Enregistre une valeur de secret à masquer partout. Les valeurs de moins de
    /// 6 caractères sont ignorées (trop de faux positifs).
    pub fn register(&self, value: &str) {
        if value.len() < 6 {
            return;
        }
        if let Ok(mut g) = self.known.write()
            && !g.iter().any(|v| &**v == value)
        {
            g.push(Arc::from(value));
        }
    }

    pub fn forget(&self, value: &str) {
        if let Ok(mut g) = self.known.write() {
            g.retain(|v| &**v != value);
        }
    }

    pub fn registered_count(&self) -> usize {
        self.known.read().map(|g| g.len()).unwrap_or(0)
    }

    /// Secrets enregistrés et valeurs apprises, à masquer ou à reconnaître.
    fn known_values(&self) -> Vec<Arc<str>> {
        let mut v: Vec<Arc<str>> = self.known.read().map(|g| g.clone()).unwrap_or_default();
        if let Ok(g) = self.learned.read() {
            v.extend(g.iter().cloned());
        }
        v
    }
}

/// Enregistre une valeur de secret à masquer partout : voir [`Redactor::register`].
pub fn register_secret(value: &str) {
    Redactor::global().register(value);
}

pub fn forget_secret(value: &str) {
    Redactor::global().forget(value);
}

pub fn registered_count() -> usize {
    Redactor::global().registered_count()
}

/// Masque tout secret détecté : voir [`Redactor::redact`].
pub fn redact(input: &str) -> String {
    Redactor::global().redact(input)
}

/// Redaction récursive d'une valeur JSON : voir [`Redactor::redact_json`].
pub fn redact_json(v: &serde_json::Value) -> serde_json::Value {
    Redactor::global().redact_json(v)
}

/// Retient les secrets d'un texte lu par l'agent : voir [`Redactor::learn_secrets`].
pub fn learn_secrets(text: &str) {
    Redactor::global().learn_secrets(text)
}

/// Voir [`Redactor::forbidden_secret`].
pub fn forbidden_secret(input: &str) -> Option<Forbidden> {
    Redactor::global().forbidden_secret(input)
}

/// Voir [`Redactor::contains_secret`].
pub fn contains_secret(input: &str) -> bool {
    Redactor::global().contains_secret(input)
}

/// Voir [`Redactor::secret_kind`].
pub fn secret_kind(input: &str) -> Option<&'static str> {
    Redactor::global().secret_kind(input)
}

/// Voir [`Redactor::secret_fragment`].
pub fn secret_fragment(input: &str) -> Option<String> {
    Redactor::global().secret_fragment(input)
}

/// Voir [`Redactor::secret_spans`].
pub fn secret_spans(input: &str) -> Vec<SecretSpan> {
    Redactor::global().secret_spans(input)
}

/// Voir [`Redactor::leaked_secret_kind`].
pub fn leaked_secret_kind(line: &str) -> Option<&'static str> {
    Redactor::global().leaked_secret_kind(line)
}

/// Voir [`Redactor::stored_secret_kind`].
pub fn stored_secret_kind(text: &str) -> Option<&'static str> {
    Redactor::global().stored_secret_kind(text)
}

impl Redactor {
    /// Masque tout secret détecté. Idempotent : appliquer deux fois donne le même texte.
    pub fn redact(&self, input: &str) -> String {
        // Les références `${SECRET:nom}` traversent la rédaction intactes (issue #37). Le
        // découpage est **linéaire** : chaque segment entre deux références est rédigé par
        // `redact_segment`, qui ne se rappelle jamais.
        //
        // Cette fonction s'appelait elle-même sur les segments (issue #153). Un texte portant
        // `${SECRET:` sans référence complète derrière — `${SECRET:…}` avec le caractère « … »,
        // ou une ligne coupée au milieu d'une référence — passait le `contains`, `find_iter` ne
        // trouvait rien, et `redact(&input[0..])` repartait sur le **même** texte, sans fin. Six
        // lignes de ce genre, écrites par Pénélope en expliquant la syntaxe, ont fait déborder
        // la pile du thread écrivain au démarrage et revenir en arrière les 0.17.30 et 0.17.31.
        let mut out = String::with_capacity(input.len());
        let mut last = 0;
        for m in reference_re().find_iter(input) {
            out.push_str(&self.redact_segment(&input[last..m.start()]));
            out.push_str(m.as_str());
            last = m.end();
        }
        out.push_str(&self.redact_segment(&input[last..]));
        out
    }

    /// Rédige un texte qui ne contient **aucune** référence complète : c'est le seul endroit
    /// où les règles s'appliquent, et il ne se rappelle pas lui-même.
    fn redact_segment(&self, input: &str) -> String {
        let mut out = input.to_string();

        for v in self.known_values() {
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

impl Redactor {
    /// Retient comme secrets connus les valeurs repérées par motif dans un texte lu par
    /// l'agent (fichier, sortie de commande) : recopiées ensuite dans une commande, un
    /// message ou une demande, elles sont masquées partout où la rédaction passe (issue
    /// #134). Rien n'est rangé ni écrit : la liste vit le temps du processus.
    ///
    /// Seule une valeur qui a la forme d'un secret est retenue ([`learnable`]) : un mot
    /// ordinaire placé après `key=` serait sinon masqué partout, pour tout le processus
    /// (issue #258).
    pub fn learn_secrets(&self, text: &str) {
        let clean = &*without_references(text);
        for span in self.secret_spans(clean) {
            let value = &clean[span.start..span.end];
            if !learnable(span.kind, value) {
                continue;
            }
            if let Ok(mut g) = self.learned.write()
                && !g.iter().any(|v| &**v == value)
            {
                if g.len() >= LEARNED_MAX {
                    g.pop_front();
                }
                g.push_back(Arc::from(value));
            }
        }
    }
}

/// Valeur repérée qu'on peut retenir pour la masquer ailleurs (issue #258). Un motif de
/// fournisseur (`sk-`, `ghp_`, JWT…) a déjà la forme d'un secret. Une affectation et un
/// `Bearer`/`Basic` ne désignent qu'une place, où l'on trouve aussi des mots ordinaires :
/// key=`project`, `api_key = os.environ[…]`, `token = self.token`, « Basic
/// authentication ». On n'en retient qu'un jeton : l'alphabet d'un jeton, lettres et
/// chiffres mêlés, ou un jeton aléatoire au sens de [`random_token`].
fn learnable(kind: &str, value: &str) -> bool {
    const LEARNED_MIN: usize = 6;
    const TOKEN_MIN: usize = 8;
    if kind == "secret enregistré" || value.len() < LEARNED_MIN {
        return false;
    }
    let symbols = match kind {
        ASSIGNMENT => "+/=_*~!$#%^&-",
        // Un jeton porteur peut être un JWT, fait de trois parties séparées par des points.
        "bearer" => "+/=_*~!$#%^&-.",
        _ => return true,
    };
    random_token(value)
        || (value.len() >= TOKEN_MIN
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || symbols.contains(c))
            && value.chars().any(|c| c.is_ascii_alphabetic())
            && value.chars().any(|c| c.is_ascii_digit()))
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
/// sauf si le mot collé désigne une carte (`carte:…`, `cb=…`). Un nombre isolé d'un seul
/// tenant n'en est un que si la ligne nomme une carte avant lui : sinon c'est un
/// identifiant cité en prose, comme un numéro de page Meta entre accents graves (issue
/// #207). Groupé par quatre (`4539 1488 …`), il a la forme d'une carte. Rend la plage du
/// nombre.
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
    let is_card_word = |w: &str| CARD_WORDS.contains(&w.to_lowercase().as_str());
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
                .collect::<String>();
            if !is_card_word(&word) {
                return None;
            }
        } else if !m.as_str().contains([' ', '-']) {
            // D'un seul tenant : une carte seulement si la ligne la nomme avant.
            let line = input[..m.start()].rsplit('\n').next().unwrap_or_default();
            if !line.split(|c: char| !c.is_alphanumeric()).any(is_card_word) {
                return None;
            }
        }
        Some(m.range())
    })
}

impl Redactor {
    /// Fragment d'un secret détecté, masqué au milieu, pour qu'un refus dise quoi retirer
    /// sans l'exposer (issue #132) : les quatre derniers chiffres d'une carte, les quatre
    /// premiers caractères d'une clé.
    pub fn secret_fragment(&self, input: &str) -> Option<String> {
        self.forbidden_secret(input).map(|f| f.fragment)
    }
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

/// Secret interdit dans ce qui est **gardé** : sa nature, un fragment masqué au milieu,
/// et s'il est certain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forbidden {
    pub kind: &'static str,
    /// Début de la valeur ou fin d'une carte (`ghp_…`, `…6467`) : de quoi retrouver la
    /// ligne sans exposer le secret (issue #132).
    pub fragment: String,
    /// Faux pour une affectation (`token = …`) dont la valeur n'a pas la forme d'un
    /// secret : la phrase peut décrire un schéma (`token = base64url(<champs>)`).
    pub certain: bool,
}

const ASSIGNMENT: &str = "affectation de secret";

impl Redactor {
    /// Le critère unique de ce qui n'a pas le droit d'être **gardé** : mémoire (§6.10),
    /// consolidation, entités, skills, et contrôle du vault (issue #207). Motifs, numéro de
    /// carte, et valeur du magasin **si elle a la forme d'un secret**.
    ///
    /// Masquer et accuser sont deux usages : `redact` masque aussi les valeurs apprises en
    /// lecture (#134) et toute valeur du magasin, identifiant de connexion ou URL compris, où
    /// un faux positif ne coûte rien. Ici un faux positif interdit un mot ordinaire du vault,
    /// et un verdict qui dépendrait de ce que le processus a lu changerait sans que le texte
    /// ait bougé : ni valeur apprise, ni valeur du magasin sans forme de secret.
    pub fn forbidden_secret(&self, input: &str) -> Option<Forbidden> {
        let input = &*without_references(input);
        let head = |v: &str| format!("{}…", v.chars().take(4).collect::<String>());
        // Du plus sûr au moins sûr : l'affectation générique vient en dernier.
        for (re, label) in &patterns().rules {
            if *label == ASSIGNMENT {
                continue;
            }
            if let Some(c) = re.captures(input) {
                let m = c.name("v").or_else(|| c.get(0)).expect("capture complète");
                return Some(Forbidden {
                    kind: label,
                    fragment: head(m.as_str()),
                    certain: true,
                });
            }
        }
        if let Some(r) = card_to_refuse(input) {
            let digits: String = input[r].chars().filter(|c| c.is_ascii_digit()).collect();
            return Some(Forbidden {
                kind: "numéro de carte",
                fragment: format!("…{}", &digits[digits.len() - 4..]),
                certain: true,
            });
        }
        let registered = self.known.read().map(|g| g.clone()).unwrap_or_default();
        if let Some(v) = registered
            .iter()
            .find(|v| secret_shaped(v) && input.contains(&***v))
        {
            return Some(Forbidden {
                kind: "secret enregistré",
                fragment: head(v),
                certain: true,
            });
        }
        let (re, _) = patterns().rules.iter().find(|(_, l)| *l == ASSIGNMENT)?;
        let value = re.captures(input)?.name("v")?.as_str();
        Some(Forbidden {
            kind: ASSIGNMENT,
            fragment: head(value),
            certain: random_token(value),
        })
    }
}

/// Valeur qui a la forme d'un secret : un motif connu, ou un jeton aléatoire. Une
/// adresse électronique, une URL, un identifiant lisible n'en sont pas.
fn secret_shaped(v: &str) -> bool {
    random_token(v)
        || patterns()
            .rules
            .iter()
            .any(|(re, label)| *label != ASSIGNMENT && re.is_match(v))
}

/// Jeton d'un seul tenant, sans `@`, `.` ni `:` (qui font les adresses et les URL), de 20
/// caractères au moins, aléatoire au sens de [`looks_random`] ou long bloc hexadécimal.
fn random_token(v: &str) -> bool {
    const TOKEN_MIN: usize = 20;
    v.len() >= TOKEN_MIN
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_*~!$#%^&-".contains(c))
        && (looks_random(v) || looks_hex_blob(v))
}

impl Redactor {
    /// Vrai si le texte contient un secret qui n'a pas le droit d'être gardé : voir
    /// [`forbidden_secret`], dont c'est le raccourci.
    pub fn contains_secret(&self, input: &str) -> bool {
        self.forbidden_secret(input).is_some()
    }

    /// Secret laissé en clair dans un journal : valeur enregistrée ou jeton reconnaissable.
    /// Les affectations génériques (`token = …`) sont ignorées, trop fréquentes dans les
    /// traces légitimes.
    pub fn leaked_secret_kind(&self, line: &str) -> Option<&'static str> {
        let line = &*without_references(line);
        if self.known_values().iter().any(|v| line.contains(&**v)) {
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
    pub fn stored_secret_kind(&self, text: &str) -> Option<&'static str> {
        let text = &*without_references(text);
        if self.known_values().iter().any(|v| text.contains(&**v)) {
            return Some("secret enregistré");
        }
        if let Some((_, label)) = patterns().rules.iter().find(|(re, _)| re.is_match(text)) {
            return Some(label);
        }
        (redact_random_tokens(text) != text).then_some("jeton aléatoire")
    }
}

/// Secret repéré dans un texte : position de la **valeur** (sans le mot-clé qui la
/// précède) et nature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSpan {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
}

impl Redactor {
    /// Valeurs de secrets d'un texte, dans l'ordre, sans chevauchement (issue #37) : de quoi
    /// ranger chaque valeur dans le magasin et ne garder en mémoire qu'une référence. Les
    /// numéros de carte n'en font pas partie : ils ne se rangent pas, ils se refusent.
    pub fn secret_spans(&self, input: &str) -> Vec<SecretSpan> {
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
            for v in self.known_values() {
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

    /// Nature du secret interdit ([`forbidden_secret`]), pour nommer un refus.
    pub fn secret_kind(&self, input: &str) -> Option<&'static str> {
        self.forbidden_secret(input).map(|f| f.kind)
    }

    /// Redaction récursive d'une valeur JSON (payloads d'événements, arguments d'outils).
    pub fn redact_json(&self, v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match v {
            Value::String(s) => Value::String(self.redact(s)),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.redact_json(v)).collect()),
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
                            self.redact_json(val)
                        },
                    );
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod forbidden_tests;

#[cfg(test)]
mod tests;
