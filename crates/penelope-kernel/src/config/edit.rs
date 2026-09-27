//! Lecture et réécriture ciblée du fichier TOML (issue #76).

use super::*;

pub(super) const SAMPLE_HEADER: &str = "\
# Configuration de Pénélope.
#
# Seules les clés qui s'écartent des valeurs par défaut figurent ici. Toutes les clés,
# leur valeur par défaut et leur rôle : `penelope config get`, ou la référence des clés
# de docs/install-headless.md. Une modification par `penelope config set` ne touche que
# la clé visée : commentaires et ordre de ce fichier sont conservés.

";

/// Clés présentes dans `raw` et absentes de la configuration relue : ce que ce binaire
/// ne connaît pas. Les tables libres (alias, seuils par modèle…) sont relues avec leurs
/// clés, donc jamais signalées.
pub(super) fn unknown_keys(
    prefix: &str,
    raw: &toml::Table,
    known: &toml::Table,
    out: &mut Vec<String>,
) {
    for (k, v) in raw {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (v, known.get(k)) {
            (_, None) => out.push(path),
            (toml::Value::Table(r), Some(toml::Value::Table(kn))) => {
                unknown_keys(&path, r, kn, out)
            }
            _ => {}
        }
    }
}

/// Réécrit `existing` en ne touchant que les clés qui changent entre `before` et `after`
/// (issue #76) : commentaires, ordre, clés inconnues et clés omises par le propriétaire
/// restent tels quels, et le fichier n'acquiert pas les clés nouvelles d'une version tant
/// que personne ne les pose.
pub fn edit_toml(existing: &str, before: &Config, after: &Config) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e: toml_edit::TomlError| KernelError::config(e.to_string()))?;
    let b = toml::Value::try_from(before).map_err(|e| KernelError::config(e.to_string()))?;
    let a = toml::Value::try_from(after).map_err(|e| KernelError::config(e.to_string()))?;
    if let (Some(b), Some(a)) = (b.as_table(), a.as_table()) {
        patch_table(doc.as_table_mut(), Some(b), a)?;
    }
    Ok(doc.to_string())
}

fn edit_value(v: &toml::Value) -> Result<toml_edit::Value> {
    v.to_string()
        .parse::<toml_edit::Value>()
        .map_err(|e| KernelError::config(e.to_string()))
}

/// Remplace une valeur en gardant sa décoration (commentaire en fin de ligne).
fn set_value(item: &mut toml_edit::Item, v: &toml::Value) -> Result<()> {
    let mut next = edit_value(v)?;
    if let Some(old) = item.as_value() {
        *next.decor_mut() = old.decor().clone();
    }
    *item = toml_edit::Item::Value(next);
    Ok(())
}

fn patch_table(
    doc: &mut toml_edit::Table,
    before: Option<&toml::Table>,
    after: &toml::Table,
) -> Result<()> {
    for (k, av) in after {
        let bv = before.and_then(|b| b.get(k));
        if bv == Some(av) {
            continue;
        }
        match (av, doc.get_mut(k)) {
            (toml::Value::Table(at), Some(toml_edit::Item::Table(t))) => {
                patch_table(t, bv.and_then(|v| v.as_table()), at)?
            }
            (toml::Value::Table(at), None) => {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                patch_table(&mut t, bv.and_then(|v| v.as_table()), at)?;
                doc.insert(k, toml_edit::Item::Table(t));
            }
            // Valeur scalaire, tableau ou table en ligne : remplacée d'un bloc.
            (_, Some(item)) => set_value(item, av)?,
            (_, None) => {
                doc.insert(k, toml_edit::Item::Value(edit_value(av)?));
            }
        }
    }
    if let Some(b) = before {
        for k in b.keys() {
            if !after.contains_key(k) {
                doc.remove(k);
            }
        }
    }
    Ok(())
}
