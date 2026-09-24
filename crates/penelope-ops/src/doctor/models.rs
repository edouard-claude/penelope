//! Contrôles des modèles : rôles, raisonnement, appel d'outils, abonnement Codex.

use super::*;

/// Un alias qui sert un rôle d'extraction structurée : il rend du JSON, pas de la prose,
/// et son budget de sortie ne doit pas partir en raisonnement (issue #152).
pub fn alias_serves_extraction(cfg: &penelope_kernel::config::Config, alias: &str) -> bool {
    ["compaction", "memory_review"]
        .iter()
        .any(|role| cfg.role_alias(role) == alias)
}

/// #152 : ce que le modèle du rôle `compaction` fera de son budget de sortie.
///
/// La nuit du 20/09, il a dépensé 8 000 tokens à réfléchir sur un seul candidat et n'a
/// rendu aucune opération : `lightest_effort` renvoyait `high` parce qu'OpenRouter ne
/// déclare que `["xhigh","high"]` pour ce modèle. Une nuit sur deux échouait.
pub async fn reasoning_effort_check(s: &Services) -> DoctorCheck {
    const ID: &str = "reasoning_effort";
    const LABEL: &str = "Raisonnement de la consolidation";
    let cfg = s.config.config();
    let alias = cfg.role_alias("compaction");
    let Some(model) = cfg.alias_model(&alias).map(str::to_string) else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!("aucun modèle pour l'alias `{alias}` du rôle `compaction`"),
            Some(format!("penelope model set {alias} <fournisseur:modèle>")),
        );
    };
    let info = s
        .catalog
        .get(model.split_once(':').map_or(model.as_str(), |(_, m)| m));
    // Ce qui partira vraiment : le budget quand le raisonnement est gardé (le défaut),
    // l'effort quand la configuration l'éteint (issue #152).
    // Inconnu du catalogue : on suppose qu'il réfléchit, comme la passe (issue #152).
    let sent = if info.as_ref().is_some_and(|i| !i.reasons()) {
        "modèle sans raisonnement".to_string()
    } else if cfg.memory.consolidation_reasoning == "off" {
        match info.as_ref().and_then(|i| i.lightest_effort()).as_deref() {
            Some("none") => "raisonnement éteint".to_string(),
            Some(e) => format!("éteint demandé, mais le modèle l'impose : effort `{e}`"),
            None => "raisonnement éteint".to_string(),
        }
    } else {
        format!(
            "raisonnement gardé, budget jusqu'à {} jetons",
            cfg.memory.consolidation_reasoning_tokens
        )
    };
    // Part de raisonnement réellement observée sur sept jours.
    let since = (s.clock.now_utc() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let (reasoning, completion): (i64, i64) = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(reasoning), 0), COALESCE(SUM(completion), 0)
                 FROM usage WHERE ts >= ?1 AND role = 'consolidation'",
                [since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap_or((0, 0));
    if completion == 0 {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!("{alias} : {sent} ; aucune passe en 7 jours"),
        );
    }
    let share = (reasoning as f64 * 100.0 / completion as f64).round() as i64;
    let detail = format!("{alias} : {sent} ; {share}% de la sortie en raisonnement sur 7 jours");
    // Gardé et budgété, une part haute est normale (la nuit réussie du 19/09 était à
    // 73 %) ; c'est éteint qu'elle trahit un modèle qui n'écoute pas.
    if cfg.memory.consolidation_reasoning == "off" && share > 10 {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(format!(
                "le raisonnement est éteint mais le modèle réfléchit quand même : lui donner \
                 un modèle qui l'accepte (`penelope model set {alias} …`), ou repasser à \
                 `memory.consolidation_reasoning = \"auto\"`"
            )),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #152 : la consolidation tombe à l'heure où le Mac dort. La nuit du 21/09, un trou de
/// journal de douze minutes (« Now drawing from 'Battery Power' ») a coupé un lot en
/// vol ; la passe attend maintenant le retour du réseau, mais une machine sur batterie à
/// l'heure du rêve reste un avertissement.
pub async fn dream_power_check(s: &Services) -> DoctorCheck {
    const ID: &str = "dream_power";
    const LABEL: &str = "Alimentation à l'heure du rêve";
    let cfg = s.config.config();
    let cron = cfg.memory.dreaming_cron.clone();
    // Sonde bloquante (`pmset`) : hors du fil asynchrone.
    let platform = s.platform.clone();
    let now = s.clock.now_ms() / 1_000;
    let on_ac = tokio::task::spawn_blocking(move || platform.host_status(now).on_ac_power())
        .await
        .ok()
        .flatten();
    let detail = match on_ac {
        Some(false) => format!("sur batterie ; consolidation prévue à `{cron}`"),
        Some(true) => format!("sur secteur ; consolidation prévue à `{cron}`"),
        None => format!("alimentation inconnue ; consolidation prévue à `{cron}`"),
    };
    if on_ac == Some(false) {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(
                "une machine sur batterie s'endort et coupe un lot en vol : la brancher avant \
                 la nuit, ou décaler `memory.dreaming_cron`"
                    .into(),
            ),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// #142 : état du fournisseur `codex` — connexion, fraîcheur des jetons, périmètre des
/// alias, et l'avertissement permanent sur l'identité empruntée.
pub async fn codex_checks(s: &Services) -> Vec<DoctorCheck> {
    let cfg = s.config.config();
    let c = &cfg.providers.codex;
    let mut out = Vec::new();
    let codex_aliases: Vec<(&String, &String)> = cfg
        .models
        .aliases
        .iter()
        .filter(|(_, m)| crate::codex_scope::is_codex(m))
        .collect();
    if !c.enabled && codex_aliases.is_empty() {
        return out;
    }

    // Connexion et jetons.
    out.push(match crate::codex_auth::status(s) {
        Ok(Some(st)) if st.connected => {
            let now = s.clock.now_ms();
            let expires_in = (st.expires_at_ms - now) / 60_000;
            let age_days = (now - st.last_refresh_ms) / 86_400_000;
            DoctorCheck::ok(
                "provider.codex",
                "Fournisseur Codex",
                format!(
                    "connecté : compte {}, plan {}, jeton valable {expires_in} min, \
                     rafraîchi il y a {age_days} j",
                    st.account, st.plan
                ),
            )
        }
        Ok(Some(st)) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            format!(
                "compte déconnecté ({}) : les alias `codex:` se replient",
                st.disconnected.unwrap_or_default()
            ),
            Some("penelope model auth codex".into()),
        ),
        Ok(None) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            "activé, mais aucun compte ChatGPT connecté",
            Some("penelope model auth codex".into()),
        ),
        Err(e) => DoctorCheck::fail(
            "provider.codex",
            "Fournisseur Codex",
            format!("connexion illisible : {e}"),
            Some("penelope model auth codex --logout puis se reconnecter".into()),
        ),
    });

    // Identité empruntée : un avertissement qui ne se tait jamais.
    out.push(DoctorCheck::ok(
        "provider.codex.identity",
        "Identité Codex",
        format!(
            "`originator: {}`, client {} — Pénélope emprunte l'identité de Codex CLI. \
             Usage toléré par OpenAI, jamais garanti : il peut cesser du jour au \
             lendemain (repli : une clé d'API sur `openai_compat`).",
            c.originator, c.client_version
        ),
    ));

    // Périmètre : un alias de rôle de fond qui vise l'abonnement l'aurait contourné.
    let mut hors: Vec<String> = Vec::new();
    for (alias, model) in &codex_aliases {
        let roles = crate::codex_scope::background_roles_of(&cfg, alias);
        if !roles.is_empty() {
            hors.push(format!("`{alias}` → `{model}` ({})", roles.join(", ")));
        }
    }
    if !hors.is_empty() {
        out.push(DoctorCheck::fail(
            "provider.codex.scope",
            "Périmètre Codex",
            format!(
                "{} : ces rôles tournent sans le propriétaire ; l'abonnement ne les sert \
                 pas, chaque appel se replie",
                hors.join(", ")
            ),
            Some("penelope model set <alias> openrouter:<modèle>".into()),
        ));
    }

    // Jauges du plan : ce qui borne vraiment, puisque le coût est nul.
    if let Some(q) = crate::codex_quota::snapshot(s).await {
        let line = crate::codex_quota::gauge_line(&q, s.clock.now_ms());
        let ratio = q.worst_ratio();
        out.push(if ratio < c.quota_stop_ratio {
            DoctorCheck::ok("provider.codex.quota", "Quota du plan ChatGPT", line)
        } else {
            DoctorCheck::fail(
                "provider.codex.quota",
                "Quota du plan ChatGPT",
                format!("{line} : Pénélope est en retrait, les tours passent par OpenRouter"),
                None,
            )
        });
    }
    out
}

/// Rôles qui appellent des outils : un alias qui les sert doit viser un modèle avec tool
/// calling (issue #54). Les rôles de service (`stt`, `tts`, `embeddings`, `classifier`,
/// `summarizer`, `titler`) n'en appellent pas.
const TOOLLESS_ROLES: &[&str] = &[
    "stt",
    "tts",
    "embeddings",
    "classifier",
    "summarizer",
    "titler",
    "vision",
    "image_describe",
    "image_locate",
    "image_generate",
    "embedding",
];

/// Vrai si cet alias sert un rôle (ou un palier de routage) qui appelle des outils.
pub fn alias_needs_tools(cfg: &penelope_kernel::config::Config, alias: &str) -> bool {
    let routing = &cfg.models.routing;
    if [&routing.low, &routing.medium, &routing.high]
        .iter()
        .any(|a| a.as_str() == alias)
    {
        return true;
    }
    cfg.models
        .roles
        .iter()
        .any(|(role, a)| a == alias && !TOOLLESS_ROLES.contains(&role.as_str()))
}

/// #54 : un alias de conversation qui vise un modèle sans tool calling ne marchera pas,
/// et rien ne l'émule.
pub(super) async fn tool_calling_check(s: &Services) -> DoctorCheck {
    const ID: &str = "models.tools";
    const LABEL: &str = "Modèles et outils";
    let cfg = s.config.config();
    if s.catalog.is_empty() {
        return DoctorCheck::ok(ID, LABEL, "catalogue pas encore chargé");
    }
    let mut sans: Vec<String> = Vec::new();
    for (alias, model) in &cfg.models.aliases {
        if !alias_needs_tools(&cfg, alias) {
            continue;
        }
        let bare = penelope_llm::catalog::strip_provider(model);
        if s.catalog.get(bare).map(|i| i.supports_tools()) == Some(false) {
            sans.push(format!("`{alias}` → `{model}`"));
        }
    }
    if sans.is_empty() {
        DoctorCheck::ok(
            ID,
            LABEL,
            "tous les alias de conversation appellent des outils",
        )
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "{} n'appelle(nt) pas d'outils : ces alias servent un rôle qui en a besoin",
                sans.join(", ")
            ),
            Some("penelope model set <alias> <modèle avec tool calling>".into()),
        )
    }
}
