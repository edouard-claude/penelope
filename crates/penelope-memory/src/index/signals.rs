use super::*;

impl MemoryIndex {
    pub async fn signals_of(&self, uid: &str) -> penelope_store::Result<Signals> {
        let uid = uid.to_string();
        self.store.read(move |c| signals_row(c, &uid)).await
    }

    /// Enregistre un rappel, avec la requête qui l'a déclenché (diversité des requêtes).
    pub async fn record_recall(
        &self,
        uid: &str,
        query: &str,
        useful: bool,
    ) -> penelope_store::Result<()> {
        self.bump_recall(uid, query, 1, useful as i64).await
    }

    /// Un souvenir servi au modèle (issue #105) : sa date de rappel et la requête sont
    /// notées ; `counted`, il compte parmi les rappels et son utilité sera jugée sur la
    /// réponse ([`MemoryIndex::mark_useful`]). Servi hors conversation, où rien ne juge,
    /// il ne pèse pas sur la part des rappels utiles.
    pub async fn record_served(
        &self,
        uid: &str,
        query: &str,
        counted: bool,
    ) -> penelope_store::Result<()> {
        self.bump_recall(uid, query, counted as i64, 0).await
    }

    /// Le souvenir servi a servi : la réponse s'en est servie (issue #105). Jamais plus de
    /// rappels utiles que de rappels.
    pub async fn mark_useful(&self, uid: &str) -> penelope_store::Result<()> {
        let uid = uid.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "UPDATE mem_signals SET useful_recalls = MIN(recalls, useful_recalls + 1)
                     WHERE uid = ?1",
                    [&uid],
                )?;
                Ok(())
            })
            .await
    }

    async fn bump_recall(
        &self,
        uid: &str,
        query: &str,
        recalls: i64,
        useful: i64,
    ) -> penelope_store::Result<()> {
        let (uid, q, now) = (uid.to_string(), query.to_string(), self.clock.now_rfc3339());
        self.store
            .write(move |tx| {
                let existing: String = tx
                    .query_row(
                        "SELECT distinct_queries FROM mem_signals WHERE uid = ?1",
                        [&uid],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| "[]".into());
                let mut queries: Vec<String> = serde_json::from_str(&existing).unwrap_or_default();
                let norm = q.to_lowercase();
                if !queries.iter().any(|x| x == &norm) {
                    queries.push(norm);
                    if queries.len() > 50 {
                        queries.remove(0);
                    }
                }
                tx.execute(
                    "INSERT INTO mem_signals(uid, recalls, useful_recalls, last_recall,
                        distinct_queries)
                     VALUES(?1, ?5, ?2, ?3, ?4)
                     ON CONFLICT(uid) DO UPDATE SET
                        recalls = recalls + ?5,
                        useful_recalls = useful_recalls + ?2,
                        last_recall = ?3,
                        distinct_queries = ?4",
                    params![
                        uid,
                        useful,
                        now,
                        serde_json::to_string(&queries).unwrap_or_default(),
                        recalls
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Compte les entrées apparues dans les résultats du rappel automatique sans être
    /// retenues (#86) : le retour d'usage ne propose au retrait que ce qui a eu sa chance.
    pub async fn record_seen(&self, uids: &[String]) -> penelope_store::Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let uids = uids.to_vec();
        self.store
            .write(move |tx| {
                for uid in &uids {
                    tx.execute(
                        "INSERT INTO mem_signals(uid, seen) VALUES(?1, 1)
                         ON CONFLICT(uid) DO UPDATE SET seen = seen + 1",
                        [uid],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Enregistre un succès ou une contradiction (calcul de confiance, §6.8).
    pub async fn record_outcome(&self, uid: &str, success: bool) -> penelope_store::Result<()> {
        let uid = uid.to_string();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mem_signals(uid, successes, contradictions)
                     VALUES(?1, ?2, ?3)
                     ON CONFLICT(uid) DO UPDATE SET
                        successes = successes + ?2, contradictions = contradictions + ?3",
                    params![uid, success as i64, (!success) as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Marqueurs d'une entrée (issue #25).
    pub async fn set_flags(
        &self,
        uid: &str,
        sensible: bool,
        expire: Option<&str>,
    ) -> penelope_store::Result<()> {
        let (uid, expire) = (uid.to_string(), expire.map(String::from));
        self.store
            .write(move |tx| {
                if !sensible && expire.is_none() {
                    tx.execute("DELETE FROM mem_flags WHERE uid = ?1", [&uid])?;
                } else {
                    tx.execute(
                        "INSERT INTO mem_flags(uid, sensible, expire) VALUES(?1, ?2, ?3)
                         ON CONFLICT(uid) DO UPDATE SET sensible = excluded.sensible,
                            expire = excluded.expire",
                        params![uid, sensible as i64, expire],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// Entrées à ne pas injecter d'office : expirées à ce jour. `sensible` n'est plus qu'un
    /// marqueur : le vault est privé, une information client ou d'infrastructure utile se
    /// garde et se sert (issue #37).
    pub async fn hidden_uids(&self) -> penelope_store::Result<std::collections::HashSet<String>> {
        let today: String = self.clock.now_rfc3339().chars().take(10).collect();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT uid FROM mem_flags
                     WHERE expire IS NOT NULL AND expire < ?1",
                )?;
                let rows = st.query_map([today], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await
    }
}
