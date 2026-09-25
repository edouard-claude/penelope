//! Arguments des outils, rendu des listings et étiquettes de session.

use super::*;

pub(super) fn new_workflow_plan(
    id: &str,
    args: &Value,
) -> ToolResult<penelope_workflow::plan::PlanDraft> {
    use penelope_workflow::plan::{Plan, PlanDraft, PlanStep};
    let goal = str_arg(args, "goal")?;
    let steps: Vec<PlanStep> = serde_json::from_value(
        args.get("steps")
            .cloned()
            .ok_or_else(|| ToolError::Invalid("steps requis".into()))?,
    )
    .map_err(|e| ToolError::Invalid(e.to_string()))?;
    let plan = Plan::new(goal, steps).map_err(|e| ToolError::Invalid(e.to_string()))?;
    Ok(PlanDraft {
        workflow_id: id.into(),
        params: args.get("params").cloned().unwrap_or(json!({})),
        brief: args.get("brief").and_then(Value::as_str).map(String::from),
        plan,
    })
}

pub(super) fn str_arg(args: &Value, key: &str) -> ToolResult<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ToolError::Invalid(format!("`{key}` manquant")))
}

pub(super) fn u_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

pub(super) fn b_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Au-delà, la liste d'un `fs_list` part en artefact avec un résumé (~4 k tokens).
pub(super) const FS_LIST_INLINE_CHARS: usize = 16_000;

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} Gio", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} Mio", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} Kio", b as f64 / (1u64 << 10) as f64),
        b => format!("{b} o"),
    }
}

/// `fs_list` en lignes compactes, chemins relatifs à la racine.
pub(super) fn render_listing(root: &Path, v: &Value) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let mut out = format!("{} ({} entrées)\n", root.display(), items.len());
    for it in &items {
        let path = it["path"].as_str().unwrap_or_default();
        let rel = Path::new(path)
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.to_string());
        if it["dir"].as_bool().unwrap_or(false) {
            out.push_str(&format!("{rel}/\n"));
        } else {
            let size = size_label(it["bytes"].as_u64().unwrap_or(0));
            out.push_str(&format!("{rel}  {size}\n"));
        }
    }
    out
}

/// Résumé d'une longue liste : décompte par dossier de premier niveau et aperçu.
pub(super) fn summarise_listing(
    root: &Path,
    v: &Value,
    max: usize,
    listing: &str,
    artifact: &str,
) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let dirs = items
        .iter()
        .filter(|i| i["dir"].as_bool().unwrap_or(false))
        .count();
    let mut per_top: std::collections::BTreeMap<String, usize> = Default::default();
    for it in &items {
        let path = Path::new(it["path"].as_str().unwrap_or_default());
        if let Ok(rel) = path.strip_prefix(root)
            && let Some(first) = rel.components().next()
        {
            let is_leaf_file =
                rel.components().count() == 1 && !it["dir"].as_bool().unwrap_or(false);
            let key = if is_leaf_file {
                "(racine)".to_string()
            } else {
                format!("{}/", first.as_os_str().to_string_lossy())
            };
            *per_top.entry(key).or_default() += 1;
        }
    }
    let mut top: Vec<(String, usize)> = per_top.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = format!(
        "{} : {} entrées ({dirs} dossiers, {} fichiers){}.\nPar dossier de premier niveau :\n",
        root.display(),
        items.len(),
        items.len() - dirs,
        if items.len() >= max {
            format!(", liste arrêtée à max_entries = {max}")
        } else {
            String::new()
        }
    );
    for (name, n) in top.iter().take(30) {
        out.push_str(&format!("- {name} {n}\n"));
    }
    if top.len() > 30 {
        out.push_str(&format!("- … {} autres\n", top.len() - 30));
    }
    out.push_str("Aperçu :\n");
    for line in listing.lines().skip(1).take(40) {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!(
        "Liste complète : artifact_read(\"{artifact}\"). Pour moins de bruit, lister un \
         sous-dossier ou passer par fs_search."
    ));
    out
}

/// Ajoute à chaque extrait le titre et la date de sa session.
pub(super) async fn with_session_labels(s: &Services, hits: &mut Value) {
    let Some(items) = hits.as_array_mut() else {
        return;
    };
    let mut known: std::collections::BTreeMap<String, (String, String)> = Default::default();
    for h in items.iter_mut() {
        let Some(sid) = h["session_id"].as_str().map(String::from) else {
            continue;
        };
        if !known.contains_key(&sid) {
            let label = match s.sessions.get(&sid).await {
                Ok(Some(sess)) => (
                    sess.title
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| "(sans titre)".into()),
                    sess.last_activity
                        .unwrap_or(sess.created_at)
                        .chars()
                        .take(10)
                        .collect(),
                ),
                _ => ("(session inconnue)".into(), String::new()),
            };
            known.insert(sid.clone(), label);
        }
        if let Some((title, date)) = known.get(&sid) {
            h["session_titre"] = json!(title);
            h["session_date"] = json!(date);
        }
    }
}
