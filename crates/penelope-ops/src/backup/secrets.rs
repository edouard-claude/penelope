//! Les valeurs des secrets dans l'archive (#327).
//!
//! ```text
//!  magasin (trousseau) ─► {nom: valeur} ─► seconde couche (même phrase, autre sel)
//!                                          └─► penelope/secrets.enc dans le tar
//! ```
//!
//! La phrase de passe des sauvegardes n'y est pas : la restauration la reçoit de qui la
//! tape, et la range elle-même. Un secret illisible (trousseau verrouillé, entrée
//! corrompue) ne fait pas échouer la sauvegarde : son nom va dans le manifeste, à
//! ressaisir après restauration.

use super::PASSPHRASE_SECRET;
use penelope_platform::SecretStore;
use std::collections::BTreeMap;

/// Nom du fichier des secrets dans l'archive.
pub const FILE: &str = "secrets.enc";

/// Les secrets lus dans le magasin.
#[derive(Debug, Default)]
pub struct Dump {
    pub values: BTreeMap<String, String>,
    /// Noms listés mais illisibles : à ressaisir.
    pub unreadable: Vec<String>,
}

/// Lit chaque secret du magasin, la phrase de passe des sauvegardes exceptée.
pub fn dump(store: &dyn SecretStore) -> Dump {
    let mut out = Dump::default();
    for name in store.list().unwrap_or_default() {
        if name == PASSPHRASE_SECRET {
            continue;
        }
        match store.get(&name) {
            Ok(Some(v)) => {
                out.values.insert(name, v);
            }
            Ok(None) | Err(_) => out.unreadable.push(name),
        }
    }
    out
}

/// Chiffre les valeurs sous leur seconde couche.
pub fn seal(values: &BTreeMap<String, String>, passphrase: &str) -> anyhow::Result<Vec<u8>> {
    let plain = serde_json::to_vec(values)?;
    penelope_platform::archive::seal_secrets(&plain, passphrase)
        .map_err(|e| anyhow::anyhow!("chiffrement des secrets : {e}"))
}

/// Déchiffre les valeurs d'une archive.
pub fn open(raw: &[u8], passphrase: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let plain = penelope_platform::archive::open_secrets(raw, passphrase)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(serde_json::from_slice(&plain)?)
}

/// Range les valeurs dans le magasin, puis la phrase de passe tapée à la restauration.
/// Renvoie les noms rangés et ceux qui ont échoué, avec la cause.
pub fn restore(
    store: &dyn SecretStore,
    values: &BTreeMap<String, String>,
    passphrase: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    let all = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain([(PASSPHRASE_SECRET, passphrase)]);
    for (name, value) in all {
        match store.set(name, value) {
            Ok(()) => done.push(name.to_string()),
            Err(e) => failed.push((name.to_string(), e.to_string())),
        }
    }
    (done, failed)
}
