//! `hooks.d/` (#383) : un fichier TOML par hook, nom du fichier = nom du hook, rechargé
//! à la consultation quand le dossier a changé (même empreinte que `mcp.d` : nom, date,
//! taille). Un fichier invalide est signalé et ignoré ; les autres hooks tournent.

use super::{Event, Hook};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// L'état chargé du dossier.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub fingerprint: String,
    pub hooks: Vec<Hook>,
    /// Fichiers refusés et leur motif.
    pub invalid: Vec<(String, String)>,
}

/// Les hooks du propriétaire, relus quand `hooks.d/` change.
#[derive(Debug)]
pub struct Registry {
    dir: PathBuf,
    state: Mutex<Loaded>,
}

impl Registry {
    pub fn new(dir: PathBuf) -> Self {
        Registry {
            dir,
            state: Mutex::new(Loaded::default()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Les hooks à jour : le dossier est relu seulement si son empreinte a changé.
    pub fn current(&self) -> Loaded {
        let fp = fingerprint(&self.dir);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.fingerprint != fp || fp.is_empty() && !state.hooks.is_empty() {
            let (hooks, invalid) = load_dir(&self.dir);
            *state = Loaded {
                fingerprint: fp,
                hooks,
                invalid,
            };
        }
        state.clone()
    }

    /// Un hook par son nom.
    pub fn get(&self, name: &str) -> Option<Hook> {
        self.current().hooks.into_iter().find(|h| h.name == name)
    }
}

/// `nom:date_ms:taille` des `*.toml`, triés.
fn fingerprint(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    let mut parts: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let ms = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis();
            Some(format!(
                "{}:{ms}:{}",
                e.file_name().to_string_lossy(),
                m.len()
            ))
        })
        .collect();
    parts.sort();
    parts.join("|")
}

/// Lit `hooks.d/*.toml` : les hooks valides, et les fichiers refusés avec leur motif.
pub fn load_dir(dir: &Path) -> (Vec<Hook>, Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    let (mut hooks, mut invalid) = (Vec::new(), Vec::new());
    for path in paths {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| parse(&name, &raw))
        {
            Ok(h) => hooks.push(h),
            Err(e) => invalid.push((name, e)),
        }
    }
    (hooks, invalid)
}

/// Une déclaration lue et validée ; le nom vient du fichier.
pub fn parse(name: &str, raw: &str) -> Result<Hook, String> {
    if penelope_platform::validate_slug(name).is_err() {
        return Err(format!(
            "nom de fichier `{name}` : lettres, chiffres, `-` et `_`"
        ));
    }
    let mut table: toml::Table = toml::from_str(raw).map_err(|e| e.to_string())?;
    if table.contains_key("name") {
        return Err("`name` vient du nom du fichier, pas d'un champ".into());
    }
    table.insert("name".into(), toml::Value::String(name.to_string()));
    let json = serde_json::to_value(&table).map_err(|e| e.to_string())?;
    let hook: Hook = serde_json::from_value(json).map_err(|e| e.to_string())?;
    validate(&hook)?;
    Ok(hook)
}

/// Les règles d'une déclaration : un événement servi, une commande, aucun secret hors de
/// `env`.
pub fn validate(hook: &Hook) -> Result<(), String> {
    if hook.input == super::Input::Text && hook.on != Event::PromptSubmit {
        return Err("`input = \"text\"` n'a de sens que pour `prompt_submit`".into());
    }
    if hook.command.trim().is_empty() {
        return Err("`command` est vide".into());
    }
    let secret_outside_env = hook
        .args
        .iter()
        .chain(hook.cwd.iter())
        .chain(std::iter::once(&hook.command))
        .any(|v| v.contains("${SECRET:") || v.contains("${ENV:"));
    if secret_outside_env {
        return Err("un secret ne passe que par `env` (`env = { X = \"${SECRET:x}\" }`)".into());
    }
    Ok(())
}
