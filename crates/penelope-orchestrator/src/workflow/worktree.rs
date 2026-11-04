//! Un worktree git par run de workflow (#397) : deux runs sur le même dépôt ne se marchent
//! plus dessus, chacun livre sa PR depuis sa branche. Un workflow `settings.workspace:
//! "worktree"` reçoit le dépôt en paramètre `repo` ; au démarrage, `git worktree add` ouvre
//! `{data}/worktrees/<run>` sur une branche `penelope/<run>` partie de `HEAD`, et ce
//! dossier devient l'espace du run (ses sous-runs compris, comme tout espace).
//!
//! À la fin du run (terminé, échoué, annulé ou `/stop`), le worktree est retiré s'il est
//! propre ; la branche et ses commits restent, pour la PR. Un worktree gardé (des
//! modifications non commitées) se relit et se nettoie par `penelope wf worktrees
//! list|prune`.
//!
//! `git` est lancé comme exécutable, jamais par un shell, sans invite d'identifiants.

use super::*;
use std::path::{Path, PathBuf};

const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Ce que le run sait de son worktree, rangé dans le kv.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    pub run: String,
    /// Racine du dépôt d'origine.
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
}

fn key(run_id: &str) -> String {
    format!("wf.worktree.{run_id}")
}

/// Dossier des worktrees de runs.
pub fn root(s: &Services) -> PathBuf {
    s.platform.dirs.data().join("worktrees")
}

/// Le nom court d'un run, pour sa branche et son dossier : ses caractères
/// alphanumériques, en minuscules.
fn short(run_id: &str) -> String {
    run_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}

pub(super) async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} : délai dépassé", args[0]))?
        .map_err(|e| format!("git introuvable : {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} : {}",
            args.join(" "),
            penelope_observe::redact(String::from_utf8_lossy(&out.stderr).trim())
        ))
    }
}

/// Le dépôt nommé par le paramètre `repo` : `~/…` développé, un chemin relatif lu dans
/// les workspaces configurés, la racine du dépôt rendue. Elle doit être dans un workspace
/// autorisé : un worktree n'ouvre pas un dépôt que les outils ne pourraient pas lire.
async fn repo_root(s: &Services, repo: &str) -> Result<PathBuf, String> {
    let given = penelope_app::mission::expand(repo.trim());
    let candidate = if given.is_absolute() {
        given
    } else {
        penelope_executor::executor::default_workspaces(s)
            .iter()
            .map(|w| w.join(&given))
            .find(|c| c.exists())
            .ok_or_else(|| format!("le dépôt `{repo}` est introuvable dans les workspaces"))?
    };
    let top = git(&candidate, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|_| format!("`{}` n'est pas un dépôt git", candidate.display()))?;
    penelope_app::mission::dir_of(s, &top)
        .ok_or_else(|| format!("la racine du dépôt `{top}` n'est dans aucun workspace autorisé"))
}

/// Les workspaces d'un sous-agent qui travaille dans `workdir` : lui, et le dépôt d'origine
/// si `workdir` est un worktree de run, car `git commit` y écrit ses objets et ses refs.
pub(super) async fn with_repo(s: &Services, workdir: PathBuf) -> Vec<PathBuf> {
    let mut out = vec![workdir.clone()];
    if workdir.starts_with(root(s))
        && let Ok(common) = git(
            &workdir,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await
        && let Some(repo) = Path::new(&common).parent()
    {
        out.push(repo.to_path_buf());
    }
    out
}

/// Ouvre le worktree du run `run_id` dans le dépôt `repo`, et l'enregistre.
pub(super) async fn open(s: &Services, run_id: &str, repo: &str) -> Result<Worktree, String> {
    let repo = repo_root(s, repo).await?;
    let name = short(run_id);
    let branch = format!("penelope/{name}");
    let parent = root(s);
    std::fs::create_dir_all(&parent).map_err(|e| format!("{} : {e}", parent.display()))?;
    let path = parent.join(&name);
    let p = path.to_string_lossy().to_string();
    git(&repo, &["worktree", "add", "-b", &branch, &p, "HEAD"]).await?;
    let wt = Worktree {
        run: run_id.to_string(),
        repo,
        path,
        branch,
    };
    let raw = serde_json::to_string(&wt).map_err(|e| e.to_string())?;
    s.kv_set(&key(run_id), &raw)
        .await
        .map_err(|e| e.to_string())?;
    Ok(wt)
}

/// Le worktree enregistré d'un run.
pub async fn of(s: &Services, run_id: &str) -> Option<Worktree> {
    let raw = s.kv_get(&key(run_id)).await.ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

/// Retire le worktree d'un run fini s'il est propre ; sinon le garde. Rend ce qui a été
/// fait, en clair. La branche reste toujours.
pub(super) async fn release(s: &Services, wt: &Worktree, force: bool) -> Result<String, String> {
    if !wt.path.exists() {
        let _ = git(&wt.repo, &["worktree", "prune"]).await;
        let _ = s.kv_delete(&key(&wt.run)).await;
        return Ok("déjà retiré".into());
    }
    let dirty = git(&wt.path, &["status", "--porcelain"]).await?;
    if !dirty.trim().is_empty() && !force {
        return Ok(format!(
            "gardé : des modifications n'y sont pas commitées (`{}`)",
            wt.path.display()
        ));
    }
    let p = wt.path.to_string_lossy().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    git(&wt.repo, &args).await?;
    let _ = s.kv_delete(&key(&wt.run)).await;
    Ok(format!("retiré, la branche `{}` reste", wt.branch))
}

/// À la fin d'un run : son worktree retiré s'il est propre, le résultat journalisé.
pub(super) async fn on_finish(s: &Services, run: &Run) {
    let Some(wt) = of(s, &run.id).await else {
        return;
    };
    let outcome = release(s, &wt, false).await;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.worktree",
                json!({"run": run.id, "path": wt.path, "branch": wt.branch,
                       "outcome": match &outcome { Ok(o) => o.clone(), Err(e) => e.clone() }}),
            )
            .session(&run.session_id),
        )
        .await;
}

/// Tous les worktrees enregistrés, avec l'état de leur run et leur propreté.
pub async fn list(s: &Services) -> Vec<Value> {
    let rows: Vec<String> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT v FROM kv WHERE k LIKE 'wf.worktree.%' ORDER BY k")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    for wt in rows
        .iter()
        .filter_map(|r| serde_json::from_str::<Worktree>(r).ok())
    {
        let state = s.runs.get(&wt.run).await.ok().flatten().map(|r| r.state);
        let dirty = match wt.path.exists() {
            true => git(&wt.path, &["status", "--porcelain"])
                .await
                .map(|o| !o.trim().is_empty())
                .ok(),
            false => None,
        };
        out.push(json!({
            "run": wt.run,
            "state": state.map(|st| st.as_str()),
            "repo": wt.repo,
            "path": wt.path,
            "branch": wt.branch,
            "exists": wt.path.exists(),
            "dirty": dirty,
        }));
    }
    out
}

/// Retire les worktrees des runs finis (ou oubliés). `force` retire aussi ceux qui ont
/// des modifications non commitées ; un run vivant garde toujours le sien.
pub async fn prune(s: &Services, force: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for item in list(s).await {
        let Ok(wt) = serde_json::from_value::<Worktree>(item.clone()) else {
            continue;
        };
        let alive = s
            .runs
            .get(&wt.run)
            .await
            .ok()
            .flatten()
            .is_some_and(|r| !r.state.is_terminal());
        let outcome = if alive {
            "gardé : le run est vivant".to_string()
        } else {
            release(s, &wt, force).await.unwrap_or_else(|e| e)
        };
        out.push(json!({"run": wt.run, "path": wt.path, "outcome": outcome}));
    }
    out
}

/// `wf.worktrees` : `list` (défaut) ou `prune` (`force` pour les worktrees sales).
pub async fn rpc(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let force = p["force"].as_bool().unwrap_or(false);
    match p["op"].as_str().unwrap_or("list") {
        "list" => Ok(json!(list(s).await)),
        "prune" => Ok(json!(prune(s, force).await)),
        other => anyhow::bail!("opération inconnue `{other}` : `list` ou `prune`"),
    }
}
