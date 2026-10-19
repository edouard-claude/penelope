use super::*;
use crate::bus::Origin;
use crate::mission::{Request, start, stop};
use std::sync::Arc;

fn sh(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// #397 : deux missions sur le même dépôt ont chacune leur worktree et leur branche ; le
/// dépôt d'origine ne bouge pas ; un worktree propre est retiré à la fin, la branche reste.
#[tokio::test]
async fn two_missions_on_one_repo_get_their_own_worktree() {
    if penelope_platform::which("git").is_none() {
        eprintln!("git absent : test ignoré");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(penelope_kernel::clock::SystemClock);
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let ws = crate::helpers::default_workspaces(&s).remove(0);
    let repo = ws.join("depot");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("lib.rs"), "fn main() {}\n").unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-q", "-m", "départ"]);

    let mut cwds = Vec::new();
    for sid in ["s_01AAAAAAAAAAAA1111", "s_01BBBBBBBBBBBB2222"] {
        let (m, text) = start(
            &s,
            sid,
            Request {
                goal: "ajouter un test".into(),
                cwd: Some(repo.to_string_lossy().into()),
                worktree: true,
                ..Default::default()
            },
            &Origin::Cli,
        )
        .await
        .unwrap();
        let branch = m.branch.clone().unwrap();
        assert!(branch.starts_with("penelope/mission-"), "{branch}");
        assert!(text.contains(&branch), "{text}");
        let cwd = std::path::PathBuf::from(m.cwd.clone().unwrap());
        assert!(cwd.join("lib.rs").exists(), "le worktree porte le dépôt");
        assert!(
            cwd.starts_with(std::fs::canonicalize(&repo).unwrap()),
            "dans le workspace"
        );
        cwds.push(cwd);
    }
    assert_ne!(cwds[0], cwds[1]);
    let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains("/.penelope/worktrees/"), "{exclude}");

    // Une modification non commitée garde le worktree ; un worktree propre est retiré.
    std::fs::write(cwds[1].join("brouillon.txt"), "x").unwrap();
    stop(&s, "s_01AAAAAAAAAAAA1111").await.unwrap();
    stop(&s, "s_01BBBBBBBBBBBB2222").await.unwrap();
    assert!(!cwds[0].exists(), "propre : retiré");
    assert!(cwds[1].exists(), "modifié : gardé");
    let branches = std::process::Command::new("git")
        .args(["branch", "--list", "penelope/*"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&branches.stdout).lines().count(),
        2,
        "les branches restent"
    );

    // Sans dépôt, l'option est refusée.
    let err = start(
        &s,
        "s_01CCCCCCCCCCCC3333",
        Request {
            goal: "x".into(),
            worktree: true,
            ..Default::default()
        },
        &Origin::Cli,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("--cwd"), "{err}");
}
