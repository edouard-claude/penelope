//! Validation **bloquante** des workflows (§12.6).
//!
//! Un fichier invalide est rejeté : notification Telegram et CLI avec chemin JSON et
//! message, **la version précédente reste active**.

use crate::conditions::{is_dynamic_var, known_static_vars};
use crate::model::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Chemin JSON de l'élément fautif, par exemple `/steps/3/transitions/0/goto`.
    pub path: String,
    pub message: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self.severity {
            Severity::Error => "erreur",
            Severity::Warning => "avertissement",
        };
        write!(f, "{s} {} : {}", self.path, self.message)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub issues: Vec<Issue>,
}

impl Report {
    pub fn errors(&self) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect()
    }
    pub fn warnings(&self) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .collect()
    }
    pub fn is_valid(&self) -> bool {
        self.errors().is_empty()
    }
    pub fn render(&self) -> String {
        if self.issues.is_empty() {
            return "workflow valide".into();
        }
        self.issues
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            message: message.into(),
            severity: Severity::Error,
        });
    }
    fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            message: message.into(),
            severity: Severity::Warning,
        });
    }
}

/// Ce que le harnais connaît au moment de valider.
#[derive(Debug, Default, Clone)]
pub struct Known {
    pub workflow_ids: BTreeSet<String>,
    pub model_aliases: BTreeSet<String>,
    pub native_tools: BTreeSet<String>,
    /// Outils MCP disponibles à l'instant T : leur absence n'est qu'un avertissement.
    pub mcp_tools: BTreeSet<String>,
    pub templates: BTreeSet<String>,
    pub max_depth: u32,
}

/// Valide un workflow, en s'appuyant sur le nom de fichier pour vérifier l'identifiant.
pub fn validate(w: &Workflow, file_stem: Option<&str>, known: &Known) -> Report {
    let mut r = Report::default();

    // --- métadonnées ---
    if w.metadata.id.is_empty() {
        r.error("/metadata/id", "identifiant obligatoire");
    } else if penelope_platform::validate_slug(&w.metadata.id).is_err() {
        r.error(
            "/metadata/id",
            format!("`{}` doit être un slug [a-z0-9-]", w.metadata.id),
        );
    }
    if let Some(stem) = file_stem {
        let expected = stem.trim_end_matches(".workflow");
        if !w.metadata.id.is_empty() && w.metadata.id != expected {
            r.error(
                "/metadata/id",
                format!(
                    "l'identifiant `{}` doit être égal au nom du fichier `{expected}`",
                    w.metadata.id
                ),
            );
        }
    }
    for (i, p) in w.metadata.parameters.iter().enumerate() {
        if p.id.is_empty() {
            r.error(format!("/metadata/parameters/{i}/id"), "identifiant vide");
        }
        if !matches!(
            p.kind.as_str(),
            "string" | "number" | "integer" | "boolean" | "enum"
        ) {
            r.error(
                format!("/metadata/parameters/{i}/type"),
                format!("type inconnu : `{}`", p.kind),
            );
        }
    }
    for (i, p) in w.metadata.platforms.iter().enumerate() {
        if !matches!(p.as_str(), "macos" | "linux" | "windows") {
            r.error(
                format!("/metadata/platforms/{i}"),
                format!("plateforme inconnue : `{p}`"),
            );
        }
    }

    // --- budget global obligatoire ---
    if w.settings.budget.max_usd <= 0.0
        && w.settings.budget.max_tokens == 0
        && w.settings.budget.max_wall_ms == 0
    {
        r.error("/settings/budget", "un budget global est obligatoire");
    }
    if !matches!(
        w.settings.concurrency.admission.as_str(),
        "parallel" | "hold" | "coalesce" | "drop"
    ) {
        r.error(
            "/settings/concurrency/admission",
            format!("valeur inconnue : `{}`", w.settings.concurrency.admission),
        );
    }
    if !(w.settings.workspace == "ephemeral" || w.settings.workspace.starts_with("persistent:")) {
        r.error(
            "/settings/workspace",
            format!(
                "attendu `ephemeral` ou `persistent:<nom>`, reçu `{}`",
                w.settings.workspace
            ),
        );
    }

    // --- identifiants uniques ---
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (i, s) in w.steps.iter().enumerate() {
        if s.id.is_empty() {
            r.error(format!("/steps/{i}/id"), "identifiant d'étape vide");
        } else if !seen.insert(&s.id) {
            r.error(
                format!("/steps/{i}/id"),
                format!("identifiant dupliqué : `{}`", s.id),
            );
        }
        if s.id.starts_with('$') {
            r.error(
                format!("/steps/{i}/id"),
                "un identifiant d'étape ne peut pas commencer par `$`",
            );
        }
    }

    if w.steps.is_empty() {
        r.error("/steps", "aucune étape");
        return r;
    }

    // --- entryStep ---
    if w.step(&w.entry_step).is_none() {
        r.error(
            "/entryStep",
            format!("`{}` ne correspond à aucune étape", w.entry_step),
        );
    }

    // --- étapes ---
    let declared_params: BTreeSet<String> =
        w.metadata.parameters.iter().map(|p| p.id.clone()).collect();

    for (i, s) in w.steps.iter().enumerate() {
        validate_step(
            &mut r,
            s,
            &format!("/steps/{i}"),
            w,
            known,
            &declared_params,
            false,
        );
    }

    // --- atteignabilité de `$done` ---
    if !reaches_done(w) {
        r.error(
            "/steps",
            format!("`{DONE}` n'est pas atteignable depuis `{}`", w.entry_step),
        );
    }

    // --- sous-workflows : existence et absence de cycle ---
    check_subworkflows(&mut r, w, known);

    // --- sous-groupes : on ne quitte une boucle que par une transition taguée ---
    check_subgroups(&mut r, w);

    // --- clés de métadonnées jamais écrites ---
    check_metadata_keys(&mut r, w);

    r
}

fn validate_step(
    r: &mut Report,
    s: &Step,
    path: &str,
    w: &Workflow,
    known: &Known,
    declared_params: &BTreeSet<String>,
    is_child: bool,
) {
    if !STEP_KINDS.contains(&s.kind.as_str()) {
        r.error(
            format!("{path}/type"),
            format!("type inconnu : `{}`", s.kind),
        );
        return;
    }
    if is_child && !PARALLEL_CHILD_KINDS.contains(&s.kind.as_str()) {
        r.error(
            format!("{path}/type"),
            format!(
                "`{}` est interdit comme enfant d'un `parallel` (autorisés : {})",
                s.kind,
                PARALLEL_CHILD_KINDS.join(", ")
            ),
        );
    }

    // Champs obligatoires par type.
    match s.kind.as_str() {
        "agent" => {
            if s.prompt.trim().is_empty() {
                r.error(format!("{path}/prompt"), "prompt vide");
            }
        }
        "sub_agent" => {
            if s.prompt.trim().is_empty() {
                r.error(format!("{path}/prompt"), "prompt vide");
            }
            if let Some(schema) = &s.output_schema
                && schema.get("type").is_none()
            {
                r.error(format!("{path}/outputSchema"), "schéma sans `type`");
            }
        }
        "shell" => {
            if s.command.is_null() {
                r.error(format!("{path}/command"), "commande absente");
            } else {
                for os in &w.metadata.platforms {
                    if s.command_for_os(os).is_none() {
                        r.error(
                            format!("{path}/command"),
                            format!(
                                "aucune commande pour `{os}`, pourtant déclaré dans `platforms`"
                            ),
                        );
                    }
                }
            }
        }
        "tool" => {
            if s.tool.is_empty() {
                r.error(format!("{path}/tool"), "outil absent");
            } else if s.tool.starts_with("mcp__") {
                if !known.mcp_tools.is_empty() && !known.mcp_tools.contains(&s.tool) {
                    r.warn(
                        format!("{path}/tool"),
                        format!("outil MCP `{}` momentanément indisponible", s.tool),
                    );
                }
            } else if !known.native_tools.is_empty() && !known.native_tools.contains(&s.tool) {
                r.error(
                    format!("{path}/tool"),
                    format!("outil natif inconnu : `{}`", s.tool),
                );
            }
        }
        "user" => {
            if s.template.is_empty() {
                r.error(format!("{path}/template"), "template absent");
            } else if !known.templates.is_empty() && !known.templates.contains(&s.template) {
                r.error(
                    format!("{path}/template"),
                    format!("template inconnu : `{}`", s.template),
                );
            }
            if s.choices.is_empty() {
                r.error(
                    format!("{path}/choices"),
                    "une étape `user` doit offrir des choix",
                );
            }
            if let Some(id) = s.input.strip_prefix("form:") {
                match w.settings.forms.get(id) {
                    None => r.error(
                        format!("{path}/input"),
                        format!("formulaire `{id}` absent de `settings.forms`"),
                    ),
                    Some(schema) => {
                        let fields = schema
                            .get("properties")
                            .and_then(|p| p.as_object())
                            .map(|p| p.len())
                            .unwrap_or(0);
                        if schema.get("type").and_then(|t| t.as_str()) != Some("object")
                            || fields == 0
                        {
                            r.error(
                                format!("/settings/forms/{id}"),
                                "un formulaire est un schéma `object` avec au moins une propriété",
                            );
                        }
                    }
                }
            } else if !(s.input == "none" || s.input == "text") {
                r.error(
                    format!("{path}/input"),
                    format!(
                        "attendu `none`, `text` ou `form:<schema>`, reçu `{}`",
                        s.input
                    ),
                );
            }
        }
        "parallel" => {
            if s.children.is_empty() {
                r.error(format!("{path}/children"), "aucun enfant");
            }
            for (j, c) in s.children.iter().enumerate() {
                validate_step(
                    r,
                    c,
                    &format!("{path}/children/{j}"),
                    w,
                    known,
                    declared_params,
                    true,
                );
            }
        }
        "workflow" => {
            if s.workflow_id.is_empty() {
                r.error(format!("{path}/workflowId"), "sous-workflow absent");
            } else if !known.workflow_ids.is_empty() && !known.workflow_ids.contains(&s.workflow_id)
            {
                r.error(
                    format!("{path}/workflowId"),
                    format!("workflow inconnu : `{}`", s.workflow_id),
                );
            }
        }
        "wait" => {
            let ok = s.on.get("event").is_some()
                || s.on.get("cron").is_some()
                || s.on.get("duration_ms").is_some()
                || s.on.get("mcp_task").is_some();
            if !ok {
                r.error(
                    format!("{path}/on"),
                    "attendu `event`, `cron`, `duration_ms` ou `mcp_task`",
                );
            }
            if let Some(c) = s.on.get("cron").and_then(|v| v.as_str())
                && let Err(e) = penelope_kernel::cron::Cron::parse(c)
            {
                r.error(format!("{path}/on/cron"), e.to_string());
            }
        }
        "verify" if s.verifier.is_empty() && s.checks.is_empty() => {
            r.error(
                format!("{path}/verifier"),
                "une étape `verify` doit avoir un vérificateur ou des contrôles",
            );
        }
        _ => {}
    }

    // Alias de modèle.
    if !s.model.is_empty()
        && !known.model_aliases.is_empty()
        && !known.model_aliases.contains(&s.model)
    {
        r.error(
            format!("{path}/model"),
            format!("alias de modèle inconnu : `{}`", s.model),
        );
    }

    // Transitions.
    if !is_child {
        validate_transitions(r, s, path, w);
    }

    // Variables de template.
    for (field, text) in [
        ("prompt", s.prompt.as_str()),
        ("nudgePrompt", s.nudge_prompt.as_str()),
        ("cwd", s.cwd.as_str()),
    ] {
        check_vars(r, &format!("{path}/{field}"), text, declared_params);
    }
    if let Value::String(cmd) = &s.command {
        check_vars(r, &format!("{path}/command"), cmd, declared_params);
    }
    if let Some(args) = s.args.as_object() {
        for (k, v) in args {
            if let Value::String(t) = v {
                check_vars(r, &format!("{path}/args/{k}"), t, declared_params);
            }
        }
    }
}

fn validate_transitions(r: &mut Report, s: &Step, path: &str, w: &Workflow) {
    if s.transitions.is_empty() {
        r.error(format!("{path}/transitions"), "aucune transition");
        return;
    }
    let valid_targets: BTreeSet<String> = w
        .step_ids()
        .into_iter()
        .chain([DONE.to_string(), BLOCKED.to_string()])
        .collect();

    for (i, t) in s.transitions.iter().enumerate() {
        if !valid_targets.contains(&t.goto) {
            r.error(
                format!("{path}/transitions/{i}/goto"),
                format!("cible inconnue : `{}`", t.goto),
            );
        }
        if t.condition.get("type").is_none() {
            r.error(
                format!("{path}/transitions/{i}/condition"),
                "condition sans `type`",
            );
        }
    }

    // Une transition `always` doit être en dernière position, sauf étape `user` dont les
    // choix sont exhaustifs (§12.6).
    let last_is_always = s.transitions.last().map(|t| t.is_always()).unwrap_or(false);
    let always_positions: Vec<usize> = s
        .transitions
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_always())
        .map(|(i, _)| i)
        .collect();

    if let Some(&first) = always_positions.first()
        && first != s.transitions.len() - 1
    {
        r.error(
            format!("{path}/transitions/{first}"),
            "une transition `always` doit être la dernière : celles qui suivent sont \
                 inatteignables",
        );
    }

    if !last_is_always {
        if s.kind == "user" {
            let covered: BTreeSet<String> = s
                .transitions
                .iter()
                .filter_map(|t| {
                    t.condition
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect();
            for c in &s.choices {
                if !covered.contains(c) {
                    r.error(
                        format!("{path}/transitions"),
                        format!("le choix `{c}` n'a pas de transition"),
                    );
                }
            }
        } else {
            r.error(
                format!("{path}/transitions"),
                "aucune transition `always` en dernière position",
            );
        }
    }
}

/// Sous-groupes (§12.7, sémantique OpenFox) : les étapes d'un même `subGroup` forment une
/// tranche qui boucle. Une transition qui en sort (vers une étape d'un autre groupe ou vers
/// `$done`) porte un `tag` qui dit pourquoi la boucle s'arrête ; `$blocked` reste l'issue
/// d'échec implicite. Chaque sous-groupe a au moins une sortie taguée.
pub fn check_subgroups(r: &mut Report, w: &Workflow) {
    let group_of: BTreeMap<&str, &str> = w
        .steps
        .iter()
        .filter(|s| !s.sub_group.is_empty())
        .map(|s| (s.id.as_str(), s.sub_group.as_str()))
        .collect();
    let mut escapes: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, s) in w.steps.iter().enumerate() {
        if s.sub_group.is_empty() {
            continue;
        }
        escapes.entry(s.sub_group.as_str()).or_insert(0);
        for (j, t) in s.transitions.iter().enumerate() {
            let path = format!("/steps/{i}/transitions/{j}");
            let leaving = t.goto != BLOCKED
                && group_of.get(t.goto.as_str()).copied() != Some(s.sub_group.as_str());
            match (leaving, t.tag.is_empty()) {
                (true, true) => r.error(
                    format!("{path}/tag"),
                    format!(
                        "la transition vers `{}` sort du sous-groupe `{}` sans `tag` : une \
                         boucle ne se quitte que par une transition taguée",
                        t.goto, s.sub_group
                    ),
                ),
                (true, false) => {
                    *escapes.entry(s.sub_group.as_str()).or_insert(0) += 1;
                }
                (false, false) => r.warn(
                    format!("{path}/tag"),
                    format!(
                        "`tag` sans effet : `{}` reste dans le sous-groupe `{}`",
                        t.goto, s.sub_group
                    ),
                ),
                (false, true) => {}
            }
        }
    }
    for (group, n) in escapes {
        if n == 0 {
            r.error(
                "/steps",
                format!(
                    "le sous-groupe `{group}` n'a aucune sortie taguée : il ne peut que boucler"
                ),
            );
        }
    }
}

/// Tag de la transition qui fait sortir `from` de son sous-groupe vers `next`, s'il y en a
/// une.
pub fn escape_tag<'a>(
    w: &'a Workflow,
    from: &'a crate::model::Step,
    next: &str,
) -> Option<&'a str> {
    if from.sub_group.is_empty() || next == BLOCKED {
        return None;
    }
    let same = w.step(next).is_some_and(|n| n.sub_group == from.sub_group);
    if same {
        return None;
    }
    from.transitions
        .iter()
        .find(|t| t.goto == next && !t.tag.is_empty())
        .map(|t| t.tag.as_str())
}

fn check_vars(r: &mut Report, path: &str, text: &str, declared_params: &BTreeSet<String>) {
    if text.is_empty() {
        return;
    }
    for name in crate::conditions::placeholders(text) {
        if is_dynamic_var(&name) || known_static_vars().contains(&name.as_str()) {
            continue;
        }
        let bare = name.strip_prefix("params.").unwrap_or(&name);
        if declared_params.contains(bare) {
            continue;
        }
        r.error(path, format!("variable inconnue : `{{{{{name}}}}}`"));
    }
}

/// Parcours depuis `entryStep` : `$done` doit être atteignable.
fn reaches_done(w: &Workflow) -> bool {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = vec![w.entry_step.as_str()];
    while let Some(id) = stack.pop() {
        if id == DONE {
            return true;
        }
        if id == BLOCKED || !seen.insert(id) {
            continue;
        }
        let Some(s) = w.step(id) else { continue };
        for t in &s.transitions {
            stack.push(&t.goto);
        }
    }
    false
}

fn check_subworkflows(r: &mut Report, w: &Workflow, known: &Known) {
    let max_depth = if known.max_depth == 0 {
        3
    } else {
        known.max_depth
    };
    let children: Vec<&str> = w
        .steps
        .iter()
        .filter(|s| s.kind == "workflow")
        .map(|s| s.workflow_id.as_str())
        .collect();
    for c in children {
        if c == w.metadata.id {
            r.error(
                "/steps",
                format!("cycle de sous-workflows : `{c}` s'appelle lui-même"),
            );
        }
    }
    // La profondeur réelle est vérifiée à l'exécution (les définitions ne sont pas toutes
    // chargées ici) ; on rappelle la borne dans un avertissement si le workflow en
    // appelle d'autres.
    if w.steps.iter().any(|s| s.kind == "workflow") {
        r.warn(
            "/steps",
            format!("profondeur d'imbrication limitée à {max_depth} à l'exécution"),
        );
    }
}

/// Clés de métadonnées lues par une condition mais jamais écrites (§12.4, avertissement).
fn check_metadata_keys(r: &mut Report, w: &Workflow) {
    let mut read: BTreeMap<String, String> = BTreeMap::new();
    for (i, s) in w.steps.iter().enumerate() {
        for (j, t) in s.transitions.iter().enumerate() {
            collect_metadata_keys(
                &t.condition,
                &mut read,
                &format!("/steps/{i}/transitions/{j}"),
            );
        }
    }
    // Une clé est réputée écrite si une étape `agent` ou `verify` la mentionne.
    let written: BTreeSet<String> = w
        .steps
        .iter()
        .flat_map(|s| {
            let mut v = Vec::new();
            if s.kind == "verify" {
                v.push(s.criteria_key.clone());
            }
            if !s.prompt.is_empty() {
                for key in read.keys() {
                    if s.prompt.contains(key.as_str()) {
                        v.push(key.clone());
                    }
                }
            }
            v
        })
        .collect();

    for (key, path) in read {
        if !written.contains(&key) {
            r.warn(
                path,
                format!(
                    "la clé de métadonnées `{key}` n'est jamais écrite : la condition \
                         sera vraie par vacuité"
                ),
            );
        }
    }
}

fn collect_metadata_keys(cond: &Value, out: &mut BTreeMap<String, String>, path: &str) {
    match cond.get("type").and_then(|t| t.as_str()) {
        Some("metadata_all_match") | Some("metadata_any_match") | Some("metadata_all_in") => {
            if let Some(k) = cond.get("key").and_then(|k| k.as_str()) {
                out.entry(k.to_string()).or_insert_with(|| path.to_string());
            }
        }
        Some("all") | Some("any") => {
            if let Some(a) = cond.get("of").and_then(|o| o.as_array()) {
                for c in a {
                    collect_metadata_keys(c, out, path);
                }
            }
        }
        Some("not") => {
            if let Some(c) = cond.get("cond") {
                collect_metadata_keys(c, out, path);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
