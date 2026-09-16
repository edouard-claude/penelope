//! Registre des workflows (§12.1) : `bundled` < `{data}/workflows/` <
//! `<workspace>/.penelope/workflows/`, un fichier `<id>.workflow.json`, surveillés à chaud.
//!
//! **Un fichier invalide est rejeté** et la version précédente reste active (§12.6).

use crate::model::Workflow;
use crate::validate::{Known, Report};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Bundled,
    User,
    Workspace,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Bundled => "bundled",
            Scope::User => "user",
            Scope::Workspace => "workspace",
        }
    }
    pub fn priority(&self) -> u8 {
        match self {
            Scope::Bundled => 0,
            Scope::User => 1,
            Scope::Workspace => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub workflow: Workflow,
    pub scope: Scope,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub path: PathBuf,
    pub report: Report,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} :\n{}", self.path.display(), self.report.render())
    }
}

#[derive(Default)]
struct State {
    workflows: BTreeMap<String, Entry>,
    errors: Vec<LoadError>,
    generation: u64,
}

/// Registre publié par génération.
#[derive(Clone, Default)]
pub struct WorkflowRegistry {
    state: Arc<RwLock<State>>,
}

impl WorkflowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge les workflows livrés.
    pub fn with_bundled(known: &Known) -> Self {
        let r = Self::new();
        {
            let mut g = r.state.write().unwrap_or_else(|p| p.into_inner());
            for w in crate::bundled::all() {
                let report = crate::validate::validate(&w, Some(&w.metadata.id), known);
                if report.is_valid() {
                    g.workflows.insert(
                        w.metadata.id.clone(),
                        Entry {
                            workflow: w,
                            scope: Scope::Bundled,
                            path: None,
                        },
                    );
                } else {
                    g.errors.push(LoadError {
                        path: PathBuf::from(format!("bundled/{}", w.metadata.id)),
                        report,
                    });
                }
            }
            g.generation = 1;
        }
        r
    }

    /// Charge un répertoire. Les fichiers invalides sont **rejetés**, les précédents
    /// restent en place.
    pub fn load_dir(&self, dir: &Path, scope: Scope, known: &Known) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().ends_with(".workflow.json"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();

        let mut loaded = 0;
        let mut g = self.state.write().unwrap_or_else(|p| p.into_inner());
        g.errors.retain(|e| !e.path.starts_with(dir));

        for p in paths {
            let stem = p
                .file_name()
                .map(|n| n.to_string_lossy().replace(".workflow.json", ""))
                .unwrap_or_default();
            let raw = match std::fs::read_to_string(&p) {
                Ok(r) => r,
                Err(e) => {
                    g.errors.push(LoadError {
                        path: p.clone(),
                        report: Report {
                            issues: vec![crate::validate::Issue {
                                path: "/".into(),
                                message: e.to_string(),
                                severity: crate::validate::Severity::Error,
                            }],
                        },
                    });
                    continue;
                }
            };
            let w = match Workflow::from_json(&raw) {
                Ok(w) => w,
                Err(e) => {
                    g.errors.push(LoadError {
                        path: p.clone(),
                        report: Report {
                            issues: vec![crate::validate::Issue {
                                path: "/".into(),
                                message: format!("JSON invalide : {e}"),
                                severity: crate::validate::Severity::Error,
                            }],
                        },
                    });
                    continue;
                }
            };
            let report = crate::validate::validate(&w, Some(&stem), known);
            if !report.is_valid() {
                g.errors.push(LoadError {
                    path: p.clone(),
                    report,
                });
                // La version précédente reste active : on ne retire rien.
                continue;
            }
            let id = w.metadata.id.clone();
            let replace = match g.workflows.get(&id) {
                Some(existing) => scope.priority() >= existing.scope.priority(),
                None => true,
            };
            if replace {
                g.workflows.insert(
                    id,
                    Entry {
                        workflow: w,
                        scope,
                        path: Some(p),
                    },
                );
                loaded += 1;
            }
        }
        g.generation += 1;
        loaded
    }

    pub fn get(&self, id: &str) -> Option<Workflow> {
        self.state
            .read()
            .ok()
            .and_then(|g| g.workflows.get(id).map(|e| e.workflow.clone()))
    }

    pub fn entry(&self, id: &str) -> Option<Entry> {
        self.state
            .read()
            .ok()
            .and_then(|g| g.workflows.get(id).cloned())
    }

    pub fn ids(&self) -> Vec<String> {
        self.state
            .read()
            .map(|g| g.workflows.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn all(&self) -> Vec<Entry> {
        self.state
            .read()
            .map(|g| g.workflows.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn errors(&self) -> Vec<LoadError> {
        self.state
            .read()
            .map(|g| g.errors.clone())
            .unwrap_or_default()
    }

    pub fn generation(&self) -> u64 {
        self.state.read().map(|g| g.generation).unwrap_or(0)
    }

    /// Workflows lançables sur cet OS (§2.10).
    pub fn runnable_on(&self, os: &str) -> Vec<String> {
        self.all()
            .into_iter()
            .filter(|e| e.workflow.runs_on(os))
            .map(|e| e.workflow.metadata.id)
            .collect()
    }

    /// Écrit un workflow approuvé (§12.8).
    pub fn write(&self, dir: &Path, w: &Workflow, known: &Known) -> Result<PathBuf, String> {
        let report = crate::validate::validate(w, Some(&w.metadata.id), known);
        if !report.is_valid() {
            return Err(report.render());
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.workflow.json", w.metadata.id));
        penelope_kernel::config::atomic_write(&path, w.to_json().as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use serde_json::json;

    fn known() -> Known {
        Known {
            workflow_ids: [
                "deploy-generic",
                "ticket-to-deploy",
                "build-verify",
                "review",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            model_aliases: ["main", "fast", "reasoning", "code", "summarizer"]
                .into_iter()
                .map(String::from)
                .collect(),
            native_tools: Default::default(),
            mcp_tools: Default::default(),
            templates: Default::default(),
            max_depth: 3,
        }
    }

    fn sample(id: &str) -> Workflow {
        Workflow {
            metadata: Metadata {
                id: id.into(),
                name: id.into(),
                ..Default::default()
            },
            entry_step: "un".into(),
            settings: Settings::default(),
            start_condition: json!({"type":"always"}),
            steps: vec![Step {
                id: "un".into(),
                kind: "shell".into(),
                command: json!("echo ok"),
                transitions: vec![Transition::always(DONE)],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn bundled_workflows_are_valid() {
        let r = WorkflowRegistry::with_bundled(&known());
        assert!(r.errors().is_empty(), "{:?}", r.errors());
        for id in [
            "build-verify",
            "review",
            "ticket-to-deploy",
            "deploy-generic",
        ] {
            assert!(r.get(id).is_some(), "workflow livré manquant : {id}");
        }
    }

    #[test]
    fn user_files_override_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = sample("build-verify");
        w.metadata.name = "Version maison".into();
        std::fs::write(dir.path().join("build-verify.workflow.json"), w.to_json()).unwrap();

        let r = WorkflowRegistry::with_bundled(&known());
        assert_eq!(r.load_dir(dir.path(), Scope::User, &known()), 1);
        assert_eq!(
            r.get("build-verify").unwrap().metadata.name,
            "Version maison"
        );
        assert_eq!(r.entry("build-verify").unwrap().scope, Scope::User);
    }

    /// CA 12 : un workflow invalide déposé par `scp` est rejeté avec un message précis ;
    /// l'ancienne version reste active.
    #[test]
    fn ca_12_3_invalid_file_is_rejected_and_previous_stays() {
        let dir = tempfile::tempdir().unwrap();
        let good = sample("demo");
        std::fs::write(dir.path().join("demo.workflow.json"), good.to_json()).unwrap();

        let r = WorkflowRegistry::new();
        assert_eq!(r.load_dir(dir.path(), Scope::User, &known()), 1);
        assert!(r.get("demo").is_some());
        let generation = r.generation();

        // Dépôt d'une version cassée : cible de transition inconnue.
        let mut bad = sample("demo");
        bad.steps[0].transitions = vec![Transition::always("etape_inexistante")];
        std::fs::write(dir.path().join("demo.workflow.json"), bad.to_json()).unwrap();

        assert_eq!(r.load_dir(dir.path(), Scope::User, &known()), 0);
        assert!(r.get("demo").is_some(), "l'ancienne version reste active");
        assert_eq!(
            r.get("demo").unwrap().steps[0].transitions[0].goto,
            DONE,
            "c'est bien l'ancienne définition"
        );
        assert_eq!(r.errors().len(), 1);
        let e = r.errors()[0].to_string();
        assert!(e.contains("cible inconnue"), "{e}");
        assert!(e.contains("/steps/0/transitions/0/goto"), "{e}");
        assert!(r.generation() > generation);
    }

    #[test]
    fn broken_json_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bon.workflow.json"),
            sample("bon").to_json(),
        )
        .unwrap();
        std::fs::write(dir.path().join("casse.workflow.json"), "{ pas du json").unwrap();
        let r = WorkflowRegistry::new();
        assert_eq!(r.load_dir(dir.path(), Scope::User, &known()), 1);
        assert_eq!(r.errors().len(), 1);
        assert!(r.errors()[0].to_string().contains("JSON invalide"));
    }

    #[test]
    fn id_must_match_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("autre-nom.workflow.json"),
            sample("demo").to_json(),
        )
        .unwrap();
        let r = WorkflowRegistry::new();
        assert_eq!(r.load_dir(dir.path(), Scope::User, &known()), 0);
        assert!(r.errors()[0].to_string().contains("nom du fichier"));
    }

    #[test]
    fn platform_filtering() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = sample("linux-only");
        w.metadata.platforms = vec!["linux".into()];
        std::fs::write(dir.path().join("linux-only.workflow.json"), w.to_json()).unwrap();
        let r = WorkflowRegistry::new();
        r.load_dir(dir.path(), Scope::User, &known());
        assert!(r.runnable_on("linux").contains(&"linux-only".to_string()));
        assert!(
            !r.runnable_on("macos").contains(&"linux-only".to_string()),
            "un workflow incompatible est chargé mais non lançable"
        );
        assert!(r.get("linux-only").is_some(), "il reste consultable");
    }

    #[test]
    fn write_refuses_invalid_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let r = WorkflowRegistry::new();
        let mut bad = sample("brouillon");
        bad.entry_step = "inexistante".into();
        assert!(r.write(dir.path(), &bad, &known()).is_err());
        assert!(!dir.path().join("brouillon.workflow.json").exists());

        let p = r.write(dir.path(), &sample("brouillon"), &known()).unwrap();
        assert!(p.exists());
    }
}
