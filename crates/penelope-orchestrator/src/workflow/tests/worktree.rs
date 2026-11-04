//! Un worktree git par run de workflow (#397) : deux runs sur le même dépôt travaillent
//! chacun sur sa branche, le worktree propre disparaît à la fin, un sale est gardé puis
//! retiré par `prune`, et `sync_with_base` rend les conflits comme une donnée.

use super::delivery::{DEV, git};
use super::*;
use crate::workflow::worktree as wt;
use penelope_workflow::delivery::SYNC_WITH_BASE;
use std::path::{Path, PathBuf};

/// Un dépôt dans le workspace par défaut, sur `develop`, avec un remote nu `origin`.
fn depot(e: &Env) -> PathBuf {
    let root = e.d.services.platform.dirs.data().join("workspace");
    let remote = root.join("remote.git");
    let work = root.join("depot");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&remote, &["init", "-q", "--bare"]);
    git(&work, &["init", "-q", "-b", DEV]);
    git(&work, &["config", "user.name", "Pénélope"]);
    git(&work, &["config", "user.email", "p@exemple.fr"]);
    std::fs::write(work.join("README.md"), "# Service\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "départ"]);
    git(
        &work,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&work, &["push", "-q", "origin", DEV]);
    work
}

fn worktree_wf(id: &str, steps: Value) -> Value {
    let mut w = wf(id, "etape", steps);
    w["metadata"]["parameters"] = json!([
        {"id": "repo", "label": "Dépôt", "type": "string", "required": true}
    ]);
    w["settings"]["workspace"] = json!("worktree");
    w["settings"]["concurrency"] = json!({"maxConcurrent": 4, "admission": "parallel"});
    w
}

fn shell_true() -> Value {
    json!([{"id": "etape", "type": "shell", "command": "true",
            "transitions": [{"goto": "$done"}]}])
}

async fn started(e: &Env, id: &str, repo: &Path) -> Run {
    start_run(
        &e.d,
        id,
        json!({"repo": repo.to_string_lossy()}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn two_runs_on_one_repository_get_two_worktrees_that_vanish_when_clean() {
    let e = env().await;
    let repo = depot(&e);
    install(&e.d, worktree_wf("deux", shell_true())).await;
    let a = started(&e, "deux", &repo).await;
    let b = started(&e, "deux", &repo).await;
    let s = &e.d.services;
    let (wa, wb) = (
        wt::of(s, &a.id).await.unwrap(),
        wt::of(s, &b.id).await.unwrap(),
    );
    assert_ne!(wa.path, wb.path);
    assert_ne!(wa.branch, wb.branch);
    assert!(wa.path.starts_with(wt::root(s)));
    assert_eq!(a.workdir.as_deref(), Some(&*wa.path.to_string_lossy()));
    assert_eq!(
        git(&wa.path, &["symbolic-ref", "--short", "HEAD"]),
        wa.branch
    );
    assert_eq!(wt::list(s).await.len(), 2);

    for run in [&a, &b] {
        assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    }
    assert!(!wa.path.exists() && !wb.path.exists(), "propres : retirés");
    assert!(wt::list(s).await.is_empty());
    let branches = git(&repo, &["branch", "--list", "penelope/*"]);
    assert!(branches.contains(&wa.branch) && branches.contains(&wb.branch));
    assert_eq!(git(&repo, &["symbolic-ref", "--short", "HEAD"]), DEV);
}

#[tokio::test]
async fn a_dirty_worktree_is_kept_until_a_forced_prune() {
    let e = env().await;
    let repo = depot(&e);
    install(&e.d, worktree_wf("sale", shell_true())).await;
    let run = started(&e, "sale", &repo).await;
    let s = &e.d.services;
    let w = wt::of(s, &run.id).await.unwrap();
    std::fs::write(w.path.join("brouillon.txt"), "pas commité\n").unwrap();
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    assert!(w.path.exists(), "des modifications : gardé");
    let listed = wt::list(s).await;
    assert_eq!(listed[0]["dirty"], true);
    assert_eq!(listed[0]["state"], "done");

    let kept = wt::prune(s, false).await;
    assert!(kept[0]["outcome"].as_str().unwrap().starts_with("gardé"));
    assert!(w.path.exists());
    let gone = wt::prune(s, true).await;
    assert!(gone[0]["outcome"].as_str().unwrap().starts_with("retiré"));
    assert!(!w.path.exists());
    assert!(wt::list(s).await.is_empty());
}

#[tokio::test]
async fn a_live_run_keeps_its_worktree_through_prune_and_cancel_releases_it() {
    let e = env().await;
    let repo = depot(&e);
    install(&e.d, worktree_wf("vivant", shell_true())).await;
    let run = started(&e, "vivant", &repo).await;
    let s = &e.d.services;
    let w = wt::of(s, &run.id).await.unwrap();
    let out = wt::prune(s, true).await;
    assert_eq!(out[0]["outcome"], "gardé : le run est vivant");
    assert!(w.path.exists());
    control(&e.d, &run.id, &penelope_workflow::Control::Cancel)
        .await
        .unwrap();
    assert!(!w.path.exists(), "l'arrêt retire le worktree propre");
}

#[tokio::test]
async fn a_repository_outside_the_workspaces_fails_the_run_in_plain_words() {
    let e = env().await;
    let outside = tempfile::tempdir().unwrap();
    git(outside.path(), &["init", "-q"]);
    install(&e.d, worktree_wf("dehors", shell_true())).await;
    let err = start_run(
        &e.d,
        "dehors",
        json!({"repo": outside.path().to_string_lossy()}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap_err();
    assert!(err.contains("aucun workspace autorisé"), "{err}");
    let runs = e.d.services.runs.list(None, 10).await.unwrap();
    assert_eq!(runs[0].state, RunState::Failed);
}

/// `develop` avance sur le remote pendant que le run touche le même fichier.
async fn sync_bench(e: &Env, conflict: bool) -> (Run, wt::Worktree) {
    let repo = depot(e);
    std::fs::create_dir_all(repo.join(".penelope")).unwrap();
    std::fs::write(
        repo.join(".penelope/delivery.toml"),
        format!("[branches]\ndev = \"{DEV}\"\n"),
    )
    .unwrap();
    // Commitée : le worktree part de `HEAD`, il ne voit que ce qui est suivi.
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "livraison"]);
    install(
        &e.d,
        worktree_wf(
            "synchro",
            json!([{"id": "etape", "type": "delivery", "delivery": SYNC_WITH_BASE,
            "transitions": [
                {"goto": "$done", "condition": {"type": "step_result", "result": "passed"}},
                {"goto": "$blocked"}
            ]}]),
        ),
    )
    .await;
    let run = started(e, "synchro", &repo).await;
    let w = wt::of(&e.d.services, &run.id).await.unwrap();
    // La base avance, dans le dépôt d'origine.
    std::fs::write(repo.join("README.md"), "# Service\n\nDe la base.\n").unwrap();
    git(&repo, &["commit", "-qam", "base"]);
    git(&repo, &["push", "-q", "origin", DEV]);
    // Le run, lui, change le même fichier, ou un autre.
    let file = if conflict { "README.md" } else { "run.txt" };
    std::fs::write(w.path.join(file), "Du run.\n").unwrap();
    git(&w.path, &["add", "."]);
    git(&w.path, &["commit", "-qm", "run"]);
    (run, w)
}

#[tokio::test]
async fn sync_with_base_merges_a_clean_base() {
    let e = env().await;
    let (run, w) = sync_bench(&e, false).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Done);
    let r = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.step_outputs["etape"]["merged"], true, "{r:?}");
    assert!(
        !w.path.exists(),
        "fusion commitée, worktree propre : retiré"
    );
    let readme = git(&w.repo, &["show", &format!("{}:README.md", w.branch)]);
    assert!(readme.contains("De la base"), "{readme}");
}

#[tokio::test]
async fn sync_with_base_hands_the_conflicts_back_as_data() {
    let e = env().await;
    let (run, w) = sync_bench(&e, true).await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let r = e.d.services.runs.get(&run.id).await.unwrap().unwrap();
    let out = &r.step_outputs["etape"];
    assert_eq!(out["result"], "failed", "{out}");
    assert_eq!(out["conflicts"], json!(["README.md"]));
    assert!(
        w.path.exists(),
        "la fusion en cours reste, le worktree aussi"
    );
    assert!(
        std::fs::read_to_string(w.path.join("README.md"))
            .unwrap()
            .contains("<<<<<<<"),
        "les marqueurs sont laissés à l'étape qui résout"
    );
}
