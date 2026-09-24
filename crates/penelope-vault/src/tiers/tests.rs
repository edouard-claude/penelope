use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, Arc::new(s))
}

/// #58 : le projet actif du workspace de la session compte dans le classement : à
/// texte égal, l'entrée du projet passe devant.
#[tokio::test]
async fn an_entry_of_the_open_project_ranks_first() {
    let (_d, s) = services().await;
    let sess = s
        .sessions
        .create_with(
            penelope_kernel::session::SessionKind::Chat,
            None,
            None,
            Some("/Users/edouard/Code/atlas".into()),
        )
        .await
        .unwrap();
    let sid = sess.id.to_string();
    let key = penelope_memory::recall::project_key(None, "/Users/edouard/Code/atlas");

    let vault = vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("projets.md"),
        format!(
            "# Projets\n\n\
             - Les migrations passent par sqlx. <!-- uid: 01PROJ --> \
               <!-- projet: {key} -->\n\
             - Les migrations passent par sqlx ailleurs. <!-- uid: 01AUTRE -->\n"
        ),
    )
    .unwrap();
    crate::vault_ops::reindex(&s, &vault).await.unwrap();

    let ctx = current_context(&s, "comment fait-on les migrations ?", Some(&sid)).await;
    assert_eq!(
        ctx.active_projects,
        vec![key.clone()],
        "projet actif du tour"
    );

    let hits = s
        .memory
        .search(
            "migrations sqlx",
            None,
            &penelope_memory::index::SearchFilter {
                limit: 5,
                automatic: true,
                ..Default::default()
            },
            &ctx.active_projects,
        )
        .await
        .unwrap();
    assert_eq!(
        hits.first().map(|h| h.entry.uid.as_str()),
        Some("01PROJ"),
        "l'entrée du projet ouvert passe devant : {hits:?}"
    );
}
