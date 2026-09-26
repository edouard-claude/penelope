//! #146 : les sondes Python et Node, jusqu'à la commande qui pose ce qui manque.

use super::*;

#[tokio::test]
async fn python_and_node_packages_are_probed() {
    let missing = missing_for(&[
        "pip:penelope_module_absent_xyz".into(),
        "npm:penelope-paquet-absent-xyz".into(),
    ])
    .await;
    assert_eq!(missing.len(), 2, "{missing:?}");
    assert_eq!(missing[0].requirement, "npm:penelope-paquet-absent-xyz");
    assert!(
        missing[0]
            .how
            .starts_with("npm install -g penelope-paquet-absent-xyz"),
        "{:?}",
        missing[0]
    );
    assert!(
        missing[1]
            .how
            .starts_with("pip install penelope_module_absent_xyz"),
        "{:?}",
        missing[1]
    );

    // Un module de la bibliothèque standard est présent dès que `python3` l'est.
    if in_path("python3") {
        assert!(missing_for(&["pip:json".into()]).await.is_empty());
    }
    assert!(!in_path("penelope-binaire-absent-xyz"));
}
