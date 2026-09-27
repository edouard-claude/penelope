//! Contrôle du vault (issue #207) : le même critère que le filtre d'écriture, un verdict
//! qui ne dépend que du vault, des erreurs qu'on peut vérifier, des conseils exécutables.

use super::*;

fn secret_issues(report: &serde_json::Value) -> Vec<serde_json::Value> {
    report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["message"].as_str().unwrap_or("").contains("secret"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_vault_check_judges_like_the_write_filter() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    // Identifiant de connexion rangé au magasin parce qu'un serveur MCP en a besoin.
    let login = "proprietaire-vc207@exemple.fr";
    penelope_observe::register_secret(login);
    std::fs::create_dir_all(vault.join("journal")).unwrap();
    std::fs::write(
        vault.join("journal/2026-09-23.md"),
        format!(
            "# Journal\n\
             - écrit à {login} au sujet du devis\n\
             - la page Meta `100000000000009` publie le lundi\n\
             - le jeton se construit : token = base64url(<champs>)\n\
             - la clé du service reste ${{SECRET:service_login}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        vault.join("notes.md"),
        "# Notes\n- mot de passe ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();

    let report = vault_check(s).await;
    let errors: Vec<_> = secret_issues(&report)
        .into_iter()
        .filter(|i| i["severity"] == "error")
        .collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0]["file"], "notes.md");
    let msg = errors[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("jeton github") && msg.contains("« ghp_… »"),
        "{msg}"
    );

    // L'affectation qui décrit un schéma est signalée, sans condamner le vault.
    let journal: Vec<_> = secret_issues(&report)
        .into_iter()
        .filter(|i| i["file"] == "journal/2026-09-23.md")
        .collect();
    assert_eq!(journal.len(), 1, "{journal:#?}");
    assert_eq!(journal[0]["severity"], "warning");
    assert_eq!(journal[0]["line"], 4);
    assert!(
        journal[0]["message"]
            .as_str()
            .unwrap()
            .contains("affectation de secret")
    );

    // Sans la clé, le vault repasse à `ok`.
    std::fs::write(vault.join("notes.md"), "# Notes\n- rien de secret\n").unwrap();
    assert_eq!(vault_check(s).await["ok"], true);
    penelope_observe::redact::forget_secret(login);
}

/// Une valeur du magasin qui a la forme d'un secret, recopiée en clair, reste une erreur.
#[tokio::test]
async fn a_secret_shaped_store_value_copied_in_the_vault_stays_an_error() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let key = "Vc207mT2xK9pLw4ZrB8nYc3HsD6fJa1GuE5oRq0XkN";
    penelope_observe::register_secret(key);
    std::fs::write(vault.join("notes.md"), format!("# Notes\n- clé {key}\n")).unwrap();
    let report = vault_check(s).await;
    penelope_observe::redact::forget_secret(key);
    assert_eq!(report["ok"], false, "{report:#}");
    let errors = secret_issues(&report);
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("secret enregistré"),
        "{errors:#?}"
    );
}

/// Deux contrôles séparés par une lecture rendent le même verdict.
#[tokio::test]
async fn the_verdict_does_not_depend_on_what_the_process_read() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::write(
        vault.join("notes.md"),
        "# Notes\n- la doc cite la valeur vc207ab34ef56gh78 en exemple\n",
    )
    .unwrap();
    let before = vault_check(s).await;
    penelope_observe::redact::learn_secrets("KEY = \"vc207ab34ef56gh78\"");
    let after = vault_check(s).await;
    assert_eq!(before["ok"], after["ok"]);
    assert_eq!(before["issues"], after["issues"]);
    assert_eq!(after["ok"], true, "{after:#}");
}

/// Pas de conseil `reindex` sur un fichier que `reindex` saute ; il reste sur les autres.
#[tokio::test]
async fn no_reindex_advice_on_an_excluded_file() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    for dir in ["accueil", "notes", "attachments"] {
        std::fs::create_dir_all(vault.join(dir)).unwrap();
    }
    for rel in [
        "accueil/2026-09-23.md",
        "notes/s1.md",
        "attachments/original.md",
        "log.md",
        "memoire.md",
    ] {
        std::fs::write(vault.join(rel), "# Page\n- une ligne sans uid\n").unwrap();
    }
    let report = vault_check(s).await;
    let advised: Vec<&str> = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| {
            i["message"].as_str().unwrap_or("").contains("reindex") && i["severity"] == "info"
        })
        .map(|i| i["file"].as_str().unwrap())
        .collect();
    assert_eq!(advised, vec!["memoire.md"]);
}
