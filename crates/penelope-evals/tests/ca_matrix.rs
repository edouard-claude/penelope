//! Garde-fou du §20.2 : `docs/ca-matrix.md` est à jour, et chaque section du PRD qui
//! porte des CA en a au moins un.

use penelope_evals::ca_matrix;

#[test]
fn the_matrix_is_up_to_date() {
    let root = ca_matrix::repo_root();
    let tests = ca_matrix::collect(&root);
    // Le plancher est la liste figée par le gel (#213) : budget.toml [ca].required.
    let budget = std::fs::read_to_string(root.join("crates/penelope-archtest/budget.toml"))
        .expect("budget.toml lisible");
    let required = budget
        .parse::<toml::Value>()
        .expect("budget.toml valide")
        .get("ca")
        .and_then(|c| c.get("required"))
        .and_then(|r| r.as_array())
        .map_or(0, Vec::len);
    assert!(required > 0, "budget.toml : [ca].required vide ou absent");
    assert!(
        tests.len() >= required,
        "trop peu de tests d'acceptation trouvés : {} (attendus : {required})",
        tests.len()
    );
    let rendered = ca_matrix::render(&tests);
    let path = ca_matrix::output_path(&root);

    if std::env::var("UPDATE_CA_MATRIX").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
        return;
    }

    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        current, rendered,
        "docs/ca-matrix.md est périmé : relancer avec UPDATE_CA_MATRIX=1"
    );
}

#[test]
fn every_section_with_acceptance_criteria_is_covered() {
    let root = ca_matrix::repo_root();
    let tests = ca_matrix::collect(&root);

    // Sections du PRD qui portent un bloc **CA** explicite.
    for section in [
        "2", "3", "4", "5", "6", "7", "8", "9", "10", "12", "13", "14", "15", "17",
    ] {
        assert!(
            tests.iter().any(|t| t.section == section),
            "aucun test d'acceptation pour le §{section}"
        );
    }
}

#[test]
fn acceptance_tests_are_spread_across_the_workspace() {
    let root = ca_matrix::repo_root();
    let tests = ca_matrix::collect(&root);
    let files: std::collections::BTreeSet<&str> = tests.iter().map(|t| t.file.as_str()).collect();
    assert!(
        files.len() >= 20,
        "les CA doivent vivre près du code qu'ils couvrent : {files:?}"
    );
    assert!(tests.iter().all(|t| t.file.starts_with("crates/")));
}
