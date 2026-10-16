//! Hooks de cycle de vie (#382, épopée #386) : une commande que le propriétaire branche
//! sur un moment de la boucle (après un outil, avant, en fin de tour…), en forme exec
//! **sans shell**.
//!
//! ```text
//!  déclaration ──► correspond ? (événement, outil, chemin) ──► rendu par élément
//!                                                              ({file}, {workspace}…)
//!                         ──► spawn direct de `command` + `args` (jamais `sh`)
//!                             sous le bac à sable du shell, bornes du hook
//! ```
//!
//! Un argument n'est jamais découpé ni interprété : `a b.rs` arrive tel quel. Une valeur
//! substituée qui porte un caractère que le shell interpréterait est quand même refusée
//! (le hook n'est pas lancé, et il le dit) : la même règle que le `--hook` de mission, et
//! aucune surprise si une commande relaie ses arguments à un shell.
//!
//! Seul le `--hook` de mission (#366) garde sa ligne de shell, traduite en
//! `sh -c <ligne rendue>` par [`Hook::mission`] ; un hook déclaré n'a pas cette porte.

mod admin;
mod registry;
pub use admin::{EDITABLE, edit};
pub use registry::{Loaded, Registry, load_dir, parse, validate};

use crate::edit_hooks::safe_path;
use std::collections::BTreeMap;
use std::time::Duration;

/// Les moments de la boucle où un hook peut se brancher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    SessionStart,
    PromptSubmit,
    PreTool,
    PostTool,
    TurnEnd,
}

/// Ce qu'un hook qui échoue (code inattendu, délai, lancement impossible) décide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    #[default]
    Continue,
    Block,
}

/// Où va le résultat d'un hook.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deliver {
    /// Joint au résultat de l'outil, pour le modèle.
    #[default]
    Note,
    /// Une carte au propriétaire.
    Card,
    /// Journalisé seulement.
    Silent,
}

/// Bac à sable d'un hook : jamais plus large que l'écriture dans le workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

impl Sandbox {
    /// Le profil du bac à sable du shell qui lui correspond.
    pub fn profile_name(self) -> &'static str {
        match self {
            Sandbox::ReadOnly => "readonly",
            Sandbox::WorkspaceWrite => "workspace-write",
        }
    }

    /// Le profil effectif d'un hook (#383) : le sien là où la plateforme sait l'appliquer
    /// (macOS), jamais plus large que celui de la configuration ; sur une plateforme sans
    /// bac à sable où le propriétaire a choisi `full`, celui du shell, faute de quoi tout
    /// hook échouerait fermé.
    pub fn effective(self, configured: &str) -> &'static str {
        let rank = |p: &str| match p {
            "readonly" | "read-only" => 0,
            "full" => 2,
            _ => 1,
        };
        if configured == "full" && !cfg!(target_os = "macos") {
            return "full";
        }
        if rank(configured) < rank(self.profile_name()) {
            if rank(configured) == 0 {
                "readonly"
            } else {
                "workspace-write"
            }
        } else {
            self.profile_name()
        }
    }
}

/// À quels appels un hook répond : noms d'outils (`fs_write|fs_edit`) et motif de chemin
/// (`**/*.rs`). Un champ absent répond à tout.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Match {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

impl Match {
    pub fn matches(&self, tool: Option<&str>, path: Option<&str>) -> bool {
        let tool_ok = match (&self.tool, tool) {
            (None, _) => true,
            (Some(want), Some(t)) => want.split('|').map(str::trim).any(|w| w == t),
            (Some(_), None) => false,
        };
        let path_ok = match (&self.path, path) {
            (None, _) => true,
            (Some(glob), Some(p)) => path_matches(glob, p),
            (Some(_), None) => false,
        };
        tool_ok && path_ok
    }
}

/// Motif de chemin par segments : `**` couvre zéro ou plusieurs dossiers, `*` une partie
/// d'un nom. Aucun autre opérateur : la correspondance est déterministe et lisible.
pub fn path_matches(glob: &str, path: &str) -> bool {
    let g: Vec<&str> = glob.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    fn walk(g: &[&str], p: &[&str]) -> bool {
        match g.split_first() {
            None => p.is_empty(),
            Some((&"**", rest)) => (0..=p.len()).any(|i| walk(rest, &p[i..])),
            Some((seg, rest)) => match p.split_first() {
                Some((name, prest)) => {
                    crate::fs::matches_glob(name, Some(seg)) && walk(rest, prest)
                }
                None => false,
            },
        }
    }
    walk(&g, &p)
}

/// Une déclaration de hook.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hook {
    pub name: String,
    pub on: Event,
    #[serde(default, rename = "match")]
    pub matcher: Match,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_timeout", with = "secs")]
    pub timeout: Duration,
    #[serde(default)]
    pub sandbox: Sandbox,
    #[serde(default)]
    pub on_failure: OnFailure,
    #[serde(default)]
    pub deliver: Deliver,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_timeout() -> Duration {
    DEFAULT_TIMEOUT
}

/// Délai d'un hook sans `timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Sortie lue d'un hook, en octets.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Sortie d'un hook renvoyée au modèle, en caractères (la fin, là où est l'erreur).
pub const NOTE_CHARS: usize = 4_000;

mod secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}s", d.as_secs()))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let raw = String::deserialize(d)?;
        penelope_kernel::config::parse_duration(&raw).map_err(serde::de::Error::custom)
    }
}

/// Les valeurs qu'un hook peut recevoir, par élément.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    pub file: Option<String>,
    pub workspace: Option<String>,
    pub session: Option<String>,
    pub tool: Option<String>,
    pub data: Option<String>,
}

/// Un hook prêt à lancer : programme, arguments et dossier rendus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

/// Le motif d'un hook qu'on ne lance pas, dit au modèle tel quel.
pub const UNSAFE_VALUE: &str = "chemin avec un caractère spécial du shell : hook non lancé";

impl Hook {
    /// Le `--hook` de mission (#366) : sa ligne de shell, déjà rendue (`{file}` cité par
    /// `edit_hooks::render`), devient `sh -c <ligne>` après `fs_write` et `fs_edit`. La
    /// seule porte vers un shell : un hook déclaré n'y passe jamais.
    pub fn mission(rendered_line: &str) -> Hook {
        Hook {
            name: "mission".into(),
            on: Event::PostTool,
            matcher: Match {
                tool: Some("fs_write|fs_edit".into()),
                path: None,
            },
            command: "sh".into(),
            args: vec!["-c".into(), rendered_line.into()],
            cwd: None,
            timeout: DEFAULT_TIMEOUT,
            sandbox: Sandbox::WorkspaceWrite,
            on_failure: OnFailure::Continue,
            deliver: Deliver::Note,
            env: BTreeMap::new(),
        }
    }

    /// Ce hook répond-il à cet événement, pour cet outil et ce chemin ?
    pub fn applies(&self, event: Event, tool: Option<&str>, path: Option<&str>) -> bool {
        self.on == event && self.matcher.matches(tool, path)
    }

    /// Rend `args` et `cwd` élément par élément ; `Err` si une valeur substituée porte
    /// un caractère que le shell interprète ([`UNSAFE_VALUE`]).
    pub fn render(&self, vars: &Vars) -> Result<Rendered, String> {
        let one = |raw: &str| -> Result<String, String> {
            let mut out = raw.to_string();
            for (key, value) in [
                ("{file}", &vars.file),
                ("{workspace}", &vars.workspace),
                ("{session}", &vars.session),
                ("{tool}", &vars.tool),
                ("{data}", &vars.data),
            ] {
                if !out.contains(key) {
                    continue;
                }
                let v = value.as_deref().unwrap_or_default();
                if !safe_path(v) {
                    return Err(UNSAFE_VALUE.into());
                }
                out = out.replace(key, v);
            }
            Ok(out)
        };
        Ok(Rendered {
            program: self.command.clone(),
            args: self.args.iter().map(|a| one(a)).collect::<Result<_, _>>()?,
            cwd: self.cwd.as_deref().map(one).transpose()?,
        })
    }
}

/// Le résultat d'un hook lancé, tel que l'outil le joint (`hook`).
pub fn note_of(label: &str, out: &crate::shell::ShellOutput) -> serde_json::Value {
    if out.exit_code == 0 {
        return serde_json::json!({"command": label, "exitCode": 0});
    }
    let text = format!("{}{}", out.stdout, out.stderr);
    let chars: Vec<char> = text.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(NOTE_CHARS)..]
        .iter()
        .collect();
    serde_json::json!({"command": label, "exitCode": out.exit_code, "output": tail,
        "note": "le hook échoue après cette édition : corrige avant de continuer"})
}

/// Le verdict d'un hook d'après son code de sortie et sa sortie : 0 = `continue` (la
/// sortie est une note), 2 = verdict JSON `{"verdict":"continue|block","reason","note"}`,
/// tout autre code = échec tranché par `on_failure` (`échec` ou `block`). Rend le
/// verdict et la note bornée.
pub fn verdict_of(
    exit_code: i32,
    output: &str,
    on_failure: OnFailure,
) -> (&'static str, Option<String>) {
    let bounded = |t: &str| -> Option<String> {
        let t = t.trim();
        let chars: Vec<char> = t.chars().collect();
        (!t.is_empty()).then(|| {
            chars[chars.len().saturating_sub(NOTE_CHARS)..]
                .iter()
                .collect()
        })
    };
    let failed = |note: Option<String>| match on_failure {
        OnFailure::Block => ("block", note),
        OnFailure::Continue => ("échec", note),
    };
    match exit_code {
        0 => ("continue", bounded(output)),
        2 => {
            let v: serde_json::Value =
                serde_json::from_str(output.trim()).unwrap_or(serde_json::Value::Null);
            let note = v["note"]
                .as_str()
                .or(v["reason"].as_str())
                .and_then(bounded);
            match v["verdict"].as_str() {
                Some("block") => ("block", note),
                Some("continue") => ("continue", note),
                // `allow` n'existe pas : un hook ne desserre jamais (#384).
                _ => failed(bounded("verdict illisible (continue ou block attendu)")),
            }
        }
        _ => failed(bounded(output)),
    }
}

/// Lance un hook rendu, sans shell, sous le profil donné et dans `cwd`.
pub async fn run(
    host: &penelope_platform::UnixProcessHost,
    hook: &Hook,
    rendered: &Rendered,
    profile: &penelope_platform::Profile,
    cwd: &std::path::Path,
) -> crate::error::ToolResult<crate::shell::ShellOutput> {
    crate::shell::exec_argv(
        host,
        &rendered.program,
        &rendered.args,
        crate::shell::ExecOptions {
            profile: Some(profile),
            cwd: Some(cwd),
            timeout: hook.timeout,
            max_output_bytes: MAX_OUTPUT_BYTES,
            env: hook.env.clone().into_iter().collect(),
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
mod tests;
