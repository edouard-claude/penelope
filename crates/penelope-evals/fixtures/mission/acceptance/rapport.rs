//! Recette de la story « rapport » : la commande lit un fichier et imprime le stock.
use std::process::Command;

fn atelier(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(args)
        .output()
        .expect("binaire")
}

#[test]
fn rapport_prints_the_items_and_the_total() {
    let dir = std::env::temp_dir().join(format!("atelier-rapport-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("stock.txt");
    std::fs::write(&f, "vis;10;0.05\nmarteau;2;12.50\n").unwrap();
    let out = atelier(&["rapport", f.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("vis"), "{text}");
    assert!(text.contains("marteau"), "{text}");
    assert!(text.contains("Total : 25,50 €"), "{text}");
}

#[test]
fn a_bad_line_fails_with_its_number() {
    let dir = std::env::temp_dir().join(format!("atelier-rapport-ko-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("stock.txt");
    std::fs::write(&f, "vis;10;0.05\nmarteau;deux;12.50\n").unwrap();
    let out = atelier(&["rapport", f.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ligne 2"), "{err}");
}

#[test]
fn a_missing_file_fails_with_its_name() {
    let out = atelier(&["rapport", "/nulle/part/stock.txt"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("stock.txt"));
}
