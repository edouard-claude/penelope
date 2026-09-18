//! Politiques d'autorisation (§8.10) et fenêtres (§9.2).
//!
//! Ordre d'évaluation, du plus spécifique au plus général :
//! 1. règle sur `(outil, motif d'arguments)` ;
//! 2. règle sur l'outil ;
//! 3. règle sur le serveur ;
//! 4. surcharge de configuration par classe de risque ;
//! 5. politique par défaut de la classe.
//!
//! Les annotations d'un serveur MCP sont des **indices** : la surcharge de configuration
//! prévaut toujours.

use penelope_kernel::clock::SharedClock;
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    Global,
    Server,
    Tool,
}

impl RuleScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleScope::Global => "global",
            RuleScope::Server => "server",
            RuleScope::Tool => "tool",
        }
    }
    pub fn parse(s: &str) -> Option<RuleScope> {
        Some(match s {
            "global" => RuleScope::Global,
            "server" => RuleScope::Server,
            "tool" => RuleScope::Tool,
            _ => return None,
        })
    }
    /// Plus la valeur est élevée, plus la règle est spécifique.
    pub fn specificity(&self) -> u8 {
        match self {
            RuleScope::Global => 0,
            RuleScope::Server => 1,
            RuleScope::Tool => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: String,
    pub scope: RuleScope,
    pub tool: Option<String>,
    pub server: Option<String>,
    /// Motif d'arguments : chaque paire doit correspondre pour que la règle s'applique.
    /// Exemple : `{"project": "penelope"}`.
    pub arg_match: Option<Value>,
    pub decision: PolicyDecision,
    pub window: PolicyWindow,
    /// Identifiant de run ou de session pour les fenêtres bornées.
    pub window_ref: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub hits: i64,
}

impl PolicyRule {
    /// Vrai si la règle s'applique à cet appel.
    pub fn matches(&self, tool: &str, server: Option<&str>, args: &Value) -> bool {
        if let Some(t) = &self.tool
            && t != tool
        {
            return false;
        }
        if let Some(s) = &self.server
            && Some(s.as_str()) != server
        {
            return false;
        }
        if let Some(pattern) = &self.arg_match
            && !args_match(pattern, args)
        {
            return false;
        }
        // Le réseau d'une commande ne s'accorde jamais implicitement (issue #106) : une
        // règle sur `shell_exec` qui ne le nomme pas (antérieure, ou sur l'outil entier)
        // ne couvre pas un appel qui le demande.
        if self.tool.as_deref() == Some("shell_exec")
            && args.get("network") == Some(&Value::Bool(true))
            && self.arg_match.as_ref().and_then(|p| p.get("network")) != Some(&Value::Bool(true))
        {
            return false;
        }
        true
    }

    /// Vrai si la fenêtre est encore ouverte dans ce contexte.
    pub fn in_window(&self, run_id: Option<&str>, session_id: Option<&str>) -> bool {
        match self.window {
            PolicyWindow::Always => true,
            PolicyWindow::Run => self.window_ref.as_deref() == run_id && run_id.is_some(),
            PolicyWindow::Session => {
                self.window_ref.as_deref() == session_id && session_id.is_some()
            }
            PolicyWindow::Once => false,
        }
    }
}

/// Correspondance de motif : toutes les clés du motif doivent être présentes et égales.
///
/// Trois opérateurs bornent un « toujours » au contexte de l'appel (issue #67), chacun
/// écrit pour ne pas se contourner :
///
/// - [`CMD_PREFIX_OP`] : famille de commandes, **sans** enchaînement (`;`, `&&`, `|`,
///   `$(…)`, redirection, retour à la ligne), et à la frontière d'un mot ;
/// - [`PATH_PREFIX_OP`] : répertoire, comparé sur le chemin normalisé (`..` résolu) ;
/// - [`ORIGIN_OP`] : schéma et hôte **exacts** d'une URL, jamais un préfixe de texte.
fn args_match(pattern: &Value, args: &Value) -> bool {
    if let Value::Object(p) = pattern
        && p.len() == 1
        && let Some((op, Value::String(expected))) = p.iter().next()
    {
        let candidate = args.as_str().unwrap_or_default();
        match op.as_str() {
            CMD_PREFIX_OP => return command_matches(expected, candidate),
            PATH_PREFIX_OP => return path_matches(expected, candidate),
            ORIGIN_OP => return origin_of(candidate).as_deref() == Some(expected.as_str()),
            _ => {}
        }
    }
    match (pattern, args) {
        (Value::Object(p), Value::Object(a)) => p.iter().all(|(k, v)| match a.get(k) {
            Some(av) => args_match(v, av),
            None => false,
        }),
        (p, a) => p == a,
    }
}

/// Famille de commandes (`cargo test`).
pub const CMD_PREFIX_OP: &str = "$cmd_prefix";
/// Répertoire (`src/`), comparé sur le chemin normalisé.
pub const PATH_PREFIX_OP: &str = "$path_prefix";
/// Origine d'une URL (`https://example.com`).
pub const ORIGIN_OP: &str = "$origin";

/// Caractères qui enchaînent ou détournent une commande : une règle « toujours » sur
/// `cargo test` ne doit pas couvrir `cargo test; rm -rf ~`.
pub const CHAINING: &[char] = &[';', '&', '|', '`', '$', '>', '<', '\n', '\r', '(', ')'];

/// Vrai si `candidate` est une commande de la famille `prefix`, sans enchaînement ni
/// mot plus long (`cargo testament` n'est pas `cargo test`).
pub fn command_matches(prefix: &str, candidate: &str) -> bool {
    if candidate.contains(CHAINING) {
        return false;
    }
    let Some(rest) = candidate.strip_prefix(prefix) else {
        return false;
    };
    // La suite commence à une frontière de mot : `cargo testament` ne passe pas pour
    // `cargo test`.
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

fn path_matches(prefix: &str, candidate: &str) -> bool {
    // `src/../../etc/passwd` commence textuellement par `src/` : la comparaison se fait
    // sur le chemin normalisé, jamais sur le texte brut.
    let norm = normalise_text(candidate);
    let want = normalise_text(prefix);
    if want.is_empty() {
        return !norm.contains('/');
    }
    norm.starts_with(&want)
}

/// Résout `.` et `..` textuellement, sans toucher au disque.
fn normalise_text(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    let trailing = if p.ends_with('/') && !joined.is_empty() {
        "/"
    } else {
        ""
    };
    format!("{}{joined}{trailing}", if absolute { "/" } else { "" })
}

/// Schéma et hôte d'une URL : `https://example.com/a` donne `https://example.com`.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (!host.is_empty()).then(|| format!("{}://{}", scheme.to_lowercase(), host.to_lowercase()))
}

/// Motif rendu lisible pour une carte ou `/policies`.
pub fn describe_pattern(pattern: &Value) -> String {
    let Some(obj) = pattern.as_object() else {
        return pattern.to_string();
    };
    obj.iter()
        .map(|(k, v)| match v.as_object().and_then(|o| o.iter().next()) {
            Some((op, val)) if op == CMD_PREFIX_OP => {
                format!("{k} : famille « {} »", val.as_str().unwrap_or_default())
            }
            Some((op, val)) if op == PATH_PREFIX_OP => {
                format!("{k} sous « {} »", val.as_str().unwrap_or_default())
            }
            Some((op, val)) if op == ORIGIN_OP => {
                format!("{k} sur « {} »", val.as_str().unwrap_or_default())
            }
            _ if k == "network" && v == &Value::Bool(true) => "avec réseau".to_string(),
            _ => format!(
                "{k} = {}",
                v.as_str().map(String::from).unwrap_or(v.to_string())
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Décision motivée, pour l'audit et l'affichage.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub decision: PolicyDecision,
    pub risk: RiskClass,
    pub reason: String,
    pub rule_id: Option<String>,
}

#[derive(Clone)]
pub struct PolicyEngine {
    store: Store,
    clock: SharedClock,
}

impl PolicyEngine {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        PolicyEngine { store, clock }
    }

    /// Politique par défaut d'une classe de risque, d'après la configuration.
    pub fn default_for(
        cfg: &penelope_kernel::config::McpPolicy,
        risk: RiskClass,
    ) -> PolicyDecision {
        let s = match risk {
            RiskClass::Read => &cfg.read,
            RiskClass::Write => &cfg.write,
            RiskClass::Destructive => &cfg.destructive,
            RiskClass::External => &cfg.external,
            RiskClass::Unknown => &cfg.unknown,
        };
        PolicyDecision::parse(s).unwrap_or(PolicyDecision::Ask)
    }

    /// Évalue un appel.
    // Chaque paramètre est une colonne du ledger : les regrouper dans une structure
    // ne ferait que déplacer la liste.
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate(
        &self,
        cfg: &penelope_kernel::config::McpPolicy,
        tool: &str,
        server: Option<&str>,
        args: &Value,
        risk: RiskClass,
        run_id: Option<&str>,
        session_id: Option<&str>,
    ) -> penelope_store::Result<Verdict> {
        let rules = self.active_rules().await?;
        let mut best: Option<&PolicyRule> = None;
        for r in &rules {
            if !r.matches(tool, server, args) {
                continue;
            }
            if !r.in_window(run_id, session_id) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => {
                    (r.scope.specificity(), r.arg_match.is_some())
                        > (b.scope.specificity(), b.arg_match.is_some())
                }
            };
            if better {
                best = Some(r);
            }
        }

        if let Some(r) = best {
            let id = r.id.clone();
            self.bump(&id).await?;
            return Ok(Verdict {
                decision: r.decision,
                risk,
                reason: format!(
                    "règle {} ({}, fenêtre {})",
                    r.id,
                    r.scope.as_str(),
                    r.window.as_str()
                ),
                rule_id: Some(id),
            });
        }

        Ok(Verdict {
            decision: Self::default_for(cfg, risk),
            risk,
            reason: format!("politique par défaut pour la classe `{}`", risk.as_str()),
            rule_id: None,
        })
    }

    /// Crée une règle à partir d'une décision « toujours », « pour ce run » ou
    /// « pour cette session ».
    // Chaque paramètre est une colonne du ledger : les regrouper dans une structure
    // ne ferait que déplacer la liste.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_rule(
        &self,
        scope: RuleScope,
        tool: Option<&str>,
        server: Option<&str>,
        arg_match: Option<Value>,
        decision: PolicyDecision,
        window: PolicyWindow,
        window_ref: Option<&str>,
    ) -> penelope_store::Result<PolicyRule> {
        let rule = PolicyRule {
            id: format!("p_{}", penelope_kernel::ids::Ulid::new()),
            scope,
            tool: tool.map(String::from),
            server: server.map(String::from),
            arg_match,
            decision,
            window,
            window_ref: window_ref.map(String::from),
            created_at: self.clock.now_rfc3339(),
            revoked_at: None,
            hits: 0,
        };
        let row = rule.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO policies(id, scope, tool, server, arg_match, decision, window,
                        window_ref, created_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        row.id,
                        row.scope.as_str(),
                        row.tool,
                        row.server,
                        row.arg_match.as_ref().map(|v| v.to_string()),
                        row.decision.as_str(),
                        row.window.as_str(),
                        row.window_ref,
                        row.created_at
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(rule)
    }

    /// Révoque une règle (`/policies`).
    pub async fn revoke(&self, id: &str) -> penelope_store::Result<bool> {
        let (id, now) = (id.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE policies SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
                    params![id, now],
                )? > 0)
            })
            .await
    }

    /// Révoque toutes les règles bornées à un run ou une session terminés.
    pub async fn revoke_window(
        &self,
        window: PolicyWindow,
        reference: &str,
    ) -> penelope_store::Result<usize> {
        let (w, r, now) = (
            window.as_str().to_string(),
            reference.to_string(),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE policies SET revoked_at=?3
                     WHERE window=?1 AND window_ref=?2 AND revoked_at IS NULL",
                    params![w, r, now],
                )?)
            })
            .await
    }

    pub async fn active_rules(&self) -> penelope_store::Result<Vec<PolicyRule>> {
        self.store
            .read(|c| {
                let mut st = c.prepare(
                    "SELECT id, scope, tool, server, arg_match, decision, window, window_ref,
                            created_at, revoked_at, hits
                     FROM policies WHERE revoked_at IS NULL ORDER BY created_at",
                )?;
                let rows = st.query_map([], row_to_rule)?;
                let mut v = Vec::new();
                for r in rows {
                    v.push(r?);
                }
                Ok(v)
            })
            .await
    }

    async fn bump(&self, id: &str) -> penelope_store::Result<()> {
        let id = id.to_string();
        self.store
            .write(move |tx| {
                tx.execute("UPDATE policies SET hits = hits + 1 WHERE id = ?1", [id])?;
                Ok(())
            })
            .await
    }
}

fn row_to_rule(
    r: &penelope_store::rusqlite::Row<'_>,
) -> penelope_store::rusqlite::Result<PolicyRule> {
    let scope: String = r.get(1)?;
    let arg_match: Option<String> = r.get(4)?;
    let decision: String = r.get(5)?;
    let window: String = r.get(6)?;
    Ok(PolicyRule {
        id: r.get(0)?,
        scope: RuleScope::parse(&scope).unwrap_or(RuleScope::Global),
        tool: r.get(2)?,
        server: r.get(3)?,
        arg_match: arg_match.and_then(|s| serde_json::from_str(&s).ok()),
        decision: PolicyDecision::parse(&decision).unwrap_or(PolicyDecision::Ask),
        window: PolicyWindow::parse(&window).unwrap_or(PolicyWindow::Always),
        window_ref: r.get(7)?,
        created_at: r.get(8)?,
        revoked_at: r.get(9)?,
        hits: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {

    /// #67 (revue de sécurité) : les trois opérateurs de motif ne se contournent pas.
    #[test]
    fn pattern_operators_cannot_be_tricked() {
        // Commandes : enchaînement, substitution, redirection, mot plus long.
        assert!(command_matches(
            "cargo test",
            "cargo test -p penelope-kernel"
        ));
        assert!(command_matches("cargo test", "cargo test"));
        for detour in [
            "cargo test; rm -rf ~",
            "cargo test && curl https://exfil.example",
            "cargo test | tee /tmp/x",
            "cargo test `cat ~/.ssh/id_ed25519`",
            "cargo test $(whoami)",
            "cargo test > /tmp/vol",
            "cargo test\nrm -rf ~",
            "cargo testament",
            "cargotest",
        ] {
            assert!(!command_matches("cargo test", detour), "{detour}");
        }

        // Chemins : `..` résolu avant comparaison.
        assert!(path_matches("src/", "src/b.rs"));
        assert!(path_matches("src/", "./src/sous/c.rs"));
        assert!(!path_matches("src/", "src/../../.zshrc"));
        assert!(!path_matches("src/", "autre/b.rs"));
        assert!(path_matches("/etc/app/", "/etc/app/conf"));
        assert!(!path_matches("/etc/app/", "/etc/app/../shadow"));

        // Origine : hôte exact, casse ignorée.
        assert!(origin_of("https://example.com/a?b=1").as_deref() == Some("https://example.com"));
        assert!(origin_of("HTTPS://Example.COM/a").as_deref() == Some("https://example.com"));
        assert!(origin_of("pas-une-url").is_none());
    }
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::config::McpPolicy;
    use serde_json::json;
    use std::sync::Arc;

    fn engine() -> PolicyEngine {
        PolicyEngine::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        )
    }

    #[tokio::test]
    async fn defaults_follow_the_prd_table() {
        let cfg = McpPolicy::default();
        assert_eq!(
            PolicyEngine::default_for(&cfg, RiskClass::Read),
            PolicyDecision::Auto
        );
        assert_eq!(
            PolicyEngine::default_for(&cfg, RiskClass::Write),
            PolicyDecision::Ask
        );
        assert_eq!(
            PolicyEngine::default_for(&cfg, RiskClass::Destructive),
            PolicyDecision::AskTwice
        );
        assert_eq!(
            PolicyEngine::default_for(&cfg, RiskClass::External),
            PolicyDecision::Ask
        );
        assert_eq!(
            PolicyEngine::default_for(&cfg, RiskClass::Unknown),
            PolicyDecision::Ask
        );
    }

    /// CA 9 : une règle « toujours » est appliquée au prochain appel identique, puis
    /// révoquée par `/policies`.
    #[tokio::test]
    async fn ca_9_3_always_rule_applies_then_is_revocable() {
        let e = engine();
        let cfg = McpPolicy::default();
        let args = json!({"project":"penelope","title":"fix"});

        let v = e
            .evaluate(
                &cfg,
                "mcp__forge__create_issue",
                Some("forge"),
                &args,
                RiskClass::Write,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(v.decision, PolicyDecision::Ask);

        let rule = e
            .create_rule(
                RuleScope::Tool,
                Some("mcp__forge__create_issue"),
                Some("forge"),
                Some(json!({"project":"penelope"})),
                PolicyDecision::Auto,
                PolicyWindow::Always,
                None,
            )
            .await
            .unwrap();

        let v = e
            .evaluate(
                &cfg,
                "mcp__forge__create_issue",
                Some("forge"),
                &args,
                RiskClass::Write,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(v.decision, PolicyDecision::Auto);
        assert_eq!(v.rule_id.as_deref(), Some(rule.id.as_str()));

        // Un autre projet ne bénéficie pas de la règle.
        let v = e
            .evaluate(
                &cfg,
                "mcp__forge__create_issue",
                Some("forge"),
                &json!({"project":"autre"}),
                RiskClass::Write,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(v.decision, PolicyDecision::Ask);

        assert!(e.revoke(&rule.id).await.unwrap());
        let v = e
            .evaluate(
                &cfg,
                "mcp__forge__create_issue",
                Some("forge"),
                &args,
                RiskClass::Write,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            v.decision,
            PolicyDecision::Ask,
            "la règle révoquée ne s'applique plus"
        );
    }

    #[tokio::test]
    async fn more_specific_rule_wins() {
        let e = engine();
        let cfg = McpPolicy::default();
        e.create_rule(
            RuleScope::Server,
            None,
            Some("forge"),
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
        e.create_rule(
            RuleScope::Tool,
            Some("mcp__forge__delete_repo"),
            Some("forge"),
            None,
            PolicyDecision::Deny,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();

        let v = e
            .evaluate(
                &cfg,
                "mcp__forge__delete_repo",
                Some("forge"),
                &json!({}),
                RiskClass::Destructive,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            v.decision,
            PolicyDecision::Deny,
            "la règle d'outil prime sur celle de serveur"
        );
    }

    #[tokio::test]
    async fn run_window_is_scoped_to_that_run() {
        let e = engine();
        let cfg = McpPolicy::default();
        e.create_rule(
            RuleScope::Tool,
            Some("shell_exec"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Run,
            Some("r1"),
        )
        .await
        .unwrap();

        let v = e
            .evaluate(
                &cfg,
                "shell_exec",
                None,
                &json!({}),
                RiskClass::Write,
                Some("r1"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(v.decision, PolicyDecision::Auto);

        let v = e
            .evaluate(
                &cfg,
                "shell_exec",
                None,
                &json!({}),
                RiskClass::Write,
                Some("r2"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            v.decision,
            PolicyDecision::Ask,
            "un autre run n'hérite de rien"
        );
    }

    #[tokio::test]
    async fn once_window_never_matches_later() {
        let e = engine();
        let cfg = McpPolicy::default();
        e.create_rule(
            RuleScope::Tool,
            Some("t"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Once,
            None,
        )
        .await
        .unwrap();
        let v = e
            .evaluate(&cfg, "t", None, &json!({}), RiskClass::Write, None, None)
            .await
            .unwrap();
        assert_eq!(v.decision, PolicyDecision::Ask);
    }

    #[tokio::test]
    async fn window_revocation_on_run_end() {
        let e = engine();
        e.create_rule(
            RuleScope::Tool,
            Some("t"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Run,
            Some("r1"),
        )
        .await
        .unwrap();
        assert_eq!(e.active_rules().await.unwrap().len(), 1);
        assert_eq!(e.revoke_window(PolicyWindow::Run, "r1").await.unwrap(), 1);
        assert!(e.active_rules().await.unwrap().is_empty());
    }

    #[test]
    fn nested_argument_patterns() {
        let r = PolicyRule {
            id: "x".into(),
            scope: RuleScope::Tool,
            tool: Some("t".into()),
            server: None,
            arg_match: Some(json!({"repo":{"owner":"moi"}})),
            decision: PolicyDecision::Auto,
            window: PolicyWindow::Always,
            window_ref: None,
            created_at: String::new(),
            revoked_at: None,
            hits: 0,
        };
        assert!(r.matches("t", None, &json!({"repo":{"owner":"moi","name":"x"}})));
        assert!(!r.matches("t", None, &json!({"repo":{"owner":"autre"}})));
        assert!(!r.matches("t", None, &json!({})));
    }

    #[test]
    fn annotations_never_override_configuration() {
        // La classe de risque vient du harnais ; la règle ne dépend pas des annotations
        // du serveur. Ici, même pour un outil annoncé `readOnly`, une règle `deny`
        // s'applique.
        let r = PolicyRule {
            id: "x".into(),
            scope: RuleScope::Tool,
            tool: Some("mcp__x__read".into()),
            server: None,
            arg_match: None,
            decision: PolicyDecision::Deny,
            window: PolicyWindow::Always,
            window_ref: None,
            created_at: String::new(),
            revoked_at: None,
            hits: 0,
        };
        assert!(r.matches("mcp__x__read", None, &json!({})));
        assert_eq!(r.decision, PolicyDecision::Deny);
    }
}
