use super::*;
use penelope_store::Store;

const SECRET: &str = "abcdefghijklmnop1234";

/// Une carte telle que `pipeline.rs` la crée : `kind`, `subject`, `payload.arguments`.
struct Card {
    id: &'static str,
    subject: &'static str,
    command: String,
    state: &'static str,
    risk: &'static str,
    double: bool,
    age_days: i64,
}

fn card(id: &'static str, command: &str, state: &'static str) -> Card {
    Card {
        id,
        subject: "shell_exec",
        command: command.into(),
        state,
        risk: "write",
        double: false,
        age_days: 1,
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-25T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn fixture() -> Vec<Card> {
    vec![
        // Deux fois la même ligne composée, à la mise en page près : une commande distincte.
        card("a1", "cargo test; cargo fmt", "approved"),
        card("a2", "  cargo   test;  cargo fmt ", "denied"),
        card("a3", "cargo test; cargo fmt", "approved"),
        // Une substitution : sans motif, approuvée.
        card("b1", "echo $(date) > out.txt", "approved"),
        // Un secret dans la ligne : compté, jamais affiché.
        card(
            "c1",
            &format!("curl -H 'Authorization: Bearer {SECRET}' https://x || true"),
            "expired",
        ),
        // Sans motif mais hors du juge : destructif, puis double confirmation.
        Card {
            risk: "destructive",
            ..card("d1", "rm -rf tmp; ls", "approved")
        },
        Card {
            double: true,
            ..card("d2", "git push; git status", "approved")
        },
        // Une ligne simple a un motif : hors mesure.
        card("e1", "cargo test", "approved"),
        // Hors fenêtre, autre outil : hors mesure.
        Card {
            age_days: 40,
            ..card("f1", "cargo test; cargo fmt", "approved")
        },
        Card {
            subject: "fs_write",
            ..card("g1", "cargo test; cargo fmt", "approved")
        },
    ]
}

fn seed(store: &Store, cards: Vec<Card>) {
    let reference = now();
    store
        .write_blocking(move |tx| {
            for c in cards {
                let created = (reference - Duration::days(c.age_days))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let payload = json!({
                    "tool": c.subject,
                    "arguments": {"command": c.command},
                    "double": c.double,
                });
                tx.execute(
                    "INSERT INTO approval_requests
                     (id, kind, subject, risk, payload, created_at, expires_at, state)
                     VALUES (?1, 'tool_call', ?2, ?3, ?4, ?5, ?5, ?6)",
                    rusqlite::params![
                        c.id,
                        c.subject,
                        c.risk,
                        payload.to_string(),
                        created,
                        c.state
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

/// Empreinte de ce qu'une écriture changerait : la base et son journal WAL. Le `-shm`
/// n'en fait pas partie : c'est un index de mémoire partagée que tout lecteur WAL tient à
/// jour, pas une donnée.
fn fingerprint(db: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let dir = db.parent().unwrap();
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let digest = if name.ends_with("-shm") {
            String::new()
        } else {
            penelope_kernel::canonical::sha256_hex(&std::fs::read(dir.join(&name)).unwrap())
        };
        out.push((name, digest));
    }
    out
}

fn check_counts(s: &Stats) {
    assert_eq!(s.cartes_shell, 8, "{s:#?}");
    assert_eq!(s.sans_motif, 7, "{s:#?}");
    assert_eq!(s.eligibles_juge, 5, "{s:#?}");
    assert_eq!(s.commandes_distinctes, 5, "{s:#?}");
    assert_eq!(s.par_etat.get("approved"), Some(&5));
    assert_eq!(s.par_etat.get("denied"), Some(&1));
    assert_eq!(s.par_etat.get("expired"), Some(&1));
    // 5 oui sur 6 tranchées.
    assert_eq!(s.part_oui, Some(0.83));
    let top = &s.plus_frequentes[0];
    assert_eq!(
        (top.commande.as_str(), top.cartes, top.oui),
        ("cargo test; cargo fmt", 3, 2)
    );
    assert_eq!(
        top.empreinte,
        penelope_kernel::canonical::sha256_hex(b"cargo test; cargo fmt")[..12]
    );
    // 5 éligibles en 30 jours : 1,2 par semaine, sous le seuil.
    assert_eq!(s.par_semaine, 1.2);
    assert!(!s.verdict.go);
    assert_eq!(s.verdict.raisons.len(), 1, "{:?}", s.verdict.raisons);
}

#[test]
fn counts_on_a_live_base_without_writing_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("penelope.db");
    let store = Store::open(&db).unwrap();
    seed(&store, fixture());

    // Le daemon tourne encore : sa base et son WAL ne bougent pas pendant la mesure.
    let before = fingerprint(&db);
    let conn = open_read_only(&db).unwrap();
    let stats = measure(&conn, now(), 30).unwrap();
    drop(conn);
    assert_eq!(fingerprint(&db), before);
    check_counts(&stats);

    let text = render(&stats);
    let json = serde_json::to_string(&stats).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    assert!(!json.contains(SECRET), "{json}");
    assert!(text.contains("no-go"), "{text}");
    store.close();
}

#[test]
fn counts_on_a_stopped_base_without_writing_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live").join("penelope.db");
    let store = Store::open(&live).unwrap();
    seed(&store, fixture());
    // Une base seule dans son répertoire, sans WAL ni `-shm` : le daemon arrêté.
    let db = dir.path().join("arrete").join("penelope.db");
    store.backup_to(&db).unwrap();
    store.close();

    let before = fingerprint(&db);
    let conn = open_read_only(&db).unwrap();
    let stats = measure(&conn, now(), 30).unwrap();
    drop(conn);
    assert_eq!(fingerprint(&db), before, "aucun fichier créé ni modifié");
    check_counts(&stats);
}

#[test]
fn the_read_only_connection_refuses_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("penelope.db");
    let store = Store::open(&db).unwrap();
    seed(&store, fixture());
    let conn = open_read_only(&db).unwrap();
    assert!(conn.execute("DELETE FROM approval_requests", []).is_err());
    assert!(
        conn.execute_batch("CREATE TABLE intrus(x INTEGER)")
            .is_err()
    );
    store.close();
}

#[test]
fn the_window_is_counted_in_days() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("penelope.db");
    let store = Store::open(&db).unwrap();
    seed(&store, fixture());
    let conn = open_read_only(&db).unwrap();
    // Sur 60 jours, la carte de J-40 entre.
    let wide = measure(&conn, now(), 60).unwrap();
    assert_eq!(wide.sans_motif, 8);
    assert_eq!(wide.plus_frequentes[0].cartes, 4);
    // Une base vide de cartes : rien à mesurer, pas de part de oui.
    let none = measure(&conn, now() - Duration::days(365), 30).unwrap();
    assert_eq!((none.cartes_shell, none.part_oui), (0, None));
    assert!(!none.verdict.go);
    store.close();
}

#[test]
fn the_verdict_needs_the_three_thresholds() {
    assert!(verdict(12.0, 8, Some(0.9)).go);
    assert_eq!(verdict(9.9, 8, Some(0.9)).raisons.len(), 1);
    assert_eq!(verdict(12.0, 4, Some(0.9)).raisons.len(), 1);
    assert_eq!(verdict(12.0, 8, Some(0.5)).raisons.len(), 1);
    assert_eq!(verdict(0.0, 0, None).raisons.len(), 3);
}

#[test]
fn a_long_command_is_cut_after_masking() {
    let long = format!("git commit -m \"{}\"; git push", "x".repeat(80));
    let s = shown(&long);
    assert!(s.ends_with('…'));
    assert_eq!(s.chars().count(), SHOWN_CHARS + 1);
    assert_eq!(normalise("  a \t b\n c "), "a b c");
}
