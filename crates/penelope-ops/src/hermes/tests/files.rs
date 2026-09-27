//! `SOUL.md` et `AGENTS.md` : un secret refuse le fichier, une version différente est
//! mise de côté, une version identique ne change rien.

use super::*;

#[tokio::test]
async fn soul_and_agents_files_are_refused_set_aside_or_identical() {
    let (dir, s) = services().await;
    let root = dir.path().join("hermes-fichiers");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("SOUL.md"), "# Âme\n\nVersion Hermes.\n").unwrap();
    std::fs::write(
        root.join("AGENTS.md"),
        "jeton ghp_0123456789abcdef0123456789abcdef0123\n",
    )
    .unwrap();
    let vault = crate::helpers::vault_dir(&s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("SOUL.md"), "# Âme\n\nVersion Pénélope.\n").unwrap();
    let opts = Options {
        root: root.clone(),
        apply: true,
        test: false,
    };

    let r = import(&s, None, &opts).await.unwrap();
    assert_eq!(r.count("fichier", "refused"), 1, "{}", render(&r));
    assert_eq!(r.count("fichier", "set_aside"), 1, "{}", render(&r));
    let aside = s.platform.dirs.state().join("import-hermes/SOUL.md");
    assert_eq!(
        std::fs::read_to_string(&aside).unwrap(),
        "# Âme\n\nVersion Hermes.\n"
    );
    assert!(
        std::fs::read_to_string(vault.join("SOUL.md"))
            .unwrap()
            .contains("Pénélope"),
        "le vault garde le sien"
    );
    assert!(!vault.join("AGENTS.md").exists());

    std::fs::write(vault.join("SOUL.md"), "# Âme\n\nVersion Hermes.\n\n").unwrap();
    let r = import(&s, None, &opts).await.unwrap();
    assert_eq!(r.count("fichier", "identical"), 1, "{}", render(&r));
}
