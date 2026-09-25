//! Destination d'une planification : canal de retour, nom lisible, déplacement (#124).

use super::*;

/// Canal de retour déclaré : celui de la conversation qui a créé le schedule, ou celui
/// où il a été déplacé (#124). `None` : le défaut, la conversation du propriétaire.
pub(super) fn own_origin(sched: &Schedule) -> Option<Origin> {
    let o = sched.target.get("origin")?;
    let origin = Origin::from_payload(&json!({ "origin": o }));
    origin.is_channel().then_some(origin)
}

/// Canal de retour : celui de la conversation qui a créé le schedule, sinon la
/// conversation du propriétaire.
pub(super) fn target_origin(s: &Services, sched: &Schedule) -> Origin {
    if let Some(origin) = own_origin(sched) {
        return origin;
    }
    match owner_origin_of(s) {
        Origin::Internal { .. } => Origin::Internal {
            source: format!("schedule {}", sched.id),
        },
        owner => owner,
    }
}

/// Où livre une planification, en mots : « conversation privée », « sujet « Veille »,
/// groupe « Équipe » »… Le défaut est dit comme tel (issue #124).
pub async fn destination(s: &Services, sched: &Schedule) -> (Origin, String) {
    let origin = target_origin(s, sched);
    let mut name = place_name(s, &origin).await;
    if own_origin(sched).is_none() && origin.is_channel() {
        name.push_str(" (par défaut)");
    }
    (origin, name)
}

/// Nom lisible d'une conversation : c'est le canal qui sait la nommer (T36).
pub async fn place_name(s: &Services, origin: &Origin) -> String {
    s.channel.describe(origin).await
}

/// Déplace une planification vers une autre conversation, sans la recréer : son
/// historique, ses exécutions et son état restent (issue #124). Le canal dit si la
/// conversation peut la recevoir. Renvoie la nouvelle destination, en mots.
pub async fn retarget(s: &Services, id: &str, to: &Origin) -> Result<String, String> {
    let channel = s
        .channel
        .delivery
        .get()
        .ok_or("aucun canal du propriétaire n'est branché")?;
    let to = channel.destination_for(to).await?;
    let moved = s
        .schedules
        .set_origin(id, to.to_value())
        .await
        .map_err(|e| e.to_string())?;
    if !moved {
        return Err(format!("planification `{id}` introuvable"));
    }
    Ok(place_name(s, &to).await)
}

/// Planifications avec leur destination, pour `schedule list`, `/schedules` et l'outil
/// `schedule_list` (issue #124).
pub async fn listing(s: &Services) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for sched in s.schedules.list().await? {
        let (origin, name) = destination(s, &sched).await;
        let mut v = serde_json::to_value(&sched)?;
        v["destination"] = json!(name);
        v["destination_origin"] = origin.to_value();
        out.push(v);
    }
    Ok(out)
}

/// Planifications actives qui partent aujourd'hui (jour du propriétaire), à l'heure
/// locale et avec leur destination : le digest du matin les rappelle, pour qu'une
/// livraison au mauvais endroit se voie tout de suite (#124).
pub async fn due_today(s: &Services) -> Vec<String> {
    let tz = s
        .config
        .config()
        .owner
        .timezone
        .parse::<chrono_tz::Tz>()
        .ok();
    let local = |t: chrono::DateTime<chrono::Utc>| match tz {
        Some(tz) => t.with_timezone(&tz).naive_local(),
        None => t.naive_utc(),
    };
    let now = local(chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default());
    let mut rows = Vec::new();
    for sched in s.schedules.list().await.unwrap_or_default() {
        let Some(next) = sched
            .next_run
            .as_deref()
            .filter(|_| sched.state == "active")
            .and_then(|n| chrono::DateTime::parse_from_rfc3339(n).ok())
        else {
            continue;
        };
        let at = local(next.with_timezone(&chrono::Utc));
        if at.date() != now.date() {
            continue;
        }
        let (_, to) = destination(s, &sched).await;
        rows.push((
            at,
            format!("- {} {} → {to}", at.format("%H:%M"), label(s, &sched).await),
        ));
    }
    rows.sort_by_key(|(at, _)| *at);
    rows.into_iter().map(|(_, line)| line).collect()
}
