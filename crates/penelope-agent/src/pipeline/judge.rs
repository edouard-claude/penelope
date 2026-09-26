//! Étape 5 du pipeline d'un appel : le juge d'approbation (issue #203,
//! `design/v1/boucle-et-outils.md` §3.6, épopée #208, lot K, tâches T22 et T23).
//!
//! Appelé **seulement** quand les quatre conditions de #203 sont réunies : l'outil est
//! `shell_exec`, la politique a rendu `Ask` (jamais `Deny`, jamais `AskTwice`), la ligne
//! n'a aucun motif possible, et sa classe n'est pas `Destructive`. S'y ajoute une
//! cinquième, déterministe : aucune règle du propriétaire qui refuse ou redemande ne
//! nomme une famille présente dans la ligne. Sous les planchers, jamais au-dessus.
//!
//! Ce que la boucle fait du jugement :
//!
//! ```text
//!  échec (modèle absent, délai, hors schéma)  → carte d'aujourd'hui, sans un mot
//!  mode explain                               → carte enrichie ; « Toujours pour ces
//!                                               pouvoirs » si la règle se lit d'un coup
//!  règle de pouvoirs qui couvre le jugement   → sans carte, sauf verdict dangereux
//!  mode auto_read, lecture pure au workspace  → sans carte
//! ```
//!
//! Rien de ce que dit le juge n'autorise sans un contrôle déterministe derrière : chemins
//! normalisés et contenus dans les workspaces, réseau de l'appel, vetos de
//! `penelope_hitl::powers` (`curl … | sh`, `rm`, redirections, interpréteurs…). Un doute
//! garde la carte.

use super::policy::{Verdict, VerdictLayer};
use super::*;
use penelope_app::judge::{JUDGE_TIMEOUT, JudgeRequest, JudgeVerdict, Judgement};
use penelope_hitl::powers::{self, Power, PowerGrant};
use penelope_kernel::config::JudgeMode;
use std::path::{Path, PathBuf};

/// Ce que le juge laisse à l'approbation.
pub(crate) enum JudgeStep {
    /// La carte part ; `Some` : ce que le juge a reconnu, pour l'enrichir.
    Card(Option<Value>),
    /// L'appel part sans carte, pour cette raison.
    Auto(Verdict),
}

/// Les quatre conditions de #203.
fn eligible(info: &CallInfo, verdict: &Verdict, args: &Value) -> bool {
    info.effective_name == "shell_exec"
        && verdict.decision == PolicyDecision::Ask
        && info.risk != RiskClass::Destructive
        && crate::always_creates_no_rule(&info.effective_name, Some(args))
}

/// Répertoire de travail de l'appel : `cwd` (relatif au workspace), sinon le workspace.
fn cwd_of(args: &Value, workspace: Option<&Path>) -> Option<PathBuf> {
    match args.get("cwd").and_then(|v| v.as_str()) {
        Some(c) if Path::new(c).is_absolute() => powers::normalise_path(c, None).map(PathBuf::from),
        Some(c) => powers::normalise_path(c, workspace).map(PathBuf::from),
        None => workspace.map(Path::to_path_buf),
    }
}

/// Ce que le jugement touche, sous une forme comparable. `None` : un chemin ou un hôte
/// ne se relit pas (`~`, variable…), rien ne sera automatisé ni accordé.
struct Reach {
    paths: Vec<String>,
    hosts: Vec<String>,
}

fn reach_of(j: &Judgement, cwd: Option<&Path>) -> Option<Reach> {
    let mut paths = j
        .paths
        .iter()
        .map(|p| powers::normalise_path(p, cwd))
        .collect::<Option<Vec<_>>>()?;
    // Lire ou écrire sans chemin nommé, c'est dans le répertoire de travail.
    if paths.is_empty() && (j.powers.contains(&Power::Read) || j.powers.contains(&Power::Write)) {
        paths.push(cwd?.to_string_lossy().into_owned());
    }
    paths.dedup();
    let hosts = j
        .hosts
        .iter()
        .map(|h| powers::normalise_host(h))
        .collect::<Option<Vec<_>>>()?;
    Some(Reach { paths, hosts })
}

/// La ligne jugée, et ce que l'appel en dit hors du jugement.
struct Line<'a> {
    command: &'a str,
    network: bool,
    workspaces: &'a [PathBuf],
}

/// Un appel qui part sans carte : la couche, le nom de l'issue, la règle de pouvoirs.
struct Automation {
    verdict: Verdict,
    outcome: &'static str,
    rule: Option<String>,
}

/// `auto_read` : un verdict sûr, la lecture seule, sans hôte ni réseau, tout dans les
/// workspaces, et rien que les vetos reconnaissent comme autre chose qu'une lecture.
fn pure_read(j: &Judgement, reach: &Reach, line: &Line<'_>) -> bool {
    let inside = |p: &String| {
        line.workspaces
            .iter()
            .any(|w| powers::within(p, &w.to_string_lossy()))
    };
    j.verdict == JudgeVerdict::Safe
        && j.powers == [Power::Read]
        && reach.hosts.is_empty()
        && !line.network
        && !reach.paths.is_empty()
        && reach.paths.iter().all(inside)
        && powers::not_pure_read(line.command).is_none()
}

/// Règle de pouvoirs proposée au propriétaire, si elle se lit d'un coup d'œil.
fn grant_of(j: &Judgement, reach: &Reach, command: &str, network: bool) -> Option<PowerGrant> {
    let grant = PowerGrant {
        powers: j.powers.clone(),
        paths: reach.paths.clone(),
        hosts: reach.hosts.clone(),
        judged: None,
    };
    (j.verdict != JudgeVerdict::Dangerous
        && powers::never_automatic(command).is_none()
        && (!network || j.powers.contains(&Power::Network))
        && grant.readable())
    .then_some(grant)
}

impl AgentLoop {
    /// L'étape du juge, entre la politique et la carte.
    pub(crate) async fn judge_stage(
        &self,
        spec: &TurnSpec,
        workspace: Option<&Path>,
        info: &CallInfo,
        args: &Value,
        verdict: &Verdict,
    ) -> anyhow::Result<JudgeStep> {
        let s = &self.services;
        let mode = s.config.config().approval.judge;
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if mode == JudgeMode::Off
            || !s.judge.present()
            || command.trim().is_empty()
            || !eligible(info, verdict, args)
        {
            return Ok(JudgeStep::Card(None));
        }
        // Une règle du propriétaire qui nomme une famille de la ligne prime : le juge ne
        // la voit pas.
        if s.policies
            .owner_rule_in_line(command, spec.run_id.as_deref(), Some(&spec.session_id))
            .await?
            .is_some()
        {
            return Ok(JudgeStep::Card(None));
        }
        let cwd = cwd_of(args, workspace);
        let workspaces: Vec<PathBuf> = workspace.map(Path::to_path_buf).into_iter().collect();
        let request = JudgeRequest {
            command,
            cwd: cwd.as_deref(),
            workspaces: &workspaces,
            session_id: &spec.session_id,
            turn_id: spec.turn_id.as_deref(),
        };
        // Le port borne son appel ; la boucle le borne aussi, pour un port qui ne le
        // ferait pas.
        let judged = tokio::time::timeout(
            JUDGE_TIMEOUT + std::time::Duration::from_secs(2),
            s.judge.judge(request),
        )
        .await
        .unwrap_or(Err(penelope_app::judge::JudgeFailure::Timeout));
        let hash: String = penelope_kernel::canonical::sha256_hex(command.as_bytes())
            .chars()
            .take(16)
            .collect();
        let j = match judged {
            Ok(j) => j,
            Err(failure) => {
                self.judged_event(
                    spec,
                    json!({
                        "command_sha": hash,
                        "mode": mode.as_str(),
                        "outcome": "echec",
                        "failure": failure.kind(),
                        "detail": failure.detail(),
                    }),
                )
                .await?;
                return Ok(JudgeStep::Card(None));
            }
        };
        let network = args.get("network") == Some(&Value::Bool(true));
        let reach = reach_of(&j, cwd.as_deref());
        // L'automatisme ne vaut que hors du mode « demander tout » de la session.
        let step = match reach.as_ref() {
            Some(reach) if s.modes.of_session(&spec.session_id).await != ApprovalMode::Ask => {
                let line = Line {
                    command,
                    network,
                    workspaces: &workspaces,
                };
                self.automation(spec, mode, &j, reach, &line).await?
            }
            _ => None,
        };
        let grant = reach
            .as_ref()
            .and_then(|r| grant_of(&j, r, command, network));
        let outcome = step.as_ref().map(|a| a.outcome).unwrap_or("carte");
        let powers_list: Vec<&str> = j.powers.iter().map(|p| p.as_str()).collect();
        self.judged_event(
            spec,
            json!({
                "command_sha": hash,
                "mode": mode.as_str(),
                "outcome": outcome,
                "verdict": j.verdict.as_str(),
                "powers": powers_list,
                "hosts": j.hosts,
                "model": j.model,
                "duration_ms": j.duration_ms,
                "cost_usd": j.cost_usd,
                "rule": step.as_ref().and_then(|a| a.rule.clone()),
            }),
        )
        .await?;
        if let Some(auto) = step {
            return Ok(JudgeStep::Auto(auto.verdict));
        }
        Ok(JudgeStep::Card(Some(json!({
            "verdict": j.verdict.as_str(),
            "powers": powers_list,
            "paths": j.paths,
            "hosts": j.hosts,
            "why": j.why,
            "model": j.model,
            "grant": grant,
        }))))
    }

    /// Ce que le jugement permet sans carte : une lecture pure en `auto_read`, ou une
    /// règle de pouvoirs qui le couvre. Jamais un verdict dangereux, jamais une ligne
    /// qu'un veto déterministe retient.
    async fn automation(
        &self,
        spec: &TurnSpec,
        mode: JudgeMode,
        j: &Judgement,
        reach: &Reach,
        line: &Line<'_>,
    ) -> anyhow::Result<Option<Automation>> {
        let s = &self.services;
        if mode == JudgeMode::AutoRead && pure_read(j, reach, line) {
            return Ok(Some(Automation {
                verdict: Verdict {
                    decision: PolicyDecision::Auto,
                    layer: VerdictLayer::Judge,
                    reason: "lecture seule dans les workspaces (juge, mode auto_read)".into(),
                },
                outcome: "auto_read",
                rule: None,
            }));
        }
        if j.verdict == JudgeVerdict::Dangerous || powers::never_automatic(line.command).is_some() {
            return Ok(None);
        }
        let rules = s
            .policies
            .power_rules(spec.run_id.as_deref(), Some(&spec.session_id))
            .await?;
        let Some((rule, _)) = rules.iter().find(|(_, g)| {
            g.covers(&j.powers, &reach.paths, &reach.hosts)
                && (!line.network || g.powers.contains(&Power::Network))
        }) else {
            return Ok(None);
        };
        s.policies.record_hit(&rule.id).await?;
        Ok(Some(Automation {
            verdict: Verdict {
                decision: PolicyDecision::Auto,
                layer: VerdictLayer::Judge,
                reason: format!("règle de pouvoirs {}", rule.id),
            },
            outcome: "regle_pouvoirs",
            rule: Some(rule.id.clone()),
        }))
    }

    async fn judged_event(&self, spec: &TurnSpec, payload: Value) -> anyhow::Result<()> {
        self.services
            .events
            .append(
                TurnEventKind::ApprovalJudged
                    .draft(payload)
                    .session(&spec.session_id),
            )
            .await?;
        Ok(())
    }
}
