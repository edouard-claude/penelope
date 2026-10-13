//! Recette de la story « rôles » : seul un administrateur promeut ou rétrograde.
use atelier::{Role, User, demote, promote};

fn user(name: &str, role: Role) -> User {
    User {
        name: name.into(),
        role,
    }
}

#[test]
fn an_admin_promotes_a_user() {
    let admin = user("alice", Role::Admin);
    let mut bob = user("bob", Role::User);
    assert_eq!(promote(&admin, &mut bob), Ok(()));
    assert_eq!(bob.role, Role::Admin);
}

#[test]
fn a_user_cannot_promote() {
    let carol = user("carol", Role::User);
    let mut bob = user("bob", Role::User);
    let err = promote(&carol, &mut bob).unwrap_err();
    assert!(err.to_lowercase().contains("refus"), "{err}");
    assert_eq!(bob.role, Role::User);
}

#[test]
fn an_admin_cannot_demote_themself() {
    let mut alice = user("alice", Role::Admin);
    let by = alice.clone();
    assert!(demote(&by, &mut alice).is_err());
    assert_eq!(alice.role, Role::Admin);
}

#[test]
fn an_admin_demotes_another_admin() {
    let alice = user("alice", Role::Admin);
    let mut dan = user("dan", Role::Admin);
    assert_eq!(demote(&alice, &mut dan), Ok(()));
    assert_eq!(dan.role, Role::User);
}
