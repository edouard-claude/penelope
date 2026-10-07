//! Profils de modèles et résolution unique d'un rôle (#332, décision 0021).
//!
//! Une seule règle, pour toute requête qui choisit un modèle :
//!
//! ```text
//!  rôle ─► surcharge du profil actif ? ──────────────────────────► ✎ surcharge
//!       └► rôle de voix (stt, tts, trace) ? ─► [voice] ───────────► 🏠 local
//!       └► rôle de capacité (image, vision, embeddings) ?
//!            ├► capacité posée dans le profil ───────────────────► ⚡ capacité
//!            ├► le catalogue dit que le principal sait faire ──────► ✓ principal
//!            └► modèle livré de la capacité ──────────────────────► ⚡ capacité
//!       └► sinon ────────────────────────────────────────────────► ✓ principal
//! ```
//!
//! Les clés d'avant (`models.roles`, `models.routing`) restent lues : tant qu'aucun
//! `[models.profiles.defaut]` n'est écrit, le profil `defaut` en est **déduit** à chaque
//! lecture, rôle par rôle comme l'ancien `role_alias`, replis propres à chaque rôle
//! compris (titre et juge sur `fast`, rêve sur `compaction`, pointage sur la vision). Rien
//! ne change donc avant que le propriétaire touche à ses profils ; la première
//! modification écrit le profil en entier ([`Models::materialize`]).

use super::*;
use std::borrow::Cow;

/// Profil déduit des clés d'avant, et profil actif par défaut.
pub const DEFAULT_PROFILE: &str = "defaut";
/// Garde Codex : le travail de fond ne passe pas par l'abonnement (comportement d'avant).
pub const CODEX_DENY: &str = "deny";
/// Garde Codex levée : tout passe par l'abonnement, travail de fond compris.
pub const CODEX_ALLOW: &str = "allow";
/// Le risque de la garde levée, dit une fois dans la doc et dans `/model` (#333).
pub const CODEX_RISK: &str = "OpenAI tolère l'usage interactif d'un abonnement ChatGPT ; un \
usage automatisé (rêve, veilles, workflows planifiés) expose le compte à une suspension.";
/// Transcription livrée, quand rien d'autre n'est dit.
pub const DEFAULT_STT_MODEL: &str = "openai_compat:whisper-default";
/// Modèle livré des capacités d'image (génération et lecture).
pub const DEFAULT_IMAGE_MODEL: &str = "openrouter:google/gemini-3.1-flash-image";

/// Un profil : ce que le propriétaire choisit d'un geste.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelProfile {
    /// Modèle principal : un alias (`main`) ou un identifiant `fournisseur:modèle`.
    pub primary: String,
    /// Garde Codex du travail de fond (#333) : `deny` (il passe ailleurs, en le disant) ou
    /// `allow` (tout passe par l'abonnement).
    pub codex_background: String,
    /// Rôle vers modèle : les exceptions explicites au principal.
    pub overrides: BTreeMap<String, String>,
    /// Capacité (`image_generate`, `vision`, `embedding`) vers modèle, pour ce que le
    /// principal ne sait pas faire.
    pub capabilities: BTreeMap<String, String>,
    /// Modèles du classifieur de complexité ; vide : le principal.
    pub routing: ProfileRouting,
    /// Chaînes de repli sur panne, par modèle ou alias de départ.
    pub fallback: BTreeMap<String, Vec<String>>,
}

impl Default for ModelProfile {
    fn default() -> Self {
        ModelProfile {
            primary: "main".into(),
            codex_background: CODEX_DENY.into(),
            overrides: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            routing: ProfileRouting::default(),
            fallback: BTreeMap::new(),
        }
    }
}

/// Étages du classifieur dans un profil : vide, l'étage suit le principal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ProfileRouting {
    /// Message simple.
    pub low: String,
    /// Message ordinaire.
    pub medium: String,
    /// Message complexe.
    pub high: String,
}

/// Étage du classifieur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Low,
    Medium,
    High,
}

/// Ce que le principal peut ne pas savoir faire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ImageGenerate,
    Vision,
    Embedding,
}

impl Capability {
    pub const ALL: [Capability; 3] = [
        Capability::ImageGenerate,
        Capability::Vision,
        Capability::Embedding,
    ];

    /// Clé de `capabilities.<clé>`.
    pub fn key(self) -> &'static str {
        match self {
            Capability::ImageGenerate => "image_generate",
            Capability::Vision => "vision",
            Capability::Embedding => "embedding",
        }
    }

    pub fn parse(key: &str) -> Option<Capability> {
        Capability::ALL.into_iter().find(|c| c.key() == key)
    }

    /// Modèle livré, quand le profil n'en pose pas et que le principal ne sait pas faire.
    pub fn default_model(self) -> &'static str {
        match self {
            Capability::ImageGenerate | Capability::Vision => DEFAULT_IMAGE_MODEL,
            Capability::Embedding => DEFAULT_EMBEDDING_MODEL,
        }
    }
}

/// Familles de `/model` (#334).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Conversation,
    Background,
    Media,
    Voice,
}

impl Family {
    pub const ALL: [Family; 4] = [
        Family::Conversation,
        Family::Background,
        Family::Media,
        Family::Voice,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Family::Conversation => "conversation",
            Family::Background => "background",
            Family::Media => "media",
            Family::Voice => "voice",
        }
    }

    pub fn parse(key: &str) -> Option<Family> {
        Family::ALL.into_iter().find(|f| f.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Family::Conversation => "💬 Conversation",
            Family::Background => "⚙️ Travail de fond",
            Family::Media => "🎨 Médias",
            Family::Voice => "🏠 Local / voix",
        }
    }
}

/// Un rôle connu : sa famille, son nom lisible, la capacité qu'il exige.
#[derive(Debug, Clone, Copy)]
pub struct RoleSpec {
    pub name: &'static str,
    pub family: Family,
    pub label: &'static str,
    pub capability: Option<Capability>,
}

const fn role(name: &'static str, family: Family, label: &'static str) -> RoleSpec {
    RoleSpec {
        name,
        family,
        label,
        capability: None,
    }
}

const fn media(name: &'static str, label: &'static str, capability: Capability) -> RoleSpec {
    RoleSpec {
        name,
        family: Family::Media,
        label,
        capability: Some(capability),
    }
}

/// Tous les sites qui choisissent un modèle passent par l'un de ces rôles (#332).
pub const ROLES: &[RoleSpec] = &[
    role("chat_default", Family::Conversation, "conversation"),
    role("classifier", Family::Conversation, "classifieur"),
    role("title", Family::Conversation, "titre de session"),
    role("workflow", Family::Background, "étapes de workflow"),
    role("code", Family::Background, "étapes de code"),
    role("dream", Family::Background, "rêve (consolidation)"),
    role("memory_review", Family::Background, "mémoire et épisodes"),
    role("compaction", Family::Background, "compaction"),
    role("approval_judge", Family::Background, "juge d'approbation"),
    media(
        "image_generate",
        "génération d'image",
        Capability::ImageGenerate,
    ),
    media("image_describe", "lecture d'image", Capability::Vision),
    media(
        "image_locate",
        "pointage dans une image",
        Capability::Vision,
    ),
    media("embedding", "embeddings", Capability::Embedding),
    role("stt", Family::Voice, "transcription"),
    role("tts", Family::Voice, "synthèse vocale"),
    role("trace", Family::Voice, "narration"),
];

/// Rôles de voix : ils vivent dans `[voice]` et ne suivent jamais le principal.
pub const VOICE_ROLES: &[&str] = &["stt", "tts", "trace"];

/// Rôles qu'un alias `local:` peut servir sans être un modèle de texte.
const NOT_TEXT_ROLES: &[&str] = &[
    "stt",
    "tts",
    "embedding",
    "image_generate",
    "image_describe",
    "image_locate",
];

pub fn role_spec(name: &str) -> Option<&'static RoleSpec> {
    ROLES.iter().find(|r| r.name == name)
}

/// Pourquoi un rôle tombe sur son modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelReason {
    /// Il suit le principal.
    Primary,
    /// Surcharge du profil.
    Override,
    /// Modèle de capacité.
    Capability,
    /// Voix, à part.
    Local,
    /// Garde Codex appliquée au travail de fond.
    CodexGuard,
}

impl ModelReason {
    pub fn icon(self) -> &'static str {
        match self {
            ModelReason::Primary => "✓",
            ModelReason::Override => "✎",
            ModelReason::Capability => "⚡",
            ModelReason::Local => "🏠",
            ModelReason::CodexGuard => "⛔",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModelReason::Primary => "suit le principal",
            ModelReason::Override => "surcharge",
            ModelReason::Capability => "capacité",
            ModelReason::Local => "voix, à part",
            ModelReason::CodexGuard => "garde Codex",
        }
    }
}

/// Modèle effectif d'un rôle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    pub role: String,
    /// Ce que le profil nomme : un alias, ou l'identifiant lui-même.
    pub label: String,
    /// Identifiant `fournisseur:modèle` ; `None` : rien ne le sert (narration sans modèle
    /// local, alias absent).
    pub model: Option<String>,
    pub reason: ModelReason,
}

/// Ce que le catalogue sait d'un modèle. `None` : il ne le connaît pas, et le principal
/// ne prend alors pas une capacité qu'on ne lui sait pas.
pub trait ModelCaps {
    fn supports(&self, model_id: &str, cap: Capability) -> Option<bool>;
}

/// Sans catalogue.
pub struct NoCaps;

impl ModelCaps for NoCaps {
    fn supports(&self, _: &str, _: Capability) -> Option<bool> {
        None
    }
}

impl Models {
    /// Nom du profil actif.
    pub fn active_name(&self) -> &str {
        if self.profile.trim().is_empty() {
            DEFAULT_PROFILE
        } else {
            self.profile.as_str()
        }
    }

    /// Profils connus, `defaut` compris quand il est encore déduit.
    pub fn profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.profiles.keys().cloned().collect();
        if !names.iter().any(|n| n == DEFAULT_PROFILE) {
            names.insert(0, DEFAULT_PROFILE.to_string());
        }
        names
    }

    /// Un profil par son nom : écrit, ou `defaut` déduit des clés d'avant.
    pub fn profile_named(&self, name: &str) -> Option<Cow<'_, ModelProfile>> {
        match self.profiles.get(name) {
            Some(p) => Some(Cow::Borrowed(p)),
            None if name == DEFAULT_PROFILE => Some(Cow::Owned(self.legacy_profile())),
            None => None,
        }
    }

    /// Le profil actif ; un nom inconnu (refusé par la validation) retombe sur `defaut`.
    pub fn active(&self) -> Cow<'_, ModelProfile> {
        self.profile_named(self.active_name())
            .unwrap_or_else(|| Cow::Owned(self.legacy_profile()))
    }

    /// Vrai si ce profil est encore déduit des clés d'avant.
    pub fn is_derived(&self, name: &str) -> bool {
        name == DEFAULT_PROFILE && !self.profiles.contains_key(name)
    }

    /// Écrit un profil déduit pour le modifier : la mutation porte alors le profil entier,
    /// et le fichier ne perd rien de ce qui était déduit.
    pub fn materialize(&mut self, name: &str) -> Option<&mut ModelProfile> {
        if self.is_derived(name) {
            let p = self.legacy_profile();
            self.profiles.insert(name.to_string(), p);
        }
        self.profiles.get_mut(name)
    }

    /// Alias d'un rôle selon les clés d'avant : le rôle, sinon `chat_default`, sinon `main`.
    fn legacy_role_alias(&self, role: &str) -> String {
        self.roles
            .get(role)
            .or_else(|| self.roles.get("chat_default"))
            .cloned()
            .unwrap_or_else(|| "main".to_string())
    }

    /// Alias « petit modèle » d'avant : `fast` s'il existe, sinon celui du classifieur.
    fn legacy_small(&self, role: &str) -> String {
        if let Some(a) = self.roles.get(role) {
            return a.clone();
        }
        if self.aliases.contains_key("fast") {
            return "fast".into();
        }
        self.legacy_role_alias("classifier")
    }

    /// Le profil `defaut` tel que les clés d'avant le décrivent, rôle par rôle.
    pub fn legacy_profile(&self) -> ModelProfile {
        let primary = self.legacy_role_alias("chat_default");
        let mut overrides = BTreeMap::new();
        let skip = [
            "chat_default",
            "image_generate",
            "image_describe",
            "embedding",
            "stt",
            "tts",
            "trace",
        ];
        for (role, alias) in &self.roles {
            if !skip.contains(&role.as_str()) {
                overrides.insert(role.clone(), alias.clone());
            }
        }
        // Les replis d'avant, propres à chaque rôle, deviennent des surcharges écrites.
        for (role, alias) in [
            ("title", self.legacy_small("title")),
            ("approval_judge", self.legacy_small("approval_judge")),
            ("dream", self.legacy_role_alias("compaction")),
        ] {
            overrides.entry(role.to_string()).or_insert(alias);
        }
        for role in ROLES
            .iter()
            .filter(|r| r.capability.is_none() && !VOICE_ROLES.contains(&r.name))
        {
            overrides
                .entry(role.name.to_string())
                .or_insert_with(|| self.legacy_role_alias(role.name));
        }
        let vision = self.legacy_role_alias("image_describe");
        if overrides.get("image_locate") == Some(&vision) {
            overrides.remove("image_locate");
        }
        overrides.retain(|_, alias| *alias != primary);
        let capabilities = [
            ("image_generate", self.legacy_role_alias("image_generate")),
            ("vision", vision),
            ("embedding", self.legacy_role_alias("embedding")),
        ]
        .into_iter()
        .map(|(c, a)| (c.to_string(), a))
        .collect();
        ModelProfile {
            primary,
            codex_background: CODEX_DENY.into(),
            overrides,
            capabilities,
            routing: ProfileRouting {
                low: self.routing.low.clone(),
                medium: self.routing.medium.clone(),
                high: self.routing.high.clone(),
            },
            fallback: self.routing.fallback.clone(),
        }
    }

    /// Alias qu'aucun rôle, profil, étage ni repli ne lit (#335) : `doctor` les nomme.
    pub fn unused_aliases(&self, voice: &Voice) -> Vec<String> {
        let mut used: Vec<&str> = Vec::new();
        let profiles: Vec<Cow<'_, ModelProfile>> = self
            .profile_names()
            .iter()
            .filter_map(|n| self.profile_named(n).map(|p| Cow::Owned(p.into_owned())))
            .collect();
        for p in &profiles {
            used.push(&p.primary);
            used.extend(p.overrides.values().map(String::as_str));
            used.extend(p.capabilities.values().map(String::as_str));
            used.extend([&p.routing.low, &p.routing.medium, &p.routing.high].map(String::as_str));
            for (from, chain) in &p.fallback {
                used.push(from);
                used.extend(chain.iter().map(String::as_str));
            }
        }
        used.extend(self.roles.values().map(String::as_str));
        used.extend([&voice.stt, &voice.tts, &voice.narrator].map(String::as_str));
        // Défauts de la voix et de la narration, lus par nom.
        used.extend(["stt", "tts", "local"]);
        self.aliases
            .keys()
            .filter(|a| !used.contains(&a.as_str()))
            .cloned()
            .collect()
    }
}

impl Config {
    /// Modèle effectif d'un rôle, sans catalogue.
    pub fn resolve_role(&self, role: &str) -> Resolved {
        self.resolve_role_with(role, &NoCaps)
    }

    /// **La** résolution (#332) : surcharge, voix, capacité, principal.
    pub fn resolve_role_with(&self, role: &str, caps: &dyn ModelCaps) -> Resolved {
        let profile = self.models.active();
        let done = |label: String, reason| Resolved {
            role: role.to_string(),
            model: self.alias_model(&label).map(String::from),
            label,
            reason,
        };
        if VOICE_ROLES.contains(&role) {
            let label = self.voice_label(role);
            return match label {
                Some(l) => done(l, ModelReason::Local),
                None => Resolved {
                    role: role.to_string(),
                    label: String::new(),
                    model: None,
                    reason: ModelReason::Local,
                },
            };
        }
        if let Some(label) = profile.overrides.get(role).filter(|l| !l.is_empty()) {
            return done(label.clone(), ModelReason::Override);
        }
        if let Some(cap) = role_spec(role).and_then(|r| r.capability) {
            if let Some(label) = profile
                .capabilities
                .get(cap.key())
                .filter(|l| !l.is_empty())
            {
                return done(label.clone(), ModelReason::Capability);
            }
            let primary = self.alias_model(&profile.primary).unwrap_or_default();
            if caps.supports(primary, cap) == Some(true) {
                return done(profile.primary.clone(), ModelReason::Primary);
            }
            return done(cap.default_model().to_string(), ModelReason::Capability);
        }
        done(profile.primary.clone(), ModelReason::Primary)
    }

    /// Identifiant du modèle d'un rôle ; `None` si rien ne le sert.
    pub fn role_model(&self, role: &str) -> Option<String> {
        self.resolve_role(role).model
    }

    /// Ce que le profil nomme pour un rôle : un alias, ou un identifiant.
    pub fn role_alias(&self, role: &str) -> String {
        self.resolve_role(role).label
    }

    /// Tous les rôles connus, résolus, dans l'ordre des familles.
    pub fn resolve_all(&self, caps: &dyn ModelCaps) -> Vec<Resolved> {
        ROLES
            .iter()
            .map(|r| self.resolve_role_with(r.name, caps))
            .collect()
    }

    /// Le principal du profil actif.
    pub fn primary_label(&self) -> String {
        self.models.active().primary.clone()
    }

    /// Ce que le classifieur choisit pour un étage : l'étage du profil, sinon le principal.
    pub fn routing_label(&self, tier: Tier) -> String {
        let p = self.models.active();
        let l = match tier {
            Tier::Low => &p.routing.low,
            Tier::Medium => &p.routing.medium,
            Tier::High => &p.routing.high,
        };
        if l.is_empty() {
            p.primary.clone()
        } else {
            l.clone()
        }
    }

    /// Le classifieur a-t-il quelque chose à choisir ? Faux quand il est éteint, ou quand
    /// les trois étages tombent sur le principal (#332) : aucun appel pour rien.
    pub fn adaptive_routing(&self) -> bool {
        if !self.models.routing.classifier {
            return false;
        }
        let primary = self.alias_model(&self.primary_label()).map(String::from);
        [Tier::Low, Tier::Medium, Tier::High]
            .into_iter()
            .any(|t| self.alias_model(&self.routing_label(t)).map(String::from) != primary)
    }

    /// Chaîne de repli d'un modèle ou alias de départ, dans le profil actif.
    pub fn fallback_labels(&self, label: &str) -> Vec<String> {
        let p = self.models.active();
        if let Some(chain) = p.fallback.get(label) {
            return chain.clone();
        }
        // Un alias dont le modèle est cité comme départ, ou l'inverse.
        let id = self.alias_model(label).map(String::from);
        p.fallback
            .iter()
            .find(|(from, _)| id.is_some() && self.alias_model(from).map(String::from) == id)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    }

    /// Garde Codex levée dans le profil actif (#333).
    pub fn codex_background_allowed(&self) -> bool {
        self.models.active().codex_background == CODEX_ALLOW
    }

    /// Modèle de voix : `[voice]`, sinon les clés d'avant, sinon le défaut livré.
    fn voice_label(&self, role: &str) -> Option<String> {
        let set = |v: &String| (!v.trim().is_empty()).then(|| v.clone());
        let m = &self.models;
        match role {
            "stt" => set(&self.voice.stt)
                .or_else(|| m.roles.get("stt").cloned())
                .or_else(|| m.aliases.contains_key("stt").then(|| "stt".to_string()))
                .or_else(|| Some(DEFAULT_STT_MODEL.into())),
            "tts" => set(&self.voice.tts)
                .or_else(|| {
                    m.roles
                        .get("tts")
                        .filter(|a| m.aliases.contains_key(*a))
                        .cloned()
                })
                .or_else(|| m.aliases.contains_key("tts").then(|| "tts".to_string()))
                .or_else(|| Some(DEFAULT_TTS_MODEL.into())),
            _ => set(&self.voice.narrator)
                .or_else(|| m.roles.get("trace").cloned())
                .or_else(|| self.local_narrator()),
        }
    }

    /// Narration sans modèle dit : l'alias `local`, puis le premier alias `local:` (ordre
    /// alphabétique) qui ne sert pas un rôle de voix, d'image ou d'embeddings.
    fn local_narrator(&self) -> Option<String> {
        let m = &self.models;
        let serves_other = |alias: &str| {
            m.roles
                .iter()
                .any(|(r, a)| a == alias && NOT_TEXT_ROLES.contains(&r.as_str()))
        };
        let mut candidates: Vec<&String> = m
            .aliases
            .iter()
            .filter(|(a, id)| id.starts_with("local:") && !serves_other(a))
            .map(|(a, _)| a)
            .collect();
        candidates.sort_by_key(|a| (*a != "local", (*a).clone()));
        candidates.first().map(|a| (*a).clone())
    }

    /// Profils : chaque modèle cité se lit, la garde Codex est connue, le profil actif
    /// existe.
    pub(super) fn validate_profiles(&self) -> Result<()> {
        let m = &self.models;
        if m.profile_named(m.active_name()).is_none() {
            return Err(KernelError::config(format!(
                "models.profile : profil inconnu `{}` (profils : {})",
                m.active_name(),
                m.profile_names().join(", ")
            )));
        }
        let known = |where_: &str, label: &str| -> Result<()> {
            if self.alias_model(label).is_none() {
                return Err(KernelError::config(format!(
                    "{where_} : `{label}` n'est ni un alias ni un identifiant `fournisseur:modèle`"
                )));
            }
            Ok(())
        };
        for (name, p) in &m.profiles {
            let at = format!("models.profiles.{name}");
            known(&format!("{at}.primary"), &p.primary)?;
            if ![CODEX_ALLOW, CODEX_DENY].contains(&p.codex_background.as_str()) {
                return Err(KernelError::config(format!(
                    "{at}.codex_background doit valoir `allow` ou `deny` (reçu `{}`)",
                    p.codex_background
                )));
            }
            for (role, l) in &p.overrides {
                known(&format!("{at}.overrides.{role}"), l)?;
            }
            for (cap, l) in &p.capabilities {
                if Capability::parse(cap).is_none() {
                    return Err(KernelError::config(format!(
                        "{at}.capabilities : capacité inconnue `{cap}` (image_generate, vision, embedding)"
                    )));
                }
                known(&format!("{at}.capabilities.{cap}"), l)?;
            }
            for l in [&p.routing.low, &p.routing.medium, &p.routing.high] {
                if !l.is_empty() {
                    known(&format!("{at}.routing"), l)?;
                }
            }
            for (from, chain) in &p.fallback {
                known(&format!("{at}.fallback"), from)?;
                for to in chain {
                    known(&format!("{at}.fallback.{from}"), to)?;
                }
            }
        }
        for (key, l) in [
            ("voice.stt", &self.voice.stt),
            ("voice.tts", &self.voice.tts),
            ("voice.narrator", &self.voice.narrator),
        ] {
            if !l.trim().is_empty() {
                known(key, l)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
