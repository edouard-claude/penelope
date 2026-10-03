//! Les rendez-vous du jour pour l'« Aujourd'hui » du digest (#295) : lus par l'outil MCP
//! que nomme `digest.agenda`, à travers le port `McpGateway`, jamais par le client MCP en
//! direct. Le digest reste une affaire de données : des lignes datées, mêlées aux
//! planifications du jour par `digest_inputs`.

use super::*;
use chrono::{NaiveTime, Timelike};

/// Une ligne de l'« Aujourd'hui », et l'heure qui la range (minuit pour une journée
/// entière).
pub(super) type Dated = (NaiveTime, String);

/// Les rendez-vous d'aujourd'hui, une ligne chacun, ou la raison pour laquelle l'agenda
/// n'a pas pu être lu. `Ok(vide)` quand `digest.agenda` n'est pas posé.
pub(super) async fn today(
    s: &Services,
    mcp: Option<Arc<dyn McpAdmin>>,
) -> Result<Vec<Dated>, String> {
    let cfg = s.config.config();
    let qualified = cfg.digest.agenda.trim().to_string();
    if qualified.is_empty() {
        return Ok(Vec::new());
    }
    let registered = s
        .mcp_tools
        .get(&qualified)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "outil `{qualified}` inconnu : le serveur est-il déclaré dans mcp.d, ses outils \
                 listés ?"
            )
        })?;
    if registered.risk != penelope_kernel::risk::RiskClass::Read {
        return Err(format!(
            "le digest n'appelle qu'un outil en lecture ; `{qualified}` est `{}`",
            registered.risk.as_str()
        ));
    }
    let gateway = mcp.ok_or("superviseur MCP non démarré")?;
    let tz = cfg.owner.timezone.clone();
    let result = gateway
        .call_tool(&qualified, &json!({"timezone": tz}), Default::default())
        .await?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(tool_text(&result));
    }
    let tz: Option<chrono_tz::Tz> = tz.parse().ok();
    Ok(lines(&tool_payload(&result), tz))
}

/// Le texte d'un résultat d'outil en erreur.
fn tool_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .and_then(|a| a.iter().find_map(|b| b.get("text").and_then(Value::as_str)))
        .unwrap_or("l'outil a répondu en erreur, sans texte")
        .to_string()
}

/// Les lignes des événements d'un résultat structuré (`events[]`), dans le fuseau du
/// propriétaire ; un événement illisible est passé.
pub(super) fn lines(payload: &Value, tz: Option<chrono_tz::Tz>) -> Vec<Dated> {
    let Some(events) = payload.get("events").and_then(Value::as_array) else {
        return Vec::new();
    };
    events.iter().filter_map(|e| line(e, tz)).collect()
}

fn line(e: &Value, tz: Option<chrono_tz::Tz>) -> Option<Dated> {
    let summary = e.get("summary").and_then(Value::as_str)?.trim();
    if summary.is_empty() {
        return None;
    }
    let calendar = e
        .get("calendar")
        .and_then(Value::as_str)
        .map(|c| format!(" ({c})"))
        .unwrap_or_default();
    if e.get("all_day").and_then(Value::as_bool) == Some(true) {
        return Some((NaiveTime::MIN, format!("- journée {summary}{calendar}")));
    }
    let local = |key: &str| {
        let t = chrono::DateTime::parse_from_rfc3339(e.get(key)?.as_str()?).ok()?;
        Some(match tz {
            Some(tz) => t.with_timezone(&tz).naive_local(),
            None => t.naive_local(),
        })
    };
    let start = local("start")?;
    let when = match local("end") {
        Some(end) if end.date() == start.date() && end != start => {
            format!("{}–{}", start.format("%H:%M"), end.format("%H:%M"))
        }
        Some(end) if end != start => {
            format!("{} → {}", start.format("%H:%M"), end.format("%d/%m %H:%M"))
        }
        _ => start.format("%H:%M").to_string(),
    };
    let at = NaiveTime::from_hms_opt(start.hour(), start.minute(), 0).unwrap_or(NaiveTime::MIN);
    Some((at, format!("- {when} {summary}{calendar}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_dated_lines_in_the_owner_timezone() {
        let payload = json!({"events": [
            {"summary": "Fête", "all_day": true, "start": "2026-10-03", "end": "2026-10-03",
             "calendar": "Famille"},
            {"summary": "Dentiste", "all_day": false, "start": "2026-10-03T07:00:00Z",
             "end": "2026-10-03T07:45:00Z", "calendar": "Perso"},
            {"summary": "Garde", "all_day": false, "start": "2026-10-03T19:00:00+04:00",
             "end": "2026-10-04T01:00:00+04:00"},
            {"summary": "Top", "all_day": false, "start": "2026-10-03T06:00:00Z",
             "end": "2026-10-03T06:00:00Z"},
            {"summary": "  ", "all_day": true},
            {"all_day": false, "start": "pas une date"},
            {"summary": "Sans date", "all_day": false},
        ]});
        let dated = lines(&payload, Some(chrono_tz::Indian::Reunion));
        let texts: Vec<&str> = dated.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "- journée Fête (Famille)",
                "- 11:00–11:45 Dentiste (Perso)",
                "- 19:00 → 04/10 01:00 Garde",
                "- 10:00 Top",
            ]
        );
        assert_eq!(dated[0].0, NaiveTime::MIN);
        assert_eq!(dated[1].0, NaiveTime::from_hms_opt(11, 0, 0).unwrap());
        assert!(lines(&json!({"count": 0}), None).is_empty());
        assert!(lines(&json!("texte"), None).is_empty());
    }
}
