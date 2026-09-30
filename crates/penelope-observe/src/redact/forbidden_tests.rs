//! Un seul critère pour ce qui est **gardé** (mémoire, vault, skills) : issue #207.

use super::*;

/// Une valeur du magasin sans forme de secret (adresse, URL, identifiant) ne condamne pas
/// les lignes qui la citent ; elle reste masquée dans les journaux (#26, #134).
#[test]
fn a_store_value_without_secret_shape_is_masked_but_not_forbidden() {
    let r = Redactor::default();
    for v in [
        "proprietaire-207@exemple.fr",
        "https://redmine-207.interne.exemple/projects/penelope",
        "identifiant-de-connexion-207",
    ] {
        r.register(v);
        let line = format!("- écrire à {v} pour le compte rendu");
        assert_eq!(r.forbidden_secret(&line), None, "{line}");
        assert!(!r.contains_secret(&line), "{line}");
        assert_eq!(r.secret_kind(&line), None, "{line}");
        assert!(!r.redact(&line).contains(v), "journaux : masqué quand même");
    }
}

/// Une valeur du magasin qui a la forme d'un secret, recopiée en clair, reste interdite.
#[test]
fn a_secret_shaped_store_value_stays_forbidden() {
    let r = Redactor::default();
    let v = "Qm7vT2xK9pLw4ZrB8nYc3HsD6fJa1GuE5oRq0XkNwy";
    r.register(v);
    let f = r
        .forbidden_secret(&format!("- la clé du service est {v}"))
        .expect("interdit");
    assert_eq!(f.kind, "secret enregistré");
    assert_eq!(f.fragment, "Qm7v…");
    assert!(f.certain);
}

/// Ce qui a été lu depuis le démarrage ne change pas le verdict : le critère ne dépend que
/// du texte et du magasin.
#[test]
fn a_learned_value_does_not_change_the_verdict() {
    let r = Redactor::default();
    let line = "- le paramètre vaut ab207cd34ef56gh78 dans la doc";
    assert_eq!(r.forbidden_secret(line), None);
    r.learn_secrets("const cfg = { apiKey: 'ab207cd34ef56gh78' };");
    assert_eq!(r.forbidden_secret(line), None);
}

/// Un vrai jeton est nommé et cité par son début.
#[test]
fn a_provider_key_is_named_with_its_fragment() {
    let r = Redactor::default();
    let f = r
        .forbidden_secret("- mot de passe ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    assert_eq!(f.kind, "jeton github");
    assert_eq!(f.fragment, "ghp_…");
    assert!(f.certain);
}

/// Un identifiant numérique cité en prose n'est pas un numéro de carte, même si sa clé de
/// Luhn est vraie ; les cas de #132 restent refusés.
#[test]
fn a_numeric_identifier_in_prose_is_not_a_card() {
    let r = Redactor::default();
    let id = "100000000000009";
    assert!(luhn(id));
    for line in [
        format!("- la page Meta `{id}` publie le lundi"),
        format!("- la page Meta {id} publie le lundi"),
    ] {
        assert_eq!(r.forbidden_secret(&line), None, "{line}");
    }
    for card in [
        "4539 1488 0343 6467",
        "4539-1488-0343-6467",
        "ma carte 4539148803436467.",
        "carte 4539148803436467",
        "carte:4539148803436467",
        "cb=4539-1488-0343-6467",
        "Visa se terminant par 4539148803436467",
    ] {
        let f = r.forbidden_secret(card).unwrap_or_else(|| panic!("{card}"));
        assert_eq!(f.kind, "numéro de carte", "{card}");
        assert_eq!(f.fragment, "…6467", "{card}");
        assert!(f.certain);
    }
}

/// #284 : un mot de passe dicté en prose est interdit dans ce qui est gardé, nommé et cité
/// par son début ; sans forme de jeton, il n'est pas certain. Une phrase qui parle du mot
/// de passe sans le donner passe, et `password: …` reste une affectation.
#[test]
fn a_dictated_password_is_forbidden_but_not_certain() {
    let r = Redactor::default();
    let f = r
        .forbidden_secret("- Le mot de passe du serveur de dev est Soleil2026.")
        .expect("interdit");
    assert_eq!(f.kind, "mot de passe");
    assert_eq!(f.fragment, "Sole…");
    assert!(!f.certain, "{f:?}");
    assert!(r.contains_secret("mdp : Toto1234"));
    assert_eq!(
        r.forbidden_secret("- Le mot de passe du wifi est obligatoire"),
        None
    );
    assert_eq!(
        r.forbidden_secret("password: Hunter2Hunter2").unwrap().kind,
        "affectation de secret"
    );
}

/// Une affectation qui décrit un schéma n'est pas certaine ; une affectation de jeton
/// aléatoire l'est.
#[test]
fn an_assignment_is_certain_only_for_a_secret_shaped_value() {
    let r = Redactor::default();
    let f = r
        .forbidden_secret("- le jeton se construit : token = base64url(<champs>)")
        .unwrap();
    assert_eq!(f.kind, "affectation de secret");
    assert!(!f.certain, "{f:?}");
    let f = r
        .forbidden_secret("KEY = \"Zx9kQ2mV7pLr4TbW1nHs8YcD3fGa6JuE0oIq5*RtKyNw2BvX\"")
        .unwrap();
    assert_eq!(f.kind, "affectation de secret");
    assert!(f.certain, "{f:?}");
    assert_eq!(f.fragment, "Zx9k…");
}
