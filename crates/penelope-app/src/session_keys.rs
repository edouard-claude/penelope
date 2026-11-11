//! Les clés de travail d'une session, effacées à sa fermeture (#459).
//!
//! Une session pose dans `kv` de quoi travailler d'un tour à l'autre. Rien ne l'effaçait
//! à la fermeture : le 11/11, les listes d'outils gelées de sessions finies pesaient
//! 6,7 Mo sur une instance. Les clés sont posées par quatre crates ; leurs préfixes sont
//! relevés ici, et un test de l'ordonnanceur vérifie qu'ils nomment les clés réelles.

use crate::services::Services;

/// Ce qu'une session fermée ne relit pas : la liste d'outils gelée de son préfixe
/// (`frozen_tools`), recalculée au premier tour si la session est rouverte. C'est la plus
/// lourde, 15 Ko par session en moyenne.
const CLOSED: &[&str] = &["session.tools.frozen."];

/// Ce qu'une session qui ne reprendra pas (un tir de planification ou un run fini) laisse
/// en plus : ses outils à la demande, l'état gardé avant un tour qui n'a jamais rendu son
/// issue, son repère d'activité. Le lien vers sa planification (`session.schedule.`)
/// reste : il dit d'où vient la session, et quelle portée ont ses règles si le
/// propriétaire la rouvre (#379).
const FINISHED: &[&str] = &["session.tools.", "schedule.state.", "activity."];

/// Efface les clés de travail d'une session qui vient d'être fermée. `finished` : la
/// session ne reprendra pas. Rend le nombre de clés effacées ; un échec ne coûte que la
/// place.
pub async fn closed(s: &Services, session_id: &str, finished: bool) -> usize {
    let keys: Vec<String> = CLOSED
        .iter()
        .chain(FINISHED.iter().filter(|_| finished))
        .map(|prefix| format!("{prefix}{session_id}"))
        .collect();
    let done = s
        .store
        .write(move |tx| {
            let mut n = 0;
            for k in &keys {
                n += tx.execute("DELETE FROM kv WHERE k = ?1", [k])?;
            }
            Ok(n)
        })
        .await;
    match done {
        Ok(n) => n,
        Err(error) => {
            tracing::warn!(%error, session = %session_id, "clés de travail non effacées");
            0
        }
    }
}
