//! Destination d'une planification : canal de retour, nom lisible, déplacement (#124).

use super::*;

/// Canal de retour déclaré : celui de la conversation qui a créé le schedule, ou celui
/// où il a été déplacé (#124). `None` : le défaut, la conversation du propriétaire.
pub(super) fn own_origin(sched: &Schedule) -> Option<Origin> {
    let o = sched.target.get("origin")?;
    let origin = Origin::from_payload(&json!({ "origin": o }));
    (!matches!(origin, Origin::Internal { .. } | Origin::Cli)).then_some(origin)
}

/// Canal de retour : celui de la conversation qui a créé le schedule, sinon le chat
/// Telegram du propriétaire.
pub(super) fn target_origin(s: &Services, sched: &Schedule) -> Origin {
    if let Some(origin) = own_origin(sched) {
        return origin;
    }
    match owner_origin_of(s) {
        Origin::Internal { .. } => Origin::Internal {
            source: format!("schedule {}", sched.id),
        },
        telegram => telegram,
    }
}

/// Où livre une planification, en mots : « conversation privée », « sujet « Veille »,
/// groupe « Équipe » »… Le défaut est dit comme tel (issue #124).
pub async fn destination(s: &Services, sched: &Schedule) -> (Origin, String) {
    let origin = target_origin(s, sched);
    let mut name = place_name(s, &origin).await;
    if own_origin(sched).is_none() && matches!(origin, Origin::Telegram { .. }) {
        name.push_str(" (par défaut)");
    }
    (origin, name)
}

/// Nom lisible d'une conversation Telegram : titre du groupe et nom du sujet quand
/// Pénélope les a vus passer, identifiants sinon.
pub async fn place_name(s: &Services, origin: &Origin) -> String {
    let Origin::Telegram {
        chat_id, topic_id, ..
    } = origin
    else {
        return "aucune conversation (Telegram non configuré)".into();
    };
    let kv = |k: String| async move {
        s.store
            .read(move |c| penelope_store::kv_get(c, &k))
            .await
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
    };
    let owner = s.config.config().owner.telegram_user_id;
    let chat = if *chat_id == owner {
        "conversation privée".to_string()
    } else if *chat_id > 0 {
        format!("conversation {chat_id}")
    } else {
        match kv(crate::helpers::chat_title_key(*chat_id)).await {
            Some(title) => format!("groupe « {title} »"),
            None => format!("groupe {chat_id}"),
        }
    };
    match topic_id {
        None => chat,
        Some(t) => match kv(crate::helpers::topic_name_key(*chat_id, *t)).await {
            Some(name) => format!("sujet « {name} », {chat}"),
            None => format!("sujet {t}, {chat}"),
        },
    }
}

/// Déplace une planification vers une autre conversation, sans la recréer : son
/// historique, ses exécutions et son état restent (issue #124). La conversation doit
/// être celle du propriétaire ou une conversation autorisée. Renvoie la nouvelle
/// destination, en mots.
pub async fn retarget(
    s: &Services,
    id: &str,
    chat_id: i64,
    topic_id: Option<i64>,
) -> Result<String, String> {
    let cfg = s.config.config();
    let owner = cfg.owner.telegram_user_id;
    if chat_id == 0 {
        return Err("Telegram n'est pas configuré (`owner.telegram_user_id`)".into());
    }
    if !(chat_id == owner || cfg.telegram.allowed_chats.contains(&chat_id)) {
        return Err(format!(
            "la conversation {chat_id} n'est pas autorisée : `penelope config set \
             telegram.allowed_chats '[{chat_id}]'`"
        ));
    }
    let origin = Origin::Telegram {
        chat_id,
        topic_id,
        message_id: None,
    };
    let moved = s
        .schedules
        .set_origin(id, origin.to_value())
        .await
        .map_err(|e| e.to_string())?;
    if !moved {
        return Err(format!("planification `{id}` introuvable"));
    }
    Ok(place_name(s, &origin).await)
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
pub async fn due_today(d: &Daemon) -> Vec<String> {
    let s = &d.services;
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
            format!("- {} {} → {to}", at.format("%H:%M"), label(d, &sched).await),
        ));
    }
    rows.sort_by_key(|(at, _)| *at);
    rows.into_iter().map(|(_, line)| line).collect()
}
