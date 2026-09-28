//! Jeu de décisions du juge d'approbation (issue #233) : un échantillon par ligne
//! `shell_exec` vue par la politique, carte ou pas, pour évaluer plus tard un autre juge
//! sur les lignes réellement vues par l'instance.
//!
//! ```text
//!  décision de la boucle ─► record      (entrée, planchers, jugement ; issue si immédiate)
//!  carte tranchée/expirée ─► settle_in  (issue, via, délai) — dans la transaction de la carte
//!  exécution ─────────────► executed    (code de sortie, durée)
//! ```
//!
//! Chaque étape ne remplit que des colonnes encore vides : un échantillon est complété,
//! jamais réécrit. Rien ne s'écrit quand `observability.dataset.approvals` est faux ; la
//! boucle le vérifie avant d'appeler [`ApprovalSamples::record`]. Jamais dans le flux
//! runtime, jamais dans `approval.judged`, jamais dans une carte : la table est locale, sa
//! rétention est la sienne, et la purge d'une session la vide (`penelope-ops`).

use penelope_kernel::clock::SharedClock;
use penelope_store::Store;
use penelope_store::rusqlite::{Connection, Transaction, params};
use serde_json::{Value, json};

/// Version du schéma d'un échantillon, portée par chaque ligne exportée.
pub const SAMPLE_VERSION: i64 = 1;

/// Ce que la boucle sait d'un appel au moment de sa décision.
#[derive(Debug, Clone)]
pub struct SampleDraft {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub call_id: String,
    /// Seize caractères du SHA-256 de la ligne, comme `approval.judged`.
    pub command_sha: String,
    /// `{command, cwd, workspaces, network}`, la ligne telle que le juge la reçoit.
    pub input: Value,
    /// `{policy, layer, risk, rule, sans_motif}`.
    pub floors: Value,
    /// Sortie du juge ou son échec ; `None` : il n'a pas été appelé.
    pub judge: Option<Value>,
    /// Carte posée : l'issue viendra de sa décision.
    pub approval_id: Option<String>,
    /// Issue immédiate (`auto`, `denied`) et ce qui l'a donnée.
    pub outcome: Option<(String, String)>,
}

/// La table `approval_samples`.
#[derive(Clone)]
pub struct ApprovalSamples {
    store: Store,
    clock: SharedClock,
}

impl ApprovalSamples {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        ApprovalSamples { store, clock }
    }

    /// Écrit l'échantillon d'un appel. `false` : il existait déjà (même session, même
    /// appel), rien n'est réécrit.
    pub async fn record(&self, d: SampleDraft) -> crate::Result<bool> {
        let (now, now_ms) = (self.clock.now_rfc3339(), self.clock.now_ms());
        let n = self
            .store
            .write(move |tx| {
                let (outcome, via) = d.outcome.unzip();
                let decided = outcome.as_ref().map(|_| now.clone());
                Ok(tx.execute(
                    "INSERT OR IGNORE INTO approval_samples(v, created_at, created_ms,
                        session_id, turn_id, call_id, command_sha, input, floors, judge,
                        approval_id, outcome, via, decided_at, decision_ms)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        SAMPLE_VERSION,
                        now,
                        now_ms,
                        d.session_id,
                        d.turn_id,
                        d.call_id,
                        d.command_sha,
                        d.input.to_string(),
                        d.floors.to_string(),
                        d.judge.map(|j| j.to_string()),
                        d.approval_id,
                        outcome,
                        via,
                        decided,
                        decided.as_ref().map(|_| 0i64),
                    ],
                )?)
            })
            .await?;
        Ok(n > 0)
    }

    /// L'exécution d'un appel échantillonné : code de sortie et durée, une fois.
    pub async fn executed(
        &self,
        session_id: &str,
        call_id: &str,
        exit_code: Option<i64>,
        duration_ms: Option<i64>,
    ) -> crate::Result<()> {
        let (sid, cid, now) = (
            session_id.to_string(),
            call_id.to_string(),
            self.clock.now_rfc3339(),
        );
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE approval_samples SET exit_code = ?3, exec_ms = ?4, executed_at = ?5
                     WHERE session_id = ?1 AND call_id = ?2 AND executed_at IS NULL",
                    params![sid, cid, exit_code, duration_ms, now],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

/// L'issue d'une carte, dans la transaction qui la tranche : `approved`, `denied`,
/// `expired` ou `cancelled`, par quel canal, et le délai depuis l'échantillon. Sans
/// échantillon (collecte désactivée), ne touche rien.
pub(crate) fn settle_in(
    tx: &Transaction<'_>,
    approval_id: &str,
    outcome: &str,
    via: Option<&str>,
    now: &str,
    now_ms: i64,
) -> penelope_store::rusqlite::Result<usize> {
    tx.execute(
        "UPDATE approval_samples
         SET outcome = ?2, via = ?3, decided_at = ?4, decision_ms = max(0, ?5 - created_ms)
         WHERE approval_id = ?1 AND outcome IS NULL",
        params![approval_id, outcome, via, now, now_ms],
    )
}

/// Les échantillons créés depuis `since` (RFC 3339, `None` : tous), dans l'ordre, une
/// valeur JSON par ligne. Lecture seule : la connexion peut l'être.
pub fn export(
    conn: &Connection,
    since: Option<&str>,
) -> penelope_store::rusqlite::Result<Vec<Value>> {
    let mut st = conn.prepare(
        "SELECT v, created_at, session_id, turn_id, call_id, command_sha, input, floors,
                judge, approval_id, outcome, via, decided_at, decision_ms, exit_code,
                exec_ms, executed_at
         FROM approval_samples WHERE created_at >= ?1 ORDER BY created_at, id",
    )?;
    let parse = |s: Option<String>| -> Value {
        s.and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    };
    let mut rows = st.query([since.unwrap_or("")])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let line = json!({
            "v": r.get::<_, i64>(0)?,
            "created_at": r.get::<_, String>(1)?,
            "session_id": r.get::<_, String>(2)?,
            "turn_id": r.get::<_, Option<String>>(3)?,
            "call_id": r.get::<_, String>(4)?,
            "command_sha": r.get::<_, String>(5)?,
            "input": parse(r.get(6)?),
            "floors": parse(r.get(7)?),
            "judge": parse(r.get(8)?),
            "approval_id": r.get::<_, Option<String>>(9)?,
            "outcome": r.get::<_, Option<String>>(10)?,
            "via": r.get::<_, Option<String>>(11)?,
            "decided_at": r.get::<_, Option<String>>(12)?,
            "decision_ms": r.get::<_, Option<i64>>(13)?,
            "exit_code": r.get::<_, Option<i64>>(14)?,
            "exec_ms": r.get::<_, Option<i64>>(15)?,
            "executed_at": r.get::<_, Option<String>>(16)?,
        });
        // Les règles de masquage évoluent, les lignes écrites non (#148) : l'export repasse
        // le rédacteur du jour. Idempotent sur ce qui est déjà masqué.
        out.push(penelope_observe::redact_json(&line));
    }
    Ok(out)
}

/// Ce que `doctor` dit de la collecte : volume, plus ancien échantillon, issues.
#[derive(Debug, Default, PartialEq)]
pub struct SampleStats {
    pub total: i64,
    pub oldest: Option<String>,
    /// `(issue, nombre)`, l'issue `en_attente` pour une carte pas encore tranchée.
    pub outcomes: Vec<(String, i64)>,
}

pub fn stats(conn: &Connection) -> penelope_store::rusqlite::Result<SampleStats> {
    let (total, oldest) = conn.query_row(
        "SELECT count(*), min(created_at) FROM approval_samples",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut st = conn.prepare(
        "SELECT coalesce(outcome, 'en_attente'), count(*) FROM approval_samples
         GROUP BY 1 ORDER BY 2 DESC, 1",
    )?;
    let outcomes = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SampleStats {
        total,
        oldest,
        outcomes,
    })
}

#[cfg(test)]
mod tests;
