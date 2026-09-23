//! Le cliquet de `budget.toml` entre deux commits (R3 côté CI, #209) : ce que le budget de
//! HEAD n'a pas le droit de faire par rapport à celui de la base, sauf dérogation.
//!
//! La partie git (base, renommages, trailer) est dans `scripts/check-budget.sh` ; ici,
//! seulement la comparaison de deux contenus TOML, pour qu'elle soit testée par
//! `cargo test`.

use std::collections::BTreeMap;

use crate::snapshot::DAEMON_SRC;

/// Rappel ajouté à chaque refus.
pub const HINT: &str = "(ajouter « Dérogation-budget: #NNN » au message de commit si c'est décidé)";

/// Tables d'entiers plafonds, avec le préfixe que leurs clés omettent : pas d'entrée
/// ajoutée, pas de hausse.
const CEILING_TABLES: &[(&str, &str)] = &[
    ("files.oversized", ""),
    ("daemon.daemon_users", DAEMON_SRC),
    ("channel.allowed", ""),
];

/// Entiers plafonds isolés : pas de hausse.
const CEILING_SCALARS: &[&str] = &[
    "files.ceiling",
    "files.test_ceiling",
    "lints.allow_too_many_lines",
];

/// Listes qui ne peuvent que raccourcir.
const SHRINKING_LISTS: &[&str] = &[
    "files.test_modules",
    "daemon.modules",
    "daemon.impl_daemon",
    "scenarios.missing",
];

/// Listes qui ne peuvent que s'allonger.
const GROWING_LISTS: &[&str] = &["ca.required"];

/// Planchers (R9, à venir) : pas de baisse, pas de suppression.
const FLOOR_SCALARS: &[&str] = &["coverage.new_file_floor"];
const FLOOR_TABLES: &[&str] = &["coverage.crates"];

/// Compare le budget de la base à celui de HEAD ; chaque refus est une ligne.
///
/// `renames` : `ancien chemin → nouveau chemin` (repo-relatifs), tels que `git diff
/// --find-renames` les donne ; une entrée reportée sous son nouveau chemin avec un nombre
/// inférieur ou égal n'est ni un ajout ni une hausse.
pub fn regressions(
    base: &str,
    head: &str,
    renames: &BTreeMap<String, String>,
    base_label: &str,
) -> Result<Vec<String>, String> {
    let base: toml::Value = base
        .parse()
        .map_err(|e| format!("budget.toml de {base_label} : TOML invalide : {e}"))?;
    let head: toml::Value = head
        .parse()
        .map_err(|e| format!("budget.toml de HEAD : TOML invalide : {e}"))?;
    let mut out = Vec::new();

    for path in CEILING_SCALARS {
        if let (Some(b), Some(h)) = (int_at(&base, path), int_at(&head, path))
            && h > b
        {
            out.push(rise(path, "", b, h, base_label));
        }
    }
    for (path, prefix) in CEILING_TABLES {
        let b = int_table(&base, path);
        let h = int_table(&head, path);
        for (key, hv) in &h {
            match b.get(&former_key(key, prefix, renames)) {
                None => out.push(format!(
                    "budget.toml [{path}] : entrée ajoutée \"{key}\" = {hv} par rapport à \
                     {base_label} ; la liste de référence ne peut que rétrécir {HINT}"
                )),
                Some(bv) if hv > bv => out.push(rise(path, key, *bv, *hv, base_label)),
                Some(_) => {}
            }
        }
    }
    {
        let b = int_table(&base, "crates");
        let h = int_table(&head, "crates");
        for (key, bv) in &b {
            match h.get(key) {
                None => out.push(format!(
                    "budget.toml [crates] : plafond retiré \"{key}\" par rapport à \
                     {base_label} ; un plafond de crate ne se retire pas {HINT}"
                )),
                Some(hv) if hv > bv => out.push(rise("crates", key, *bv, *hv, base_label)),
                Some(_) => {}
            }
        }
    }
    for path in SHRINKING_LISTS {
        let b = str_list(&base, path);
        for name in str_list(&head, path) {
            if !b.contains(&name) {
                out.push(format!(
                    "budget.toml [{path}] : entrée ajoutée \"{name}\" par rapport à \
                     {base_label} ; cette liste ne peut que raccourcir {HINT}"
                ));
            }
        }
    }
    for path in GROWING_LISTS {
        let h = str_list(&head, path);
        for name in str_list(&base, path) {
            if !h.contains(&name) {
                out.push(format!(
                    "budget.toml [{path}] : critère retiré \"{name}\" par rapport à \
                     {base_label} ; les critères d'acceptation ne disparaissent pas \
                     (le renommer est interdit, le déplacer est permis) {HINT}"
                ));
            }
        }
    }
    for path in FLOOR_SCALARS {
        if let (Some(b), Some(h)) = (int_at(&base, path), int_at(&head, path))
            && h < b
        {
            out.push(fall(path, "", b, h, base_label));
        }
    }
    for path in FLOOR_TABLES {
        let b = int_table(&base, path);
        let h = int_table(&head, path);
        for (key, bv) in &b {
            match h.get(key) {
                None => out.push(format!(
                    "budget.toml [{path}] : plancher retiré \"{key}\" par rapport à \
                     {base_label} ; un plancher ne se retire pas {HINT}"
                )),
                Some(hv) if hv < bv => out.push(fall(path, key, *bv, *hv, base_label)),
                Some(_) => {}
            }
        }
    }
    Ok(out)
}

fn rise(path: &str, key: &str, from: i64, to: i64, base_label: &str) -> String {
    format!(
        "budget.toml : {} passe de {from} à {to} par rapport à {base_label} ; un budget ne \
         monte jamais {HINT}",
        entry(path, key)
    )
}

fn fall(path: &str, key: &str, from: i64, to: i64, base_label: &str) -> String {
    format!(
        "budget.toml : {} passe de {from} à {to} par rapport à {base_label} ; un plancher \
         ne descend jamais {HINT}",
        entry(path, key)
    )
}

fn entry(path: &str, key: &str) -> String {
    match (key.is_empty(), path.rsplit_once('.')) {
        (true, Some((table, name))) => format!("[{table}].{name}"),
        (true, None) => path.to_string(),
        (false, _) => format!("\"{key}\""),
    }
}

/// La clé qu'une entrée de HEAD avait dans la base, en tenant compte des renommages.
fn former_key(key: &str, prefix: &str, renames: &BTreeMap<String, String>) -> String {
    let full = format!("{prefix}{key}");
    renames
        .iter()
        .find(|(_, new)| **new == full)
        .and_then(|(old, _)| old.strip_prefix(prefix))
        .map_or_else(|| key.to_string(), str::to_string)
}

fn at<'a>(v: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.').try_fold(v, |cur, seg| cur.get(seg))
}

fn int_at(v: &toml::Value, path: &str) -> Option<i64> {
    at(v, path).and_then(|x| x.as_integer())
}

fn int_table(v: &toml::Value, path: &str) -> BTreeMap<String, i64> {
    at(v, path)
        .and_then(|t| t.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, x)| x.as_integer().map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default()
}

fn str_list(v: &toml::Value, path: &str) -> Vec<String> {
    at(v, path)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[files]
ceiling = 1000
test_ceiling = 1500
test_modules = ["crates/x/src/e2e.rs"]
[files.oversized]
"crates/penelope-daemon/src/dream.rs" = 5859
"crates/penelope-daemon/src/old.rs" = 1200
[crates]
"penelope-daemon" = 80000
[daemon]
modules = ["agent", "dream"]
impl_daemon = ["runtime.rs"]
[daemon.daemon_users]
"voice.rs" = 5
"moved.rs" = 3
[lints]
allow_too_many_lines = 25
[ca]
required = ["ca_1_1_a", "ca_5_4_b"]
[channel.allowed]
"crates/penelope-daemon/src/media.rs" = 2
[coverage]
new_file_floor = 90
[coverage.crates]
"penelope-kernel" = 70
"#;

    fn check(head: &str) -> Vec<String> {
        regressions(BASE, head, &BTreeMap::new(), "a1b2c3d").unwrap()
    }

    #[test]
    fn an_identical_budget_passes() {
        assert!(check(BASE).is_empty());
    }

    #[test]
    fn lowering_and_removing_ceilings_passes() {
        let head = BASE
            .replace("= 5859", "= 5800")
            .replace("\"crates/penelope-daemon/src/old.rs\" = 1200\n", "")
            .replace("\"voice.rs\" = 5", "\"voice.rs\" = 4")
            .replace("= 25", "= 24")
            .replace("modules = [\"agent\", \"dream\"]", "modules = [\"agent\"]")
            .replace("\"ca_5_4_b\"]", "\"ca_5_4_b\", \"ca_16_1_new\"]")
            .replace("\"penelope-kernel\" = 70", "\"penelope-kernel\" = 75");
        assert!(check(&head).is_empty(), "{:?}", check(&head));
    }

    #[test]
    fn a_rise_is_refused_with_the_issue_message() {
        let head = BASE.replace("= 5859", "= 5900");
        let r = check(&head);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(
            r[0].starts_with(
                "budget.toml : \"crates/penelope-daemon/src/dream.rs\" passe de 5859 à 5900 par \
             rapport à a1b2c3d ; un budget ne monte jamais (ajouter « Dérogation-budget: #NNN »"
            ),
            "{}",
            r[0]
        );
    }

    #[test]
    fn an_added_entry_is_refused() {
        let head = BASE.replace(
            "[crates]",
            "\"crates/penelope-daemon/src/media.rs\" = 1042\n[crates]",
        );
        let r = check(&head);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains(
            "[files.oversized] : entrée ajoutée \"crates/penelope-daemon/src/media.rs\" = 1042"
        ));
    }

    #[test]
    fn a_removed_acceptance_criterion_is_refused_but_an_added_one_passes() {
        let head = BASE.replace("\"ca_5_4_b\"", "\"ca_5_4_renamed\"");
        let r = check(&head);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("critère retiré \"ca_5_4_b\""));
    }

    #[test]
    fn whitelists_cannot_grow_and_scalars_cannot_rise() {
        let head = BASE
            .replace(
                "modules = [\"agent\", \"dream\"]",
                "modules = [\"agent\", \"dream\", \"notifications\"]",
            )
            .replace(
                "impl_daemon = [\"runtime.rs\"]",
                "impl_daemon = [\"runtime.rs\", \"dream.rs\"]",
            )
            .replace("= 25", "= 26")
            .replace("ceiling = 1000", "ceiling = 1100")
            .replace(
                "test_modules = [\"crates/x/src/e2e.rs\"]",
                "test_modules = [\"crates/x/src/e2e.rs\", \"crates/x/src/f.rs\"]",
            )
            .replace("\"penelope-daemon\" = 80000", "\"penelope-daemon\" = 90000");
        let r = check(&head);
        assert_eq!(r.len(), 6, "{}", r.join("\n"));
        assert!(
            r.iter()
                .any(|m| m.contains("[daemon.modules] : entrée ajoutée \"notifications\""))
        );
        assert!(
            r.iter()
                .any(|m| m.contains("[lints].allow_too_many_lines passe de 25 à 26"))
        );
        assert!(
            r.iter()
                .any(|m| m.contains("[files].ceiling passe de 1000 à 1100"))
        );
        assert!(
            r.iter()
                .any(|m| m.contains("\"penelope-daemon\" passe de 80000 à 90000"))
        );
    }

    #[test]
    fn removing_a_crate_ceiling_or_lowering_a_floor_is_refused() {
        let head = BASE
            .replace("\"penelope-daemon\" = 80000\n", "")
            .replace("new_file_floor = 90", "new_file_floor = 80")
            .replace("\"penelope-kernel\" = 70", "\"penelope-kernel\" = 60");
        let r = check(&head);
        assert_eq!(r.len(), 3, "{}", r.join("\n"));
        assert!(
            r.iter()
                .any(|m| m.contains("plafond retiré \"penelope-daemon\""))
        );
        assert!(
            r.iter()
                .any(|m| m.contains("un plancher ne descend jamais"))
        );
    }

    #[test]
    fn a_renamed_file_keeps_its_entry_under_the_new_path() {
        let head = BASE
            .replace(
                "\"crates/penelope-daemon/src/dream.rs\" = 5859",
                "\"crates/penelope-daemon/src/dream/mod.rs\" = 5859",
            )
            .replace("\"moved.rs\" = 3", "\"dream/moved.rs\" = 3");
        let renames: BTreeMap<String, String> = [
            (
                "crates/penelope-daemon/src/dream.rs".to_string(),
                "crates/penelope-daemon/src/dream/mod.rs".to_string(),
            ),
            (
                "crates/penelope-daemon/src/moved.rs".to_string(),
                "crates/penelope-daemon/src/dream/moved.rs".to_string(),
            ),
        ]
        .into();
        let r = regressions(BASE, &head, &renames, "a1b2c3d").unwrap();
        assert!(r.is_empty(), "{r:?}");
        // Sous le nouveau chemin, la valeur ne peut pas non plus monter.
        let up = head.replace("dream/mod.rs\" = 5859", "dream/mod.rs\" = 5860");
        let r = regressions(BASE, &up, &renames, "a1b2c3d").unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("passe de 5859 à 5860"));
    }

    #[test]
    fn a_file_renamed_out_of_the_daemon_is_a_fresh_entry() {
        let head = BASE.replace("\"moved.rs\" = 3", "\"elsewhere.rs\" = 3");
        let renames: BTreeMap<String, String> = [(
            "crates/penelope-daemon/src/moved.rs".to_string(),
            "crates/penelope-app/src/elsewhere.rs".to_string(),
        )]
        .into();
        let r = regressions(BASE, &head, &renames, "a1b2c3d").unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("entrée ajoutée \"elsewhere.rs\""));
    }

    #[test]
    fn invalid_toml_is_an_error_not_a_pass() {
        assert!(regressions(BASE, "[files\n", &BTreeMap::new(), "x").is_err());
    }
}
