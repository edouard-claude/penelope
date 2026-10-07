//! Modifications des profils (#334) : `model.set`, `model.unset`, `model.profile`.
//!
//! Toute modification d'un profil encore déduit des clés d'avant l'écrit d'abord en
//! entier (`Models::materialize`) : rien de ce qui était déduit ne se perd.

use super::str_of;
use penelope_app::codex_scope::{BACKGROUND_ROLES, is_codex, refusal};
use penelope_app::services::Services;
use penelope_kernel::config::{
    CODEX_ALLOW, CODEX_DENY, CODEX_RISK, Capability, Config, DEFAULT_PROFILE, ModelProfile,
    VOICE_ROLES, check_model_id, role_spec,
};
use serde_json::{Value, json};

/// Ce que `model.set` vise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Le principal du profil.
    Primary,
    /// Un étage du classifieur (`routing.low`…).
    Tier(String),
    /// Un modèle de capacité (`image_generate`, `vision`, `embedding`).
    Capability(String),
    /// Un rôle de voix, dans `[voice]` (`stt`, `tts`, `trace`).
    Voice(String),
    /// Une surcharge de rôle.
    Role(String),
    /// Un alias, comme avant les profils (`main`, `fast`…).
    Alias(String),
}

impl Target {
    pub fn parse(cfg: &Config, raw: &str) -> Target {
        let raw = raw.trim();
        if raw == "primary" || raw == "principal" {
            return Target::Primary;
        }
        if let Some(t) = raw.strip_prefix("routing.")
            && ["low", "medium", "high"].contains(&t)
        {
            return Target::Tier(t.to_string());
        }
        if Capability::parse(raw).is_some() {
            return Target::Capability(raw.to_string());
        }
        if VOICE_ROLES.contains(&raw) {
            return Target::Voice(raw.to_string());
        }
        if role_spec(raw).is_some() || cfg.models.active().overrides.contains_key(raw) {
            return Target::Role(raw.to_string());
        }
        Target::Alias(raw.to_string())
    }

    /// Les rôles que la cible sert : pour les refus d'outils et de garde.
    fn roles(&self, cfg: &Config) -> Vec<String> {
        match self {
            Target::Primary => vec!["chat_default".into()],
            Target::Tier(_) => vec!["chat_default".into()],
            Target::Capability(c) => vec![c.clone()],
            Target::Voice(v) | Target::Role(v) => vec![v.clone()],
            Target::Alias(a) => penelope_kernel::config::ROLES
                .iter()
                .filter(|r| cfg.role_alias(r.name) == *a)
                .map(|r| r.name.to_string())
                .collect(),
        }
    }
}

/// Le profil visé : `profile`, sinon l'actif.
fn profile_name(cfg: &Config, p: &Value) -> String {
    p.get("profile")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map_or_else(|| cfg.models.active_name().to_string(), String::from)
}

/// Un modèle nommé : un alias, ou un identifiant `fournisseur:modèle` qui se lit.
fn check_label(cfg: &Config, model: &str) -> anyhow::Result<()> {
    if cfg.models.aliases.contains_key(model) {
        return Ok(());
    }
    check_model_id(model).map_err(anyhow::Error::msg)
}

/// `model.set` : `target` (ou `alias`, comme avant) reçoit `model`.
pub fn set(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let raw = p
        .get("target")
        .or_else(|| p.get("alias"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("paramètre `target` manquant"))?;
    let model = str_of(p, "model")?;
    // Un identifiant mal écrit (`codx:`) partait en silence chez OpenRouter (#142).
    check_label(&cfg, &model)?;
    let target = Target::parse(&cfg, raw);
    let name = profile_name(&cfg, p);
    let id = cfg.alias_model(&model).unwrap_or(&model).to_string();

    // Sous la garde, un rôle de fond ne vise pas l'abonnement (#142, #333).
    if is_codex(&id) {
        if matches!(target, Target::Voice(_)) {
            anyhow::bail!("la voix ne passe jamais par l'abonnement ChatGPT : `{raw}` reste local");
        }
        let guarded = !profile_allows_codex(&cfg, &name);
        let background: Vec<String> = target
            .roles(&cfg)
            .into_iter()
            .filter(|r| BACKGROUND_ROLES.contains(&r.as_str()))
            .collect();
        if guarded && !background.is_empty() {
            anyhow::bail!(refusal(raw, &id, &background));
        }
    }
    // Un rôle à outils ne prend pas un modèle qui n'en appelle pas (#54, décision 0009).
    let needs_tools = match &target {
        Target::Alias(a) => crate::doctor::alias_needs_tools(&cfg, a),
        Target::Primary | Target::Tier(_) => true,
        Target::Role(r) => !matches!(
            r.as_str(),
            "classifier" | "title" | "approval_judge" | "compaction" | "memory_review" | "dream"
        ),
        Target::Capability(_) | Target::Voice(_) => false,
    };
    if needs_tools && s.catalog.get(&id).map(|i| i.supports_tools()) == Some(false) {
        anyhow::bail!(
            "`{model}` n'appelle pas d'outils : `{raw}` sert un rôle qui en a besoin. Choisir un \
             modèle avec tool calling, ou le donner à un rôle sans outils."
        );
    }
    let extraction = target
        .roles(&cfg)
        .iter()
        .any(|r| ["compaction", "memory_review", "dream"].contains(&r.as_str()));
    // Un rôle d'extraction donné à un modèle qui ne coupe pas son raisonnement dépense son
    // budget à réfléchir (#152) : un avertissement, pas un refus.
    let warning = s
        .catalog
        .get(&id)
        .filter(|_| extraction)
        .filter(|i| i.lightest_effort().as_deref() != Some("none"))
        .map(|i| {
            format!(
                "`{model}` impose un raisonnement (effort le plus faible : {}) et `{raw}` sert \
                 la consolidation : son budget de sortie partira en réflexion, et la passe \
                 nocturne peut ne rien rendre",
                i.lightest_effort().unwrap_or_else(|| "?".into())
            )
        });

    let (t, m, n) = (target.clone(), model.clone(), name.clone());
    let g = s.publish_config("cli", move |c| {
        let path = apply_set(c, &n, &t, &m)?;
        Ok(vec![path])
    })?;
    let known = s.catalog.is_empty() || s.catalog.get(&id).is_some();
    let mut out = json!({
        "generation": g,
        "profile": name,
        "target": raw,
        "alias": raw,
        "model": model,
        "known": known,
    });
    if let Some(w) = warning
        && let Some(o) = out.as_object_mut()
    {
        o.insert("avertissement".into(), json!(w));
    }
    Ok(out)
}

fn profile_allows_codex(cfg: &Config, name: &str) -> bool {
    cfg.models
        .profile_named(name)
        .is_some_and(|p| p.codex_background == CODEX_ALLOW)
}

fn profile_mut<'a>(c: &'a mut Config, name: &str) -> penelope_kernel::Result<&'a mut ModelProfile> {
    c.models
        .materialize(name)
        .ok_or_else(|| penelope_kernel::KernelError::config(format!("profil inconnu `{name}`")))
}

fn apply_set(
    c: &mut Config,
    name: &str,
    target: &Target,
    model: &str,
) -> penelope_kernel::Result<String> {
    let m = model.to_string();
    let at = format!("models.profiles.{name}");
    Ok(match target {
        Target::Alias(a) => {
            c.models.aliases.insert(a.clone(), m);
            format!("models.aliases.{a}")
        }
        Target::Voice(v) => {
            let field = match v.as_str() {
                "stt" => &mut c.voice.stt,
                "tts" => &mut c.voice.tts,
                _ => &mut c.voice.narrator,
            };
            *field = m;
            "voice".into()
        }
        Target::Primary => {
            profile_mut(c, name)?.primary = m;
            format!("{at}.primary")
        }
        Target::Tier(t) => {
            let r = &mut profile_mut(c, name)?.routing;
            *match t.as_str() {
                "low" => &mut r.low,
                "medium" => &mut r.medium,
                _ => &mut r.high,
            } = m;
            format!("{at}.routing.{t}")
        }
        Target::Capability(k) => {
            profile_mut(c, name)?.capabilities.insert(k.clone(), m);
            format!("{at}.capabilities.{k}")
        }
        Target::Role(r) => {
            profile_mut(c, name)?.overrides.insert(r.clone(), m);
            format!("{at}.overrides.{r}")
        }
    })
}

/// `model.unset` : le rôle revient au principal (ou au modèle livré de sa capacité).
pub fn unset(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let raw = str_of(p, "target")?;
    let target = Target::parse(&cfg, &raw);
    let name = profile_name(&cfg, p);
    let (t, n) = (target.clone(), name.clone());
    let g = s.publish_config("cli", move |c| {
        let at = format!("models.profiles.{n}");
        let path = match &t {
            Target::Primary => {
                return Err(penelope_kernel::KernelError::config(
                    "le principal ne se retire pas : `model set primary <modèle>`",
                ));
            }
            Target::Alias(a) => {
                return Err(penelope_kernel::KernelError::config(format!(
                    "`{a}` n'est pas un rôle : un alias se retire par `penelope config`"
                )));
            }
            Target::Voice(v) => {
                apply_set(c, &n, &t, "")?;
                format!("voice ({v})")
            }
            Target::Tier(tier) => {
                apply_set(c, &n, &t, "")?;
                format!("{at}.routing.{tier}")
            }
            Target::Capability(k) => {
                profile_mut(c, &n)?.capabilities.remove(k);
                format!("{at}.capabilities.{k}")
            }
            Target::Role(r) => {
                profile_mut(c, &n)?.overrides.remove(r);
                format!("{at}.overrides.{r}")
            }
        };
        Ok(vec![path])
    })?;
    let cfg = s.config.config();
    let follows = if cfg.models.active_name() == name {
        cfg.resolve_role_with(&raw, &s.catalog).model
    } else {
        None
    };
    Ok(json!({"generation": g, "profile": name, "target": raw, "model": follows}))
}

/// `model.profile` : `use`, `new`, `copy`, `rename`, `rm`.
pub fn profile(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let action = str_of(p, "action")?;
    let name = str_of(p, "name")?.trim().to_string();
    if name.is_empty() || name.contains(['.', '"', '\n']) {
        anyhow::bail!("nom de profil invalide : `{name}` (sans point ni guillemet)");
    }
    let exists = cfg.models.profile_named(&name).is_some();
    let guard = p.get("codex_background").and_then(Value::as_str);
    if let Some(g) = guard
        && ![CODEX_ALLOW, CODEX_DENY].contains(&g)
    {
        anyhow::bail!("`codex_background` vaut `allow` ou `deny` (reçu `{g}`)");
    }
    let n = name.clone();
    let g = match action.as_str() {
        "use" => {
            if !exists {
                anyhow::bail!(
                    "profil inconnu `{name}` (profils : {})",
                    cfg.models.profile_names().join(", ")
                );
            }
            s.publish_config("cli", move |c| {
                c.models.profile = n;
                Ok(vec!["models.profile".into()])
            })?
        }
        "new" | "copy" => {
            if exists {
                anyhow::bail!("le profil `{name}` existe déjà");
            }
            let mut base = if action == "copy" {
                let from = p
                    .get("from")
                    .and_then(Value::as_str)
                    .unwrap_or(cfg.models.active_name());
                cfg.models
                    .profile_named(from)
                    .ok_or_else(|| anyhow::anyhow!("profil inconnu `{from}`"))?
                    .into_owned()
            } else {
                // Un profil neuf : tout suit le principal ; les modèles de capacité
                // (images, embeddings) sont repris de l'actif, la voix est à part.
                ModelProfile {
                    primary: cfg.primary_label(),
                    capabilities: cfg.models.active().capabilities.clone(),
                    ..Default::default()
                }
            };
            if let Some(primary) = p.get("primary").and_then(Value::as_str) {
                check_label(&cfg, primary)?;
                base.primary = primary.to_string();
            }
            let codex = cfg.alias_model(&base.primary).is_some_and(is_codex);
            match guard {
                Some(g) => base.codex_background = g.to_string(),
                // Le choix est explicite pour un profil Codex (#333) : le risque est dit.
                None if codex && action == "new" => anyhow::bail!(
                    "profil Codex : choisir la garde du travail de fond, \
                     `codex_background` `deny` (le travail de fond passe ailleurs, en le \
                     disant) ou `allow` (tout passe par Codex). {CODEX_RISK}"
                ),
                None => {}
            }
            let switch = p.get("use").and_then(Value::as_bool).unwrap_or(false);
            s.publish_config("cli", move |c| {
                c.models.profiles.insert(n.clone(), base);
                if switch {
                    c.models.profile = n.clone();
                }
                Ok(vec![format!("models.profiles.{n}")])
            })?
        }
        "guard" => {
            let g =
                guard.ok_or_else(|| anyhow::anyhow!("paramètre `codex_background` manquant"))?;
            let g = g.to_string();
            s.publish_config("cli", move |c| {
                profile_mut(c, &n)?.codex_background = g;
                Ok(vec![format!("models.profiles.{n}.codex_background")])
            })?
        }
        "rename" => {
            let to = str_of(p, "to")?.trim().to_string();
            if name == DEFAULT_PROFILE || !exists {
                anyhow::bail!("`{name}` ne se renomme pas (profil inconnu, ou `defaut`)");
            }
            if to.is_empty()
                || to.contains(['.', '"', '\n'])
                || cfg.models.profile_named(&to).is_some()
            {
                anyhow::bail!("nom de profil invalide ou déjà pris : `{to}`");
            }
            s.publish_config("cli", move |c| {
                if let Some(pr) = c.models.profiles.remove(&n) {
                    c.models.profiles.insert(to.clone(), pr);
                }
                if c.models.profile == n {
                    c.models.profile = to.clone();
                }
                Ok(vec!["models.profiles".into(), "models.profile".into()])
            })?
        }
        "rm" => {
            if name == DEFAULT_PROFILE {
                anyhow::bail!("`defaut` ne se supprime pas : c'est le profil des clés d'avant");
            }
            if !cfg.models.profiles.contains_key(&name) {
                anyhow::bail!("profil inconnu `{name}`");
            }
            if cfg.models.active_name() == name {
                anyhow::bail!("`{name}` est le profil actif : basculer d'abord sur un autre");
            }
            s.publish_config("cli", move |c| {
                c.models.profiles.remove(&n);
                Ok(vec![format!("models.profiles.{n}")])
            })?
        }
        other => anyhow::bail!("action inconnue `{other}` (use, new, copy, guard, rename, rm)"),
    };
    let cfg = s.config.config();
    Ok(json!({
        "generation": g,
        "action": action,
        "profile": cfg.models.active_name(),
        "profiles": super::profile_rows(&cfg),
    }))
}
