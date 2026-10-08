//! Étape `foreach` (#338) : dérouler une liste ordonnée élément par élément.
//!
//! La liste vient d'un tableau écrit dans le workflow, d'un fichier JSON ou Markdown du
//! workspace, de la sortie d'une étape précédente ou d'un outil (MCP) en lecture. Elle est
//! **figée** au premier passage de l'étape dans `workflow_items` : un ajout pendant le run
//! ne la change pas. Chaque élément passe par un sous-workflow, dans l'ordre ; son état
//! (`todo`, `running`, `done`, `failed`, `skipped`) survit à un redémarrage, et la reprise
//! repart de l'élément courant.

use crate::conditions::{EvalContext, json_path};
use crate::model::{Step, StepResult};
use penelope_kernel::clock::SharedClock;
use penelope_store::{Store, rusqlite::params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Ce que fait l'échec d'un élément.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Arrêt de la liste, message au propriétaire (défaut).
    Stop,
    /// Passage au suivant, noté.
    Skip,
    /// N nouvelles tentatives, puis arrêt.
    Retry(u32),
}

impl OnError {
    pub fn parse(s: &str) -> Result<OnError, String> {
        match s.trim() {
            "" | "stop" => Ok(OnError::Stop),
            "skip" => Ok(OnError::Skip),
            other => other
                .strip_prefix("retry:")
                .and_then(|n| n.trim().parse().ok())
                .map(OnError::Retry)
                .ok_or_else(|| format!("attendu `stop`, `skip` ou `retry:N`, reçu `{other}`")),
        }
    }
}

/// D'où vient la liste.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Inline(Vec<Value>),
    File {
        path: String,
    },
    Step {
        step: String,
        path: String,
    },
    Tool {
        tool: String,
        args: Value,
        path: String,
    },
}

impl Source {
    pub fn parse(items: &Value) -> Result<Source, String> {
        let text = |k: &str| items.get(k).and_then(Value::as_str).map(String::from);
        let path = text("path").unwrap_or_default();
        if let Some(a) = items.as_array() {
            return Ok(Source::Inline(a.clone()));
        }
        if let Some(p) = text("file") {
            return Ok(Source::File { path: p });
        }
        if let Some(step) = text("step") {
            return Ok(Source::Step { step, path });
        }
        if let Some(tool) = text("tool") {
            let args = items.get("args").cloned().unwrap_or_else(|| json!({}));
            return Ok(Source::Tool { tool, args, path });
        }
        Err("attendu un tableau, `file`, `step` ou `tool`".into())
    }
}

/// Les éléments d'une valeur : un tableau, au chemin donné ou trouvé seul. Une chaîne qui
/// contient du JSON est lue (le texte d'un outil MCP, le `content` d'une étape) ; un
/// résultat MCP est cherché dans `structuredContent`, puis dans son texte.
pub fn extract(value: &Value, path: &str) -> Result<Vec<Value>, String> {
    let found = if path.is_empty() {
        Some(value.clone())
    } else {
        json_path(value, path)
    };
    let Some(found) = found else {
        return Err(format!("rien au chemin `{path}`"));
    };
    match as_list(&found) {
        Some(items) => Ok(items),
        None if path.is_empty() => ["structuredContent", "content", "text", "data"]
            .iter()
            .find_map(|k| found.get(*k).and_then(as_list))
            .ok_or_else(|| "aucune liste dans la valeur".to_string()),
        None => Err(format!("`{path}` n'est pas une liste")),
    }
}

fn as_list(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s.trim())
            .ok()
            .filter(|p| !p.is_string())
            .and_then(|p| as_list(&p)),
        // Un objet qui ne porte qu'une liste (`{"tasks": [...]}`) la donne.
        Value::Object(m) => {
            let lists: Vec<&Value> = m.values().filter(|x| x.is_array()).collect();
            match lists.as_slice() {
                [only] => as_list(only),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Les éléments d'un fichier : JSON (tableau, ou objet qui en porte un) ou Markdown, une
/// entrée par ligne de liste (`- `, `* `, `1. `, cases `[ ]` et `[x]` comprises). Une case
/// cochée est un élément déjà fait : elle est sautée.
pub fn parse_file(name: &str, text: &str, path: &str) -> Result<Vec<Value>, String> {
    if name.ends_with(".json") {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("{name} : {e}"))?;
        return extract(&v, path);
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_start();
        let rest = l
            .strip_prefix("- ")
            .or_else(|| l.strip_prefix("* "))
            .or_else(|| {
                let digits = l.chars().take_while(char::is_ascii_digit).count();
                (digits > 0)
                    .then(|| l[digits..].strip_prefix(". "))
                    .flatten()
            });
        let Some(rest) = rest.map(str::trim) else {
            continue;
        };
        if rest.starts_with("[x]") || rest.starts_with("[X]") {
            continue;
        }
        let title = rest.strip_prefix("[ ]").unwrap_or(rest).trim();
        if !title.is_empty() {
            out.push(json!({"title": title}));
        }
    }
    Ok(out)
}

/// Filtre (`{"status": "à faire"}` ou `{"status": ["à faire", "en cours"]}`) puis tri
/// stable sur un champ (`sortBy`, préfixe `-` pour l'ordre inverse), comme écrits dans
/// `items`.
pub fn refine(mut items: Vec<Value>, spec: &Value) -> Vec<Value> {
    if let Some(filter) = spec.get("filter").and_then(Value::as_object) {
        items.retain(|item| {
            filter.iter().all(|(field, wanted)| {
                let actual = json_path(item, field).unwrap_or(Value::Null);
                match wanted {
                    Value::Array(any) => any.iter().any(|w| same(w, &actual)),
                    one => same(one, &actual),
                }
            })
        });
    }
    if let Some(key) = spec.get("sortBy").and_then(Value::as_str) {
        let (key, desc) = match key.strip_prefix('-') {
            Some(k) => (k, true),
            None => (key, false),
        };
        items.sort_by(|a, b| {
            let (x, y) = (json_path(a, key), json_path(b, key));
            let o = match (
                x.as_ref().and_then(Value::as_f64),
                y.as_ref().and_then(Value::as_f64),
            ) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                _ => text(x.as_ref()).cmp(&text(y.as_ref())),
            };
            if desc { o.reverse() } else { o }
        });
    }
    items
}

fn same(a: &Value, b: &Value) -> bool {
    a == b || text(Some(a)) == text(Some(b))
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Libellé par défaut d'un élément : `title`, `name`, `label` ou `id`, sinon le texte.
pub fn default_label(item: &Value, idx: usize) -> String {
    let label = ["title", "name", "label", "id"]
        .iter()
        .find_map(|k| item.get(*k).map(|v| text(Some(v))))
        .filter(|l| !l.is_empty())
        .or_else(|| item.as_str().map(String::from))
        .unwrap_or_else(|| format!("#{}", idx + 1));
    label.chars().take(120).collect()
}

/// L'élément qui vient de finir appelle-t-il un point d'arrêt (`pauseAfter`) ? Un
/// `{"changes": "epic"}` s'arrête quand le suivant change de valeur (fin d'épique) ; une
/// condition se lit sur `{item, next}`.
pub fn pause_after(spec: &Value, item: &Value, next: Option<&Value>) -> bool {
    if spec.is_null() {
        return false;
    }
    if let Some(field) = spec.get("changes").and_then(Value::as_str) {
        return next.is_some_and(|n| json_path(item, field) != json_path(n, field));
    }
    let output = json!({"item": item, "next": next});
    crate::conditions::evaluate(
        spec,
        &EvalContext {
            step_result: &StepResult::Completed,
            step_output: &output,
            metadata: &Value::Null,
        },
    )
}

/// Vérifie une étape `foreach` : liste, sous-workflow, `onError`, points d'arrêt.
pub fn check(s: &Step) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    match Source::parse(&s.items) {
        Ok(Source::Inline(items)) if items.is_empty() => {
            errors.push(("items".into(), "liste vide".into()));
        }
        Ok(_) => {}
        Err(e) => errors.push(("items".into(), e)),
    }
    if let Err(e) = OnError::parse(&s.on_error) {
        errors.push(("onError".into(), e));
    }
    if s.pause_every == Some(0) {
        errors.push(("pauseEvery".into(), "attendu un entier positif".into()));
    }
    if !s.pause_after.is_null()
        && s.pause_after.get("changes").is_none()
        && s.pause_after.get("type").is_none()
    {
        errors.push((
            "pauseAfter".into(),
            "attendu `{\"changes\": \"<champ>\"}` ou une condition".into(),
        ));
    }
    errors
}

/// État d'un élément.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Todo,
    Running,
    Done,
    Failed,
    Skipped,
}

impl ItemState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemState::Todo => "todo",
            ItemState::Running => "running",
            ItemState::Done => "done",
            ItemState::Failed => "failed",
            ItemState::Skipped => "skipped",
        }
    }
    fn parse(s: &str) -> ItemState {
        match s {
            "running" => ItemState::Running,
            "done" => ItemState::Done,
            "failed" => ItemState::Failed,
            "skipped" => ItemState::Skipped,
            _ => ItemState::Todo,
        }
    }
}

/// Un élément de la liste figée.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub run_id: String,
    pub step_id: String,
    pub visit: u32,
    pub idx: usize,
    pub item: Value,
    pub label: String,
    pub state: ItemState,
    pub child_run: Option<String>,
    pub attempts: u32,
    pub error: Option<String>,
}

/// Bilan d'une liste : faits, en échec, sautés, total, élément courant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tally {
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub total: usize,
}

impl Tally {
    pub fn of(items: &[Item]) -> Tally {
        let count = |st: ItemState| items.iter().filter(|i| i.state == st).count();
        Tally {
            done: count(ItemState::Done),
            failed: count(ItemState::Failed),
            skipped: count(ItemState::Skipped),
            total: items.len(),
        }
    }
    /// `5 faits, 1 en échec, 0 sauté sur 53`.
    pub fn line(&self) -> String {
        format!(
            "{} fait{}, {} en échec, {} sauté{} sur {}",
            self.done,
            if self.done > 1 { "s" } else { "" },
            self.failed,
            self.skipped,
            if self.skipped > 1 { "s" } else { "" },
            self.total
        )
    }
}

/// Les listes figées des runs (`workflow_items`).
#[derive(Clone)]
pub struct ItemStore {
    store: Store,
    clock: SharedClock,
}

const COLUMNS: &str = "run_id, step_id, visit, idx, item, label, state, child_run, attempts, error";

fn row_to_item(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<Item> {
    let item: String = r.get(4)?;
    let state: String = r.get(6)?;
    Ok(Item {
        run_id: r.get(0)?,
        step_id: r.get(1)?,
        visit: r.get::<_, i64>(2)? as u32,
        idx: r.get::<_, i64>(3)? as usize,
        item: serde_json::from_str(&item).unwrap_or(Value::Null),
        label: r.get(5)?,
        state: ItemState::parse(&state),
        child_run: r.get(7)?,
        attempts: r.get::<_, i64>(8)? as u32,
        error: r.get(9)?,
    })
}

impl ItemStore {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        ItemStore { store, clock }
    }

    /// Fige la liste d'une visite, une fois : rend `false` si elle l'était déjà.
    pub async fn freeze(
        &self,
        run_id: &str,
        step_id: &str,
        visit: u32,
        items: Vec<(Value, String)>,
    ) -> penelope_store::Result<bool> {
        let (run, step) = (run_id.to_string(), step_id.to_string());
        self.store
            .write(move |tx| {
                let known: i64 = tx.query_row(
                    "SELECT count(*) FROM workflow_items
                     WHERE run_id = ?1 AND step_id = ?2 AND visit = ?3",
                    params![run, step, visit as i64],
                    |r| r.get(0),
                )?;
                if known > 0 {
                    return Ok(false);
                }
                for (idx, (item, label)) in items.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO workflow_items(run_id, step_id, visit, idx, item, label)
                         VALUES(?1,?2,?3,?4,?5,?6)",
                        params![run, step, visit as i64, idx as i64, item.to_string(), label],
                    )?;
                }
                Ok(true)
            })
            .await
    }

    /// La liste d'une visite, dans l'ordre.
    pub async fn list(
        &self,
        run_id: &str,
        step_id: &str,
        visit: u32,
    ) -> penelope_store::Result<Vec<Item>> {
        let (run, step) = (run_id.to_string(), step_id.to_string());
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM workflow_items
                     WHERE run_id = ?1 AND step_id = ?2 AND visit = ?3 ORDER BY idx"
                ))?;
                let rows = st.query_map(params![run, step, visit as i64], row_to_item)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            })
            .await
    }

    /// La dernière liste d'un run, quelle que soit l'étape : celle que montre le suivi.
    pub async fn latest(&self, run_id: &str) -> penelope_store::Result<Vec<Item>> {
        let run = run_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM workflow_items w
                     WHERE run_id = ?1 AND visit = (SELECT MAX(visit) FROM workflow_items
                                                    WHERE run_id = ?1)
                     ORDER BY idx"
                ))?;
                let rows = st.query_map([run], row_to_item)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            })
            .await
    }

    /// L'élément que traite un sous-run, s'il en traite un.
    pub async fn of_child(&self, child_run: &str) -> penelope_store::Result<Option<Item>> {
        let child = child_run.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM workflow_items WHERE child_run = ?1 LIMIT 1"
                ))?;
                let mut rows = st.query([child])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_item(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    /// Un élément démarre (ou redémarre, tentative suivante) sous ce sous-run.
    pub async fn start(&self, it: &Item, child_run: &str) -> penelope_store::Result<()> {
        let now = self.clock.now_rfc3339();
        let child = child_run.to_string();
        let attempts = if it.child_run.is_some() {
            it.attempts + 1
        } else {
            it.attempts
        };
        self.update(it, move |tx, (run, step, visit, idx)| {
            tx.execute(
                "UPDATE workflow_items SET state = 'running', child_run = ?5, attempts = ?6,
                    started_at = COALESCE(started_at, ?7), error = NULL
                 WHERE run_id = ?1 AND step_id = ?2 AND visit = ?3 AND idx = ?4",
                params![run, step, visit, idx, child, attempts as i64, now],
            )
        })
        .await
    }

    /// Un élément finit : fait, en échec ou sauté, avec sa raison.
    pub async fn finish(
        &self,
        it: &Item,
        state: ItemState,
        error: Option<&str>,
    ) -> penelope_store::Result<()> {
        let now = self.clock.now_rfc3339();
        let (st, err) = (state.as_str(), error.map(String::from));
        self.update(it, move |tx, (run, step, visit, idx)| {
            tx.execute(
                "UPDATE workflow_items SET state = ?5, error = ?6, ended_at = ?7
                 WHERE run_id = ?1 AND step_id = ?2 AND visit = ?3 AND idx = ?4",
                params![run, step, visit, idx, st, err, now],
            )
        })
        .await
    }

    async fn update<F>(&self, it: &Item, f: F) -> penelope_store::Result<()>
    where
        F: FnOnce(
                &penelope_store::rusqlite::Transaction<'_>,
                (String, String, i64, i64),
            ) -> penelope_store::rusqlite::Result<usize>
            + Send
            + 'static,
    {
        let key = (
            it.run_id.clone(),
            it.step_id.clone(),
            it.visit as i64,
            it.idx as i64,
        );
        self.store
            .write(move |tx| {
                f(tx, key)?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
