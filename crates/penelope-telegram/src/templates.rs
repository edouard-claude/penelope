//! Catalogue de templates Telegram (§14.5).
//!
//! Les templates sont en **TOML rechargé à chaud**. Chaque bouton porte une action
//! (§14.8) et un style. Un bouton non applicable est affiché **désactivé**, jamais retiré.

use crate::render::{Block, ButtonSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Template {
    pub id: String,
    /// Corps en Markdown, avec variables `{{nom}}`.
    pub body: String,
    /// Variables attendues : la validation au chargement empêche un template cassé.
    pub variables: Vec<String>,
    pub buttons: Vec<Vec<ButtonDef>>,
    /// Repli HTML explicite ; vide = dérivé du corps.
    pub fallback_html: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ButtonDef {
    pub label: String,
    pub action: String,
    pub style: String,
    /// Bouton URL : le libellé pointe vers `{{variable}}`.
    pub url: String,
}

/// Rendu d'un template : blocs, HTML de repli, spécifications de boutons.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub blocks: Vec<Block>,
    pub html: String,
    pub buttons: Vec<Vec<ButtonSpec>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    pub id: String,
    pub message: String,
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "template `{}` : {}", self.id, self.message)
    }
}

impl Template {
    pub fn validate(&self) -> Result<(), TemplateError> {
        let fail = |m: &str| TemplateError {
            id: self.id.clone(),
            message: m.to_string(),
        };
        if self.id.is_empty() {
            return Err(fail("`id` est obligatoire"));
        }
        if self.body.trim().is_empty() {
            return Err(fail("`body` est vide"));
        }
        // Toute variable utilisée doit être déclarée : sinon une carte part avec un trou.
        for v in placeholders(&self.body) {
            if !self.variables.contains(&v) {
                return Err(fail(&format!("variable `{v}` utilisée mais non déclarée")));
            }
        }
        for row in &self.buttons {
            for b in row {
                if b.label.is_empty() {
                    return Err(fail("bouton sans libellé"));
                }
                if b.action.is_empty() && b.url.is_empty() {
                    return Err(fail(&format!("bouton `{}` sans action ni url", b.label)));
                }
                if !b.action.is_empty() && !crate::actions::kind::ALL.contains(&b.action.as_str()) {
                    return Err(fail(&format!(
                        "action inconnue : `{}` (bouton `{}`)",
                        b.action, b.label
                    )));
                }
                if !matches!(b.style.as_str(), "" | "primary" | "success" | "danger") {
                    return Err(fail(&format!("style inconnu : `{}`", b.style)));
                }
            }
        }
        Ok(())
    }

    /// Rend le template. `tokens` associe une action à son jeton opaque.
    pub fn render(
        &self,
        vars: &BTreeMap<String, String>,
        tokens: &BTreeMap<String, String>,
        disabled: &[String],
    ) -> Result<Rendered, TemplateError> {
        let body = substitute(&self.body, vars);
        let missing: Vec<String> = placeholders(&body);
        if !missing.is_empty() {
            return Err(TemplateError {
                id: self.id.clone(),
                message: format!("variables non fournies : {}", missing.join(", ")),
            });
        }
        let blocks = crate::render::to_blocks(&body);
        let html = if self.fallback_html.is_empty() {
            crate::render::to_html(&blocks)
        } else {
            substitute(&self.fallback_html, vars)
        };

        let buttons: Vec<Vec<ButtonSpec>> = self
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .map(|b| {
                        let mut spec = if !b.url.is_empty() {
                            ButtonSpec::url(&b.label, &substitute(&b.url, vars))
                        } else {
                            ButtonSpec::callback(
                                &b.label,
                                tokens
                                    .get(&b.action)
                                    .map(|s| s.as_str())
                                    .unwrap_or("a:none"),
                                &b.style,
                            )
                        };
                        if disabled.contains(&b.action) {
                            spec = spec.disabled();
                        }
                        spec
                    })
                    .collect()
            })
            .collect();

        Ok(Rendered {
            blocks,
            html,
            buttons,
        })
    }

    /// Actions référencées par le template : le harnais sait quels jetons créer.
    pub fn actions(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .buttons
            .iter()
            .flatten()
            .filter(|b| !b.action.is_empty())
            .map(|b| b.action.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Variables `{{nom}}` d'un texte.
pub fn placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
        rest = &after[end + 2..];
    }
    out
}

fn substitute(s: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Registre des templates, rechargé à chaud.
#[derive(Debug, Clone, Default)]
pub struct TemplateRegistry {
    templates: BTreeMap<String, Template>,
    errors: Vec<TemplateError>,
}

impl TemplateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge le catalogue livré, puis les surcharges du disque.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        for t in builtin_templates() {
            r.templates.insert(t.id.clone(), t);
        }
        r
    }

    pub fn load_dir(&mut self, dir: &Path) -> usize {
        self.errors.clear();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
            .collect();
        paths.sort();

        let mut n = 0;
        for p in paths {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|raw| toml::from_str::<Template>(&raw).map_err(|e| e.to_string()))
            {
                Ok(mut t) => {
                    if t.id.is_empty() {
                        t.id = id.clone();
                    }
                    match t.validate() {
                        Ok(()) => {
                            self.templates.insert(t.id.clone(), t);
                            n += 1;
                        }
                        Err(e) => self.errors.push(e),
                    }
                }
                Err(e) => self.errors.push(TemplateError { id, message: e }),
            }
        }
        n
    }

    pub fn get(&self, id: &str) -> Option<&Template> {
        self.templates.get(id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.templates.keys().cloned().collect()
    }

    pub fn errors(&self) -> &[TemplateError] {
        &self.errors
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }
}

/// Identifiants du catalogue du §14.5.
pub const CATALOG: &[&str] = &[
    "answer",
    "tool_approval",
    "destructive_confirm",
    "plan_proposal",
    "deploy_gate",
    "run_card",
    "run_done",
    "run_blocked",
    "incident",
    "ticket_detected",
    "question",
    "form",
    "mcp_oauth_required",
    "mcp_url_elicitation",
    "sampling_request",
    "effect_unknown",
    "skill_proposal",
    "memory_proposal",
    "learned",
    "schedule_preview",
    "workflow_preview",
    "budget_alert",
    "mcp_status",
    "digest",
    "heartbeat",
    "stopped",
];

fn t(id: &str, body: &str, variables: &[&str], buttons: Vec<Vec<ButtonDef>>) -> Template {
    Template {
        id: id.into(),
        body: body.into(),
        variables: variables.iter().map(|s| s.to_string()).collect(),
        buttons,
        fallback_html: String::new(),
    }
}

fn b(label: &str, action: &str, style: &str) -> ButtonDef {
    ButtonDef {
        label: label.into(),
        action: action.into(),
        style: style.into(),
        url: String::new(),
    }
}

fn burl(label: &str, url: &str) -> ButtonDef {
    ButtonDef {
        label: label.into(),
        action: String::new(),
        style: String::new(),
        url: url.into(),
    }
}

/// Catalogue livré : un template par ligne du tableau §14.5.
pub fn builtin_templates() -> Vec<Template> {
    use crate::actions::kind as k;
    vec![
        t(
            "answer",
            "{{corps}}",
            &["corps"],
            vec![vec![
                b("🔁 Régénérer", k::REGENERATE, ""),
                b("🧠 Modèle supérieur", k::ESCALATE_MODEL, ""),
                b("📌 Mémoriser", k::MEMORISE, ""),
            ]],
        ),
        t(
            "tool_approval",
            "**Approbation demandée**\n\n\
             Outil : `{{outil}}`\nServeur : {{serveur}}\nRisque : **{{risque}}**\n\n\
             Arguments :\n```json\n{{arguments}}\n```\n\n\
             Raison donnée : {{raison}}\n{{alerte}}",
            &[
                "outil",
                "serveur",
                "risque",
                "arguments",
                "raison",
                "alerte",
            ],
            vec![
                vec![
                    b("✅ Autoriser", k::APPROVE, "success"),
                    b("✅ Pour ce run", k::APPROVE_RUN, ""),
                ],
                vec![
                    b("♾️ Toujours", k::APPROVE_ALWAYS, ""),
                    b("❌ Refuser", k::DENY, "danger"),
                    b("✏️ Refuser avec raison", k::DENY_REASON, ""),
                ],
            ],
        ),
        t(
            "destructive_confirm",
            "⚠️ **Seconde confirmation**\n\n{{rappel}}",
            &["rappel"],
            vec![vec![
                b("⚠️ Confirmer", k::CONFIRM_DESTRUCTIVE, "danger"),
                b("Annuler", k::DENY, ""),
            ]],
        ),
        t(
            "plan_proposal",
            "**Proposition**\n\n\
             Ticket : {{ticket}}\nCause probable : {{cause}}\n\n\
             Fichiers visés :\n{{fichiers}}\n\nCritères :\n{{criteres}}\n\n\
             Estimation : {{cout}} · {{duree}}",
            &["ticket", "cause", "fichiers", "criteres", "cout", "duree"],
            vec![vec![
                b("▶️ Appliquer", k::APPROVE, "primary"),
                b("✏️ Réviser", k::DENY_REASON, ""),
                b("🗑 Rejeter", k::DENY, "danger"),
            ]],
        ),
        t(
            "deploy_gate",
            "**Avant déploiement**\n\n\
             PR/MR : {{pr}}\nTests : {{tests}}\nEnvironnement : {{environnement}}\n\n{{diff}}",
            &["pr", "tests", "environnement", "diff"],
            vec![vec![
                b("🚀 Déployer", k::APPROVE, "success"),
                b("⏳ Attendre la review", k::CHOICE, ""),
                b("✋ Annuler", k::DENY, "danger"),
            ]],
        ),
        t(
            "run_card",
            "**{{nom}}**\n\n\
             Étape : {{etape}} ({{phase}})\nItérations : {{iterations}}/{{max_iterations}}\n\
             Budget : {{budget}}\nDurée : {{duree}}\n\nCritères :\n{{criteres}}",
            &[
                "nom",
                "etape",
                "phase",
                "iterations",
                "max_iterations",
                "budget",
                "duree",
                "criteres",
            ],
            vec![
                vec![
                    b("⏸ Pause", k::RUN_PAUSE, ""),
                    b("▶️ Reprendre", k::RUN_RESUME, ""),
                    b("⏹ Annuler", k::RUN_CANCEL, "danger"),
                ],
                vec![
                    b("📜 Trace", k::RUN_TRACE, ""),
                    b("🔁 Relancer l'étape", k::RUN_RETRY_STEP, ""),
                ],
            ],
        ),
        t(
            "run_done",
            "✅ **{{nom}} terminé**\n\n{{resultat}}\n\nLivrables :\n{{livrables}}\n\nCoût : {{cout}}",
            &["nom", "resultat", "livrables", "cout"],
            vec![vec![
                b("📜 Trace", k::RUN_TRACE, ""),
                b("🔁 Relancer", k::WORKFLOW_RUN_ONCE, ""),
            ]],
        ),
        t(
            "run_blocked",
            "⛔ **{{nom}} bloqué**\n\nRaison : {{raison}}\n\nDernière sortie :\n```\n{{sortie}}\n```",
            &["nom", "raison", "sortie"],
            vec![vec![
                b("🔁 Réessayer", k::RUN_RETRY_STEP, ""),
                b("⏭ Passer l'étape", k::RUN_SKIP_STEP, ""),
                b("⏹ Annuler", k::RUN_CANCEL, "danger"),
            ]],
        ),
        t(
            "incident",
            "🔥 **Incident**\n\n{{contexte}}\n\nErreur :\n```\n{{erreur}}\n```",
            &["contexte", "erreur"],
            vec![vec![
                b("Rollback", k::CHOICE, "danger"),
                b("Réessayer", k::RUN_RETRY_STEP, ""),
                b("Laisser", k::DENY, ""),
            ]],
        ),
        t(
            "ticket_detected",
            "🎫 **{{titre}}**\n\nProjet : {{projet}}\nPriorité : {{priorite}}\nÉchéance : {{echeance}}\n\n{{lien}}",
            &["titre", "projet", "priorite", "echeance", "lien"],
            vec![vec![
                b("▶️ Lancer le workflow", k::WORKFLOW_RUN_ONCE, "primary"),
                b("🙈 Ignorer", k::DENY, ""),
                b("⏰ Plus tard", k::CHOICE, ""),
            ]],
        ),
        t(
            "question",
            "❓ {{question}}",
            &["question"],
            vec![vec![b("✏️ Répondre", k::FORM_SUBMIT, "primary")]],
        ),
        t(
            "form",
            "**{{titre}}**\n\nChamp : {{champ}} ({{type_champ}})\nDéfaut : {{defaut}}\n\nProgression : {{progression}}",
            &["titre", "champ", "type_champ", "defaut", "progression"],
            vec![
                vec![b("⬅️", k::FORM_PREV, ""), b("➡️", k::FORM_NEXT, "")],
                vec![
                    b("✅ Envoyer", k::FORM_SUBMIT, "success"),
                    b("Décliner", k::FORM_DECLINE, ""),
                    b("Annuler", k::DENY, "danger"),
                ],
            ],
        ),
        t(
            "mcp_oauth_required",
            "🔐 **Autorisation requise**\n\nServeur : {{serveur}}\nScopes : {{scopes}}\nMode : {{mode}}",
            &["serveur", "scopes", "mode", "url"],
            vec![vec![
                burl("🔐 Autoriser", "{{url}}"),
                b("📋 J'ai collé l'URL", k::OAUTH_PASTED, ""),
                b("🔁 Relancer", k::OAUTH_RETRY, ""),
            ]],
        ),
        t(
            "mcp_url_elicitation",
            "🌐 **Le serveur {{serveur}} demande une action**\n\n{{raison}}",
            &["serveur", "raison", "url"],
            vec![vec![
                burl("🌐 Ouvrir", "{{url}}"),
                b("✅ J'ai terminé", k::ELICIT_DONE, "success"),
                b("Annuler", k::DENY, "danger"),
            ]],
        ),
        t(
            "sampling_request",
            "🤖 **{{serveur}} demande une génération**\n\n\
             Prompt : {{prompt}}\nModèle : {{modele}}\nBudget : {{budget}}",
            &["serveur", "prompt", "modele", "budget"],
            vec![vec![
                b("✅ Autoriser", k::APPROVE, "success"),
                b("❌ Refuser", k::DENY, "danger"),
            ]],
        ),
        t(
            "effect_unknown",
            "❔ **Effet incertain**\n\n\
             {{effet}} a peut-être eu lieu à {{horodatage}}.\n\nRequête :\n```json\n{{requete}}\n```",
            &["effet", "horodatage", "requete"],
            vec![vec![
                b("🔍 Vérifier", k::EFFECT_VERIFY, ""),
                b("🔁 Relancer", k::EFFECT_RETRY, ""),
                b("⏭ Ignorer", k::EFFECT_IGNORE, ""),
            ]],
        ),
        t(
            "skill_proposal",
            "🧩 **Skill proposée : {{nom}}**\n\n{{resume}}",
            &["nom", "resume"],
            vec![vec![
                b("✅ Activer", k::SKILL_ACTIVATE, "success"),
                b("✏️ Modifier", k::DENY_REASON, ""),
                b("🗑 Rejeter", k::DENY, "danger"),
            ]],
        ),
        t(
            "memory_proposal",
            "🧠 **Propositions de mémoire**\n\n{{items}}",
            &["items"],
            vec![vec![
                b("✅ Tout", k::MEMORY_ACCEPT, "success"),
                b("🔀 En exception", k::MEMORY_AS_EXCEPTION, ""),
                b("🗑 Rien", k::MEMORY_REJECT, "danger"),
            ]],
        ),
        t(
            "learned",
            "📚 **Appris récemment**\n\n{{apprentissages}}\n\nExceptions créées :\n{{exceptions}}",
            &["apprentissages", "exceptions"],
            vec![vec![
                b("📄 Voir la pratique", k::CHOICE, ""),
                b("↩️ Annuler", k::MEMORY_REJECT, ""),
            ]],
        ),
        t(
            "schedule_preview",
            "⏰ **Nouveau déclencheur**\n\n\
             Type : {{declencheur}}\nOutil : {{outil}}\nExtraction : {{extraction}}\n\n\
             Test à blanc :\n{{apercu}}",
            &["declencheur", "outil", "extraction", "apercu"],
            vec![vec![
                b("✅ Activer", k::SCHEDULE_ENABLE, "success"),
                b("✏️ Modifier", k::DENY_REASON, ""),
                b("❌ Annuler", k::DENY, "danger"),
            ]],
        ),
        t(
            "workflow_preview",
            "🔧 **Workflow proposé : {{nom}}**\n\n```\n{{graphe}}\n```\n\n\
             Risques : {{risques}}\nBudget : {{budget}}",
            &["nom", "graphe", "risques", "budget"],
            vec![vec![
                b("✅ Enregistrer", k::WORKFLOW_SAVE, "success"),
                b("▶️ Lancer une fois", k::WORKFLOW_RUN_ONCE, ""),
                b("❌ Rejeter", k::DENY, "danger"),
            ]],
        ),
        t(
            "budget_alert",
            "💸 **Budget {{perimetre}}**\n\nConsommé : {{consomme}} / {{plafond}}",
            &["perimetre", "consomme", "plafond"],
            vec![vec![
                b("➕ +50 %", k::BUDGET_RAISE, ""),
                b("⏹ Arrêter", k::BUDGET_STOP, "danger"),
            ]],
        ),
        t(
            "mcp_status",
            "🔌 **Serveurs MCP**\n\n{{tableau}}",
            &["tableau"],
            vec![vec![
                b("➕ Ajouter", k::CHOICE, ""),
                b("🧪 Tester", k::CHOICE, ""),
            ]],
        ),
        t(
            "digest",
            "☀️ **Digest du matin**\n\n\
             Runs : {{runs}}\nApprobations en attente : {{approbations}}\nTickets : {{tickets}}\n\
             Coûts : {{couts}}\n\n**Appris cette nuit**\n{{appris}}\n\n{{questions}}",
            &[
                "runs",
                "approbations",
                "tickets",
                "couts",
                "appris",
                "questions",
            ],
            vec![vec![b("🩺 Doctor", k::DOCTOR, "")]],
        ),
        t(
            "heartbeat",
            "💓 Uptime {{uptime}} · RAM {{ram}} · disque {{disque}} · erreurs 24 h : {{erreurs}}",
            &["uptime", "ram", "disque", "erreurs"],
            vec![vec![b("🩺 Doctor", k::DOCTOR, "")]],
        ),
        t(
            "stopped",
            "⏹ **Génération arrêtée**\n\n{{partiel}}",
            &["partiel"],
            vec![vec![
                b("▶️ Reprendre", k::STOP_RESUME, "primary"),
                b("🗑 Oublier", k::STOP_FORGET, ""),
            ]],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_catalog_is_complete() {
        let r = TemplateRegistry::with_builtins();
        for id in CATALOG {
            assert!(r.get(id).is_some(), "template manquant : {id}");
        }
        assert_eq!(r.len(), CATALOG.len());
    }

    /// CA 14 : chaque template se rend en blocs riches **et** en HTML de repli.
    #[test]
    fn ca_14_1_every_template_renders_in_both_forms() {
        let r = TemplateRegistry::with_builtins();
        for id in r.ids() {
            let t = r.get(&id).unwrap();
            t.validate().unwrap_or_else(|e| panic!("{e}"));

            let v: BTreeMap<String, String> = t
                .variables
                .iter()
                .map(|name| (name.clone(), format!("<valeur {name}>")))
                .collect();
            let tokens: BTreeMap<String, String> = t
                .actions()
                .into_iter()
                .map(|a| (a, "a:jeton1234".to_string()))
                .collect();

            let rendered = t.render(&v, &tokens, &[]).unwrap_or_else(|e| panic!("{e}"));
            assert!(!rendered.blocks.is_empty(), "{id} : aucun bloc");
            assert!(!rendered.html.is_empty(), "{id} : HTML vide");
            assert!(
                !rendered.html.contains("{{"),
                "{id} : variable non substituée dans le HTML"
            );
            // Les valeurs injectées sont échappées.
            assert!(
                !rendered.html.contains("<valeur"),
                "{id} : échappement HTML manquant"
            );
        }
    }

    #[test]
    fn missing_variable_is_an_error_not_a_hole() {
        let r = TemplateRegistry::with_builtins();
        let t = r.get("tool_approval").unwrap();
        let e = t.render(&vars(&[("outil", "x")]), &BTreeMap::new(), &[]);
        assert!(e.is_err());
        assert!(e.unwrap_err().message.contains("non fournies"));
    }

    #[test]
    fn undeclared_variable_is_rejected_at_validation() {
        let t = Template {
            id: "x".into(),
            body: "bonjour {{inconnue}}".into(),
            variables: vec![],
            buttons: vec![],
            fallback_html: String::new(),
        };
        assert!(t.validate().unwrap_err().message.contains("inconnue"));
    }

    #[test]
    fn unknown_action_or_style_is_rejected() {
        let mut t = Template {
            id: "x".into(),
            body: "corps".into(),
            variables: vec![],
            buttons: vec![vec![b("Go", "action_inconnue", "")]],
            fallback_html: String::new(),
        };
        assert!(
            t.validate()
                .unwrap_err()
                .message
                .contains("action inconnue")
        );
        t.buttons = vec![vec![b("Go", crate::actions::kind::APPROVE, "violet")]];
        assert!(t.validate().unwrap_err().message.contains("style inconnu"));
    }

    #[test]
    fn disabled_buttons_are_kept_in_place() {
        let r = TemplateRegistry::with_builtins();
        let t = r.get("run_card").unwrap();
        let v: BTreeMap<String, String> = t
            .variables
            .iter()
            .map(|n| (n.clone(), "x".to_string()))
            .collect();
        let tokens: BTreeMap<String, String> = t
            .actions()
            .into_iter()
            .map(|a| (a, "a:t".to_string()))
            .collect();
        let rendered = t
            .render(&v, &tokens, &[crate::actions::kind::RUN_RESUME.to_string()])
            .unwrap();
        let flat: Vec<&ButtonSpec> = rendered.buttons.iter().flatten().collect();
        assert_eq!(flat.len(), 5, "aucun bouton n'est retiré");
        assert_eq!(flat.iter().filter(|b| b.disabled).count(), 1);
    }

    #[test]
    fn url_buttons_substitute_variables() {
        let r = TemplateRegistry::with_builtins();
        let t = r.get("mcp_oauth_required").unwrap();
        let v = vars(&[
            ("serveur", "forge"),
            ("scopes", "repo:write"),
            ("mode", "paste_back"),
            ("url", "https://auth.example/authorize?x=1"),
        ]);
        let tokens: BTreeMap<String, String> = t
            .actions()
            .into_iter()
            .map(|a| (a, "a:t".to_string()))
            .collect();
        let rendered = t.render(&v, &tokens, &[]).unwrap();
        let url_btn = rendered.buttons[0]
            .iter()
            .find(|b| matches!(b.action, crate::render::ButtonAction::Url { .. }))
            .unwrap();
        match &url_btn.action {
            crate::render::ButtonAction::Url { url } => {
                assert_eq!(url, "https://auth.example/authorize?x=1")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hot_reload_overrides_builtins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("answer.toml"),
            "id = \"answer\"\nbody = \"Version maison : {{corps}}\"\nvariables = [\"corps\"]\n",
        )
        .unwrap();
        let mut r = TemplateRegistry::with_builtins();
        assert_eq!(r.load_dir(dir.path()), 1);
        let t = r.get("answer").unwrap();
        assert!(t.body.contains("Version maison"));
        assert!(t.buttons.is_empty());
    }

    #[test]
    fn a_broken_template_is_reported_and_the_others_survive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bon.toml"), "body = \"ok\"\n").unwrap();
        std::fs::write(dir.path().join("casse.toml"), "== pas du toml\n").unwrap();
        std::fs::write(dir.path().join("invalide.toml"), "body = \"{{absente}}\"\n").unwrap();
        let mut r = TemplateRegistry::new();
        assert_eq!(r.load_dir(dir.path()), 1);
        assert_eq!(r.errors().len(), 2);
    }

    #[test]
    fn placeholders_are_extracted_once() {
        assert_eq!(
            placeholders("a {{x}} b {{y}} c {{x}}"),
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn every_button_action_is_known() {
        for t in builtin_templates() {
            for a in t.actions() {
                assert!(
                    crate::actions::kind::ALL.contains(&a.as_str()),
                    "action inconnue dans `{}` : {a}",
                    t.id
                );
            }
        }
    }
}
