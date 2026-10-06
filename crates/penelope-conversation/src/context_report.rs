//! L'état de la fenêtre de contexte d'une session (#322) : `/context` et `penelope context`.
//!
//! ```text
//!   usage (dernier appel de conversation) ──▶ prompt réel, cache, modèle
//!        │ system_hash ──▶ prompt_snapshots ──▶ T0, T1, T2   (estimation locale)
//!        │ request_hash ──▶ journal replié (call_view) ──▶ T3, T4 (estimation locale)
//!   catalogue ──▶ fenêtre · configuration ──▶ seuils · events ──▶ compactions
//! ```
//!
//! Les chiffres du fournisseur (prompt, cache) sont donnés tels quels ; la découpe par
//! tuile est une estimation locale, dite comme telle, et l'écart avec le prompt réel (les
//! définitions d'outils, que seule leur empreinte garde) est donné par différence. Aucun
//! texte de la conversation n'en sort : des nombres seulement.

use penelope_app::services::Services;
use penelope_context::CompactionParams;
use penelope_context::tiers::TileMap;
use penelope_llm::TokenEstimator;
use penelope_store::rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Value, json};

/// Cases de la barre de remplissage.
const BAR_CELLS: u64 = 20;

/// Le dernier appel de conversation de la session, tel que le fournisseur l'a compté.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastCall {
    pub model: String,
    pub prompt: u64,
    pub cached: u64,
}

/// Une ligne de la répartition, en tokens estimés localement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TileShare {
    pub name: String,
    pub label: String,
    pub tokens: u64,
}

/// Totaux de la session, tous appels confondus (classifieur, titre, résumés compris).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    pub calls: u64,
    pub prompt: u64,
    pub completion: u64,
    pub reasoning: u64,
    pub cached: u64,
}

/// L'état de la fenêtre d'une session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextReport {
    pub session: String,
    /// `None` : aucun appel de conversation encore.
    pub last: Option<LastCall>,
    pub window: u64,
    /// Faux : modèle absent du catalogue, la fenêtre est le repli prudent.
    pub window_known: bool,
    pub background_compaction_at: u64,
    pub compaction_at: u64,
    pub compactions: u64,
    pub tiles: Vec<TileShare>,
    /// Ce que la découpe n'a pas pu relire (instantané purgé, appel antérieur au journal).
    pub reserves: Vec<String>,
    pub totals: Totals,
}

/// Une ligne d'`usage` : prompt, cache, modèle, empreintes du prompt et de la requête.
struct CallRow {
    last: LastCall,
    system_hash: Option<String>,
    request_hash: Option<String>,
}

/// Mesure la fenêtre de la session.
pub async fn report(s: &Services, session_id: &str) -> anyhow::Result<ContextReport> {
    let row = last_call(s, session_id).await?;
    let model = row
        .as_ref()
        .map(|r| r.last.model.clone())
        .unwrap_or_default();
    let cfg = s.config.config();
    let params = CompactionParams::from_config(&cfg, s.catalog.window_of(&model), &model);
    let mut reserves = Vec::new();
    let tiles = match &row {
        Some(r) => tiles(s, session_id, r, &mut reserves).await?,
        None => Vec::new(),
    };
    Ok(ContextReport {
        session: session_id.to_string(),
        window: params.window,
        window_known: !model.is_empty() && s.catalog.get(&model).is_some(),
        background_compaction_at: params.background_threshold_tokens(params.background_margin),
        compaction_at: params.threshold_tokens(),
        compactions: compactions(s, session_id).await?,
        last: row.map(|r| r.last),
        tiles,
        reserves,
        totals: totals(s, session_id).await?,
    })
}

/// La méthode `context` : la session donnée, sinon celle de la CLI ; le rapport et son
/// texte.
pub async fn rpc(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let session = match p.get("session").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => s.kv_get("cli.session").await?.ok_or_else(|| {
            anyhow::anyhow!("aucune session CLI : préciser la session, `penelope context <id>`")
        })?,
    };
    if s.sessions.get(&session).await?.is_none() {
        anyhow::bail!("session inconnue : {session}");
    }
    let r = report(s, &session).await?;
    let mut v = serde_json::to_value(&r)?;
    v["text"] = json!(render(&r, false));
    Ok(v)
}

/// Le texte de l'écran. `fenced` : la table des tuiles dans un bloc à chasse fixe, pour
/// un canal qui rend le Markdown.
pub fn render(r: &ContextReport, fenced: bool) -> String {
    let mut t = String::from("🧠 **Contexte**\n");
    let Some(last) = &r.last else {
        t.push_str("Aucun appel au modèle dans cette session : rien à mesurer encore.");
        return t;
    };
    let unknown = if r.window_known {
        ""
    } else {
        " (modèle absent du catalogue : repli prudent)"
    };
    t.push_str(&format!(
        "Modèle : `{}` · fenêtre {}{unknown}\n",
        last.model,
        group(r.window)
    ));
    t.push_str(&format!(
        "Utilisé : {} ({:.0} %)  {}\n",
        group(last.prompt),
        ratio(last.prompt, r.window) * 100.0,
        bar(last.prompt, r.window)
    ));
    let distance = match r.background_compaction_at.checked_sub(last.prompt) {
        Some(d) if d > 0 => format!("dans {}", group(d)),
        _ => "seuil atteint, elle part après le tour".into(),
    };
    t.push_str(&format!(
        "Compaction à {} ({distance}), forcée à {} · compactions : {}\n",
        group(r.background_compaction_at),
        group(r.compaction_at),
        r.compactions
    ));
    t.push_str(&format!(
        "Cache : {:.0} % du dernier appel\n",
        ratio(last.cached, last.prompt) * 100.0
    ));

    if !r.tiles.is_empty() {
        t.push_str("\nPar tuile (estimation locale)\n");
        let mut rows: Vec<(String, u64)> = r
            .tiles
            .iter()
            .map(|x| (format!("{} {}", x.name, x.label), x.tokens))
            .collect();
        rows.push(("Libre".into(), r.window.saturating_sub(last.prompt)));
        let table = table(&rows, r.window);
        if fenced {
            t.push_str(&format!("```\n{table}\n```\n"));
        } else {
            t.push_str(&format!("{table}\n"));
        }
    }
    for reserve in &r.reserves {
        t.push_str(&format!("⚠️ {reserve}\n"));
    }
    let tot = &r.totals;
    t.push_str(&format!(
        "\nSession : {} appel(s) · entrée {} · sortie {} · raisonnement {} · cache moyen {:.0} %",
        tot.calls,
        group(tot.prompt),
        group(tot.completion),
        group(tot.reasoning),
        ratio(tot.cached, tot.prompt) * 100.0
    ));
    t
}

/// Libellé, tokens, part de la fenêtre : colonnes alignées.
fn table(rows: &[(String, u64)], window: u64) -> String {
    let label_w = rows
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let numbers: Vec<String> = rows.iter().map(|(_, n)| group(*n)).collect();
    let num_w = numbers.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let shares: Vec<String> = rows
        .iter()
        .map(|(_, n)| format!("{:.1} %", ratio(*n, window) * 100.0).replace('.', ","))
        .collect();
    let share_w = shares.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    rows.iter()
        .zip(numbers.iter().zip(&shares))
        .map(|((label, _), (n, share))| format!("{label:<label_w$} {n:>num_w$} {share:>share_w$}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Milliers séparés par une espace fine insécable : `1 048 576`.
pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// `████░░░░…` : la part de la fenêtre sur vingt cases.
pub fn bar(used: u64, window: u64) -> String {
    let full = ((ratio(used, window) * BAR_CELLS as f64).round() as u64).min(BAR_CELLS);
    "█".repeat(full as usize) + &"░".repeat((BAR_CELLS - full) as usize)
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

async fn last_call(s: &Services, session_id: &str) -> anyhow::Result<Option<CallRow>> {
    let sid = session_id.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT model, prompt, cached, system_hash, request_hash FROM usage
                 WHERE session_id = ?1 AND COALESCE(role, 'chat') = 'chat'
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| {
                    Ok(CallRow {
                        last: LastCall {
                            model: r.get(0)?,
                            prompt: r.get::<_, i64>(1)?.max(0) as u64,
                            cached: r.get::<_, i64>(2)?.max(0) as u64,
                        },
                        system_hash: r.get(3)?,
                        request_hash: r.get(4)?,
                    })
                },
            )
            .optional()?)
        })
        .await?)
}

/// T0 à T2 depuis l'instantané du prompt, T3 et T4 depuis la requête repliée, l'écart au
/// prompt réel par différence.
async fn tiles(
    s: &Services,
    session_id: &str,
    row: &CallRow,
    reserves: &mut Vec<String>,
) -> anyhow::Result<Vec<TileShare>> {
    let share = |name: &str, label: &str, tokens: u64| TileShare {
        name: name.into(),
        label: label.into(),
        tokens,
    };
    let mut out = Vec::new();
    let mut whole = true;
    match &row.system_hash {
        Some(hash) => match snapshot(s, hash).await? {
            Some((rendered, Some(map))) => {
                for (name, label) in [("T0", "système"), ("T1", "capacités"), ("T2", "mémoire")]
                {
                    let text = map.slice(&rendered, name).unwrap_or_default();
                    out.push(share(name, label, TokenEstimator::raw_text_tokens(text)));
                }
            }
            Some((rendered, None)) => {
                out.push(share(
                    "T0-T2",
                    "préfixe",
                    TokenEstimator::raw_text_tokens(&rendered),
                ));
            }
            None => {
                whole = false;
                reserves.push("prompt système purgé : T0 à T2 non mesurées".into());
            }
        },
        None => {
            whole = false;
            reserves.push("dernier appel sans empreinte de prompt : T0 à T2 non mesurées".into());
        }
    }
    let view = match &row.request_hash {
        Some(hash) => s
            .context
            .history
            .call_view(session_id, hash)
            .await?
            .map_err(anyhow::Error::msg)?,
        None => None,
    };
    match view {
        Some(view) => {
            // Le contexte volatil du dernier message est la mise à jour du tour (T4) ; ceux
            // d'avant, figés avec leur message, font partie de la conversation.
            let est = TokenEstimator::new();
            let current = view.nodes.iter().rposition(|n| n.context.is_some());
            let (mut t3, mut t4) = (0, 0);
            for (i, n) in view.nodes.iter().enumerate() {
                t3 += est.message_tokens(&row.last.model, &n.message);
                if let Some(ctx) = &n.context {
                    let k = TokenEstimator::raw_text_tokens(ctx);
                    if Some(i) == current { t4 += k } else { t3 += k }
                }
            }
            out.push(share("T3", "conversation", t3));
            out.push(share("T4", "mise à jour", t4));
        }
        None => {
            whole = false;
            reserves.push("requête absente du journal : T3 et T4 non mesurées".into());
        }
    }
    let measured: u64 = out.iter().map(|t| t.tokens).sum();
    if whole && row.last.prompt > measured {
        out.push(share("+", "outils et écart", row.last.prompt - measured));
    }
    Ok(out)
}

async fn snapshot(s: &Services, hash: &str) -> anyhow::Result<Option<(String, Option<TileMap>)>> {
    let hash = hash.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT rendered, tiers FROM prompt_snapshots WHERE hash = ?1",
                [hash],
                |r| {
                    let tiers: Option<String> = r.get(1)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        tiers.and_then(|t| serde_json::from_str(&t).ok()),
                    ))
                },
            )
            .optional()?)
        })
        .await?)
}

async fn compactions(s: &Services, session_id: &str) -> anyhow::Result<u64> {
    let sid = session_id.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND kind = 'context.compacted'",
                [sid],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await?
        .max(0) as u64)
}

async fn totals(s: &Services, session_id: &str) -> anyhow::Result<Totals> {
    let sid = session_id.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt), 0), COALESCE(SUM(completion), 0),
                        COALESCE(SUM(reasoning), 0), COALESCE(SUM(cached), 0)
                 FROM usage WHERE session_id = ?1",
                [sid],
                |r| {
                    let n = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
                    Ok(Totals {
                        calls: n(0)?,
                        prompt: n(1)?,
                        completion: n(2)?,
                        reasoning: n(3)?,
                        cached: n(4)?,
                    })
                },
            )?)
        })
        .await?)
}

#[cfg(test)]
mod tests;
