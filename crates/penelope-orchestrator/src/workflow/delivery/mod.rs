//! Étapes `delivery` (T4 de #185, issue #192) : la PR vers la branche de dev, la CI du
//! projet, puis l'environnement de dev vérifié depuis l'extérieur.
//!
//! Chaque étape relit le dépôt et `.penelope/delivery.toml` à chaque visite : une
//! configuration complétée par le propriétaire après une carte « livraison bloquée » est
//! prise au « Réessayer » suivant. Les effets externes (push, ouverture de la PR) passent
//! par le ledger, planifiés **avant** l'appel : après un redémarrage, la PR est cherchée
//! sur le forgeur avant d'être ouverte, jamais ouverte deux fois.
//!
//! Une étape rend `passed`, `blocked` (information manquante, refus) ou `failed` (CI ou
//! E2E rouge) ; tout ce qui n'est pas `passed` pose la carte, avec la raison en clair.

use super::*;
use penelope_kernel::effects::{EffectKind, UnknownDecision};
use penelope_workflow::delivery::config::{self, CONFIG_PATH, Discovered, FileConfig, Missing};
use penelope_workflow::delivery::verdicts::{CI_UNAVAILABLE_LIMIT, CiVerdict, ci_backoff_ms};
use penelope_workflow::delivery::{CI, E2E, PULL_REQUEST};
use std::path::{Path, PathBuf};

mod e2e;
mod forge;
mod git;

use forge::{Forge, ForgeError};

pub(super) async fn delivery_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let outcome = match ctx.step.delivery.as_str() {
        PULL_REQUEST => pull_request(ctx).await?,
        CI => ci(ctx).await?,
        E2E => e2e_stage(ctx).await?,
        other => done(
            StepResult::Error,
            json!({"error": format!("action de livraison inconnue `{other}`")}),
        ),
    };
    if let StepOutcome::Done { result, output } = &outcome {
        let _ = ctx
            .s()
            .events
            .append(
                EventDraft::new(
                    "workflow.delivery",
                    json!({"run": ctx.run.id, "stage": ctx.step.delivery,
                           "result": result.as_str(), "content": output["content"]}),
                )
                .session(&ctx.run.session_id),
            )
            .await;
    }
    Ok(outcome)
}

// ------------------------------------------------------------------ issues

fn outcome(result: StepResult, content: String, extra: Value) -> StepOutcome {
    let mut output = json!({"result": result.as_str(), "content": content});
    if let (Some(o), Value::Object(extra)) = (output.as_object_mut(), extra) {
        o.extend(extra);
    }
    done(result, output)
}

/// Il manque une information, ou le forgeur refuse : le propriétaire décide.
fn blocked(content: String) -> StepOutcome {
    outcome(StepResult::Blocked, content, Value::Null)
}

fn asked(purpose: &str, dir: &Path, missing: &[Missing]) -> StepOutcome {
    blocked(config::ask(purpose, &dir.display().to_string(), missing))
}

/// Message court dans le sujet du run, une fois par visite d'étape et par `what`.
async fn note(ctx: &StepCtx<'_>, what: &str, text: &str) {
    let s = ctx.s();
    let key = visit_key(&format!("note-{what}"), ctx.run, &ctx.step.id);
    if s.kv_get(&key).await.ok().flatten().is_some() {
        return;
    }
    let _ = s.kv_set(&key, "1").await;
    if let Some(m) = ctx.d.workflows.ports.messenger.get() {
        let origin = origin_of(s, &ctx.run.id).await;
        if let Err(e) = m.send_text(&origin, text).await {
            tracing::debug!(run = %ctx.run.id, error = %e, "note de livraison non envoyée");
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

// ------------------------------------------------------------------ projet

/// Le dépôt livré et ce qu'il dit de lui-même.
struct Project {
    dir: PathBuf,
    cfg: FileConfig,
    found: Discovered,
}

/// Ce que la PR a laissé pour la CI et l'E2E.
fn state_key(run_id: &str) -> String {
    format!("wf.delivery.{run_id}")
}

async fn state_of(s: &Services, run_id: &str) -> Option<Value> {
    let raw = s.kv_get(&state_key(run_id)).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Les sessions neuves des phases du run, la plus récente d'abord.
async fn phase_sessions(s: &Services, run_id: &str) -> Vec<String> {
    let prefix = format!("wf.session.{run_id}.%");
    s.store
        .read(move |c| {
            let mut st = c.prepare("SELECT v FROM kv WHERE k LIKE ?1 ORDER BY ts DESC, k DESC")?;
            let rows = st.query_map([prefix], |r| r.get::<_, String>(0))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .await
        .unwrap_or_default()
}

/// Le dépôt déclaré par le plan : `verification.dir` ou `project.dir` des métadonnées
/// d'une phase (la plus récente d'abord), puis de la session du run ; à défaut, l'espace
/// du run s'il est lui-même un dépôt git.
async fn project_dir(ctx: &StepCtx<'_>) -> Option<PathBuf> {
    let s = ctx.s();
    if let Some(dir) = state_of(s, &ctx.run.id)
        .await
        .and_then(|st| st["dir"].as_str().map(PathBuf::from))
    {
        return Some(dir);
    }
    let mut sessions = phase_sessions(s, &ctx.run.id).await;
    sessions.push(ctx.run.session_id.clone());
    for session in sessions {
        let m = session_metadata(s, &session).await;
        let declared = m["verification"]["dir"]
            .as_str()
            .or_else(|| m["project"]["dir"].as_str())
            .filter(|d| !d.trim().is_empty());
        if let Some(dir) = declared {
            return Some(resolve_dir(ctx, Path::new(dir)));
        }
    }
    let workdir = ctx.workdir();
    workdir.join(".git").exists().then_some(workdir)
}

/// Un chemin relatif se lit comme les outils de fichiers le lisent : dans l'espace du
/// run, puis dans les workspaces configurés, le premier où il existe.
fn resolve_dir(ctx: &StepCtx<'_>, dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        return dir.to_path_buf();
    }
    let mut roots = vec![ctx.workdir()];
    roots.extend(penelope_executor::executor::default_workspaces(ctx.s()));
    roots
        .iter()
        .map(|root| root.join(dir))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| roots[0].join(dir))
}

async fn project(ctx: &StepCtx<'_>, purpose: &str) -> Result<Project, StepOutcome> {
    let Some(dir) = project_dir(ctx).await else {
        return Err(blocked(format!(
            "Pour {purpose}, je ne sais pas quel dépôt livrer : aucune phase n'a déclaré \
             `project.dir` (`session_metadata`, op `set`, clé `project`, entrée \
             `{{\"dir\": \"<dépôt>\"}}`) et l'espace du run n'est pas un dépôt git. Déclare-le, puis \
             « Réessayer »."
        )));
    };
    if !dir.join(".git").exists() {
        return Err(blocked(format!(
            "Pour {purpose} : `{}` n'est pas un dépôt git.",
            dir.display()
        )));
    }
    // Sans fichier, tout ce qui se découvre est découvert et le reste est demandé.
    let cfg = match std::fs::read_to_string(dir.join(CONFIG_PATH)) {
        Ok(raw) => FileConfig::parse(&raw).map_err(|e| {
            blocked(format!(
                "Pour {purpose} : {e}. Corrige-le, puis « Réessayer »."
            ))
        })?,
        Err(_) => FileConfig::default(),
    };
    let found = Discovered {
        remote_url: git::remote_url(&dir, &config::remote_name(&cfg)).await,
        has_github_workflows: dir.join(".github").join("workflows").is_dir(),
        has_gitlab_ci: dir.join(".gitlab-ci.yml").is_file(),
    };
    Ok(Project { dir, cfg, found })
}

/// Le client du forgeur du projet, jeton compris.
fn connect(s: &Services, p: &Project, purpose: &str) -> Result<Forge, StepOutcome> {
    let target = config::resolve_forge(&p.cfg, &p.found).map_err(|m| asked(purpose, &p.dir, &m))?;
    let token = s
        .platform
        .secrets
        .get(&target.token_secret)
        .ok()
        .flatten()
        .filter(|t| !t.trim().is_empty());
    let Some(token) = token else {
        return Err(blocked(format!(
            "Pour {purpose}, il me manque le jeton de l'API {} : le secret `{}` est absent du \
             coffre. `penelope secret set {}` (ou `forge.token_secret` dans `{}/{CONFIG_PATH}` \
             pour un autre nom), puis « Réessayer ».",
            target.kind.as_str(),
            target.token_secret,
            target.token_secret,
            p.dir.display()
        )));
    };
    penelope_observe::register_secret(&token);
    Forge::new(target, token).map_err(|e| blocked(format!("Pour {purpose} : {e}.")))
}

// ------------------------------------------------------------------ PR dev

/// Planifie un effet idempotent : son identifiant à exécuter, ou ce qu'il a déjà rendu.
async fn plan_effect(
    ctx: &StepCtx<'_>,
    kind: EffectKind,
    tool: &str,
    request: Value,
) -> anyhow::Result<Result<penelope_kernel::ids::EffectId, Option<Value>>> {
    let s = ctx.s();
    let spec = EffectSpec::new(kind, tool, request)
        .run(&ctx.run.id)
        .session(&ctx.run.session_id)
        .step(&ctx.step.id)
        .idempotent(true);
    let id = match s.effects.plan(spec).await? {
        Planned::Replayed(v) => return Ok(Err(Some(v))),
        Planned::InFlight(_) => return Ok(Err(None)),
        // Un effet idempotent n'attend pas de décision : le forgeur dira ce qui a été fait.
        Planned::NeedsDecision(id) => {
            s.effects
                .resolve_unknown(&id, UnknownDecision::Retry)
                .await?;
            id
        }
        Planned::Fresh(id) => id,
    };
    s.effects.dispatching(&id).await?;
    Ok(Ok(id))
}

async fn pull_request(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    const PURPOSE: &str = "ouvrir la PR dev";
    let s = ctx.s();
    let p = match project(ctx, PURPOSE).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let forge = match connect(s, &p, PURPOSE) {
        Ok(f) => f,
        Err(o) => return Ok(o),
    };
    let target = forge.target().clone();
    let dir = p.dir.display().to_string();
    let head = match git::current_branch(&p.dir).await {
        Ok(b) if b == target.dev_branch => {
            return Ok(blocked(format!(
                "Pour {PURPOSE} : le travail est sur `{b}`, la branche de dev elle-même, dans \
                 `{dir}`. Une PR relie une branche de travail à `{b}` : crée-la, puis « Réessayer »."
            )));
        }
        Ok(b) if !b.starts_with('-') => b,
        Ok(b) => return Ok(blocked(format!("Pour {PURPOSE} : branche `{b}` refusée."))),
        Err(e) => {
            return Ok(blocked(format!(
                "Pour {PURPOSE} : {e} dans `{dir}`. Place-le sur une branche, puis « Réessayer »."
            )));
        }
    };
    match git::dirty(&p.dir).await {
        Ok(false) => {}
        Ok(true) => {
            return Ok(blocked(format!(
                "Pour {PURPOSE} : des changements ne sont pas commités dans `{dir}` ; ils ne \
                 partiraient pas avec la branche `{head}`. Commite-les, puis « Réessayer »."
            )));
        }
        Err(e) => return Ok(blocked(format!("Pour {PURPOSE} : {e}."))),
    }
    let sha = match git::head_sha(&p.dir).await {
        Ok(sha) => sha,
        Err(e) => return Ok(blocked(format!("Pour {PURPOSE} : {e}."))),
    };

    let push = json!({"dir": dir, "remote": target.remote, "branch": head, "sha": sha});
    match plan_effect(ctx, EffectKind::Git, "delivery.push", push).await? {
        Err(Some(_)) => {}
        Err(None) => return Ok(StepOutcome::Waiting("push en cours".into())),
        Ok(id) => match git::push(&p.dir, &target.remote, &head).await {
            Ok(()) => s.effects.complete(&id, json!({"sha": sha})).await?,
            Err(e) => {
                s.effects.fail(&id, e.clone()).await?;
                return Ok(blocked(format!(
                    "Pour {PURPOSE} : le push de `{head}` vers `{}` a échoué : {e}",
                    target.remote
                )));
            }
        },
    }

    // Une seule PR par run : la clé ne dépend ni du commit ni de la visite.
    let request = json!({"forge": target.kind.as_str(), "api": target.api, "repo": target.repo,
                         "head": head, "base": target.dev_branch});
    let pr = match plan_effect(ctx, EffectKind::Http, "delivery.pull_request", request).await? {
        Err(Some(v)) => v,
        Err(None) => return Ok(StepOutcome::Waiting("PR dev en cours d'ouverture".into())),
        Ok(id) => match open_once(ctx, &forge, &head).await {
            Ok(v) => {
                s.effects.complete(&id, v.clone()).await?;
                v
            }
            Err(e) => {
                s.effects.fail(&id, e.to_string()).await?;
                return Ok(blocked(format!("Pour {PURPOSE} : {e}.")));
            }
        },
    };
    let found = pr["found"].as_bool().unwrap_or(false);
    let url = pr["url"].as_str().unwrap_or_default().to_string();
    let base = &target.dev_branch;
    let state = json!({"dir": dir, "forge": target.kind.as_str(), "repo": target.repo,
                       "head": head, "base": base, "sha": sha, "pr": pr});
    s.kv_set(&state_key(&ctx.run.id), &state.to_string())
        .await?;
    let noun = target.kind.request_noun();
    let verb = if found { "retrouvée" } else { "ouverte" };
    note(
        ctx,
        "pr",
        &format!("🔀 {noun} dev {verb} : {url} (`{head}` → `{base}`)"),
    )
    .await;
    Ok(outcome(
        StepResult::Passed,
        format!(
            "{noun} dev {verb} : {url} (`{head}` → `{base}`, commit `{}`)",
            short(&sha)
        ),
        json!({"pr": pr, "head": head, "base": base, "sha": sha, "forge": target.kind.as_str()}),
    ))
}

/// Cherche la PR de la branche sur le forgeur, puis l'ouvre si elle n'existe pas.
async fn open_once(ctx: &StepCtx<'_>, forge: &Forge, head: &str) -> Result<Value, ForgeError> {
    let base = forge.target().dev_branch.clone();
    if let Some(pr) = forge.find_pr(head, &base).await? {
        let mut v = pr.to_json();
        v["found"] = json!(true);
        return Ok(v);
    }
    let title = ctx
        .wf
        .metadata
        .name
        .split_once(" · ")
        .map_or(ctx.wf.metadata.name.as_str(), |(_, goal)| goal)
        .to_string();
    let brief = brief_of(ctx.s(), &ctx.run.id).await;
    let body = format!(
        "{}\n\nLivré par Pénélope, run `{}` : {}.\n\n<!-- penelope-run: {} -->",
        if brief.is_empty() { &title } else { &brief },
        ctx.run.id,
        ctx.wf.metadata.description,
        ctx.run.id
    );
    let pr = forge.open_pr(head, &base, &title, &body).await?;
    let mut v = pr.to_json();
    v["found"] = json!(false);
    Ok(v)
}

// ------------------------------------------------------------------ CI

async fn ci(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    const PURPOSE: &str = "attendre la CI de la PR dev";
    let s = ctx.s();
    let Some(state) = state_of(s, &ctx.run.id).await else {
        return Ok(blocked(
            "Aucune PR dev enregistrée pour ce run : la CI se lit sur le commit de la PR.".into(),
        ));
    };
    // L'échéance d'abord : le pilote repasse toutes les cinq secondes, le dépôt, le
    // coffre et le forgeur ne sont relus qu'à la lecture suivante de la CI.
    let now = s.clock.now_ms().max(0) as u64;
    let key = visit_key("ci", ctx.run, &ctx.step.id);
    let mut poll: Value = s
        .kv_get(&key)
        .await?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({"since": now, "next": 0, "polls": 0, "failures": 0}));
    let at = |k: &str| poll[k].as_u64().unwrap_or(0);
    if now < at("next") {
        return Ok(StepOutcome::Waiting(format!(
            "CI : prochaine lecture dans {} s",
            (at("next") - now).div_ceil(1000)
        )));
    }
    let p = match project(ctx, PURPOSE).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let forge = match connect(s, &p, PURPOSE) {
        Ok(f) => f,
        Err(o) => return Ok(o),
    };
    let plan = match config::resolve_ci(&p.cfg, &p.found, forge.kind()) {
        Ok(plan) => plan,
        Err(m) => return Ok(asked(PURPOSE, &p.dir, &m)),
    };
    let sha = state["sha"].as_str().unwrap_or_default().to_string();
    let url = state["pr"]["url"].as_str().unwrap_or_default().to_string();
    if plan.provider.is_none() {
        return Ok(outcome(
            StepResult::Passed,
            "Pas de CI pour ce projet (`ci.provider = \"none\"`) : rien à attendre.".into(),
            json!({"ci": {"provider": "none", "sha": sha}}),
        ));
    }
    let (polls, failures, since) = (at("polls") as u32, at("failures") as u32, at("since"));
    let evidence = |verdict: &str, detail: &str| {
        json!({"ci": {"verdict": verdict, "detail": detail, "sha": sha, "pr": url,
                      "polls": polls + 1}})
    };
    let sha7 = short(&sha).to_string();
    match forge.ci(&sha).await {
        Ok(CiVerdict::Green(detail)) => {
            note(
                ctx,
                "green",
                &format!("✅ CI verte sur `{sha7}` : {detail}"),
            )
            .await;
            Ok(outcome(
                StepResult::Passed,
                format!("CI verte sur `{sha7}` : {detail} ({url})"),
                evidence("green", &detail),
            ))
        }
        Ok(CiVerdict::Red(detail)) => Ok(outcome(
            StepResult::Failed,
            format!(
                "CI rouge sur `{sha7}` : {detail}. Voir {url}. Relance-la sur le forgeur puis \
                 « Réessayer », ou « Arrêter »."
            ),
            evidence("red", &detail),
        )),
        Ok(CiVerdict::Pending(detail)) if now.saturating_sub(since) >= plan.timeout_ms => {
            Ok(outcome(
                StepResult::Failed,
                format!(
                    "CI sans verdict sur `{sha7}` après {} min : {detail}. Voir {url}.",
                    plan.timeout_ms / 60_000
                ),
                evidence("timeout", &detail),
            ))
        }
        Ok(CiVerdict::Pending(detail)) => {
            if polls == 0 {
                note(
                    ctx,
                    "pending",
                    &format!("⏳ CI en attente sur `{sha7}` : {detail}"),
                )
                .await;
            }
            poll["polls"] = json!(polls + 1);
            poll["failures"] = json!(0);
            poll["next"] = json!(now + ci_backoff_ms(polls));
            poll["last"] = json!(detail);
            s.kv_set(&key, &poll.to_string()).await?;
            Ok(StepOutcome::Waiting(format!("CI en attente : {detail}")))
        }
        Err(ForgeError::Unavailable(e)) if failures + 1 >= CI_UNAVAILABLE_LIMIT => Ok(outcome(
            StepResult::Blocked,
            format!(
                "CI indisponible : {e} ({} lectures d'affilée). « Réessayer » la relit.",
                failures + 1
            ),
            evidence("unavailable", &e),
        )),
        Err(ForgeError::Unavailable(e)) => {
            poll["failures"] = json!(failures + 1);
            poll["next"] = json!(now + ci_backoff_ms(failures));
            poll["last"] = json!(e);
            s.kv_set(&key, &poll.to_string()).await?;
            Ok(StepOutcome::Waiting(format!("CI injoignable : {e}")))
        }
        Err(e @ ForgeError::Refused(_)) => Ok(outcome(
            StepResult::Blocked,
            format!("Lecture de la CI refusée : {e}."),
            evidence("refused", &e.to_string()),
        )),
    }
}

// ------------------------------------------------------------------ E2E

async fn e2e_stage(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    const PURPOSE: &str = "vérifier l'environnement de dev";
    let s = ctx.s();
    let p = match project(ctx, PURPOSE).await {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let plan = match config::resolve_e2e(&p.cfg) {
        Ok(plan) => plan,
        Err(m) => return Ok(asked(PURPOSE, &p.dir, &m)),
    };
    let proofs = e2e::run(ctx, &plan, &p.dir).await;
    let sha = state_of(s, &ctx.run.id)
        .await
        .and_then(|st| st["sha"].as_str().map(String::from))
        .unwrap_or_default();
    let evidence = json!({
        "url": plan.url,
        "sha": sha,
        "at": s.clock.now_rfc3339(),
        "checks": proofs.iter().map(|p| p.value.clone()).collect::<Vec<_>>(),
    });
    // La preuve reste dans le journal du run (sortie de l'étape) et dans son espace.
    let file = format!("livraison/e2e-{}.json", ctx.run.iterations);
    let path = ctx.workdir().join(&file);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&evidence)?);
    let red: Vec<String> = proofs
        .iter()
        .filter(|p| !p.ok)
        .map(|p| {
            format!(
                "{} : {}",
                p.value["check"].as_str().unwrap_or("?"),
                p.value["detail"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let extra = json!({"e2e": evidence, "evidence": file});
    if red.is_empty() {
        let n = proofs.len();
        note(
            ctx,
            "green",
            &format!("✅ E2E vert sur {} : {n} contrôle(s)", plan.url),
        )
        .await;
        return Ok(outcome(
            StepResult::Passed,
            format!(
                "E2E vert sur {} : {n} contrôle(s). Preuves : `{file}`.",
                plan.url
            ),
            extra,
        ));
    }
    Ok(outcome(
        StepResult::Failed,
        format!(
            "E2E rouge sur {} : {}. Preuves : `{file}`. Corrige l'environnement puis \
             « Réessayer », ou « Arrêter ».",
            plan.url,
            red.join(" ; ")
        ),
        extra,
    ))
}
