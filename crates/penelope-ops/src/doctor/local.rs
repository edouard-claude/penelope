//! Inférence locale (#259) : le serveur OpenAI-compatible qui sert un alias de texte
//! (mlx_lm.server, llama.cpp, LM Studio) répond, sert les modèles visés, et relit son
//! préfixe.
//!
//! Un endpoint qui ne sert que la voix (`stt`, `tts`) n'est pas sondé : whisper-server et
//! mlx-audio n'exposent pas tous `GET /models`, et leur propre contrôle est ailleurs.

use super::*;
use penelope_kernel::config::{Config, LocalProvider};

/// Délai de la sonde : un serveur local répond en millisecondes, même en plein calcul.
const PROBE: std::time::Duration = std::time::Duration::from_secs(5);

/// Rôles servis par un serveur audio, jamais par le serveur de texte.
const AUDIO_ROLES: &[&str] = &["stt", "tts"];

/// Un contrôle par endpoint local actif qui sert au moins un alias de texte.
pub async fn local_inference_checks(s: &Services) -> Vec<DoctorCheck> {
    let cfg = s.config.config();
    let mut checks = Vec::new();
    for (name, endpoint) in endpoints(&cfg) {
        let wanted = text_aliases(&cfg, name);
        if wanted.is_empty() {
            continue;
        }
        checks.push(endpoint_check(s, name, endpoint, &wanted).await);
    }
    checks
}

/// `providers.local` puis les endpoints supplémentaires, actifs seulement.
fn endpoints(cfg: &Config) -> Vec<(&str, &LocalProvider)> {
    let mut out = Vec::new();
    if cfg.providers.local.enabled {
        out.push(("local", &cfg.providers.local));
    }
    out.extend(
        cfg.providers
            .extra
            .iter()
            .filter(|(_, e)| e.enabled)
            .map(|(n, e)| (n.as_str(), e)),
    );
    out
}

/// Alias de texte (`alias`, modèle sans préfixe) que l'endpoint `name` sert : même règle
/// de routage que `penelope-llm`, sans les alias qui ne servent que la voix.
fn text_aliases(cfg: &Config, name: &str) -> Vec<(String, String)> {
    cfg.models
        .aliases
        .iter()
        .filter_map(|(alias, model)| {
            let (prefix, bare) = model.split_once(':')?;
            if !matches!(prefix, "local" | "openai_compat") {
                return None;
            }
            let roles: Vec<&str> = cfg
                .models
                .roles
                .iter()
                .filter(|(_, a)| *a == alias)
                .map(|(r, _)| r.as_str())
                .collect();
            if !roles.is_empty() && roles.iter().all(|r| AUDIO_ROLES.contains(r)) {
                return None;
            }
            let (served_by, _) = cfg.providers.local_endpoint(bare)?;
            (served_by == name).then(|| (alias.clone(), bare.to_string()))
        })
        .collect()
}

async fn endpoint_check(
    s: &Services,
    name: &str,
    endpoint: &LocalProvider,
    wanted: &[(String, String)],
) -> DoctorCheck {
    let id = format!("local.{name}");
    let label = format!("Inférence locale `{name}`");
    let base = endpoint.base_url.trim_end_matches('/');
    let key = s
        .platform
        .secrets
        .expand(&endpoint.api_key)
        .unwrap_or_default();
    let probe = penelope_llm::OpenAiCompatProvider::new(base, key, penelope_llm::Catalog::new());
    let served = match probe {
        Ok(p) => p.served_models(PROBE).await,
        Err(e) => Err(e),
    };
    let served = match served {
        Ok(v) => v,
        Err(e) => {
            return DoctorCheck::fail(
                &id,
                &label,
                format!(
                    "serveur injoignable à {base} ({e}) : les tours de {} passent au repli",
                    names(wanted)
                ),
                Some(start_fix(name, &wanted[0].1)),
            );
        }
    };
    if served.is_empty() {
        return DoctorCheck::fail(
            &id,
            &label,
            format!("{base} répond mais ne sert aucun modèle"),
            Some(start_fix(name, &wanted[0].1)),
        );
    }
    let missing: Vec<String> = wanted
        .iter()
        .filter(|(_, m)| !served.iter().any(|x| &x.id == m))
        .map(|(a, m)| format!("`{a}` → `{m}`"))
        .collect();
    if !missing.is_empty() {
        let list: Vec<&str> = served.iter().map(|x| x.id.as_str()).take(8).collect();
        return DoctorCheck::fail(
            &id,
            &label,
            format!(
                "{} non servi(s) par {base} (servis : {})",
                missing.join(", "),
                list.join(", ")
            ),
            Some(format!(
                "télécharger le modèle (`hf download {}`), ou \
                 `penelope model set <alias> local:<modèle servi>`",
                wanted[0].1
            )),
        );
    }
    let mut parts = vec![format!("{} modèle(s) servi(s) à {base}", served.len())];
    for (alias, model) in wanted {
        let window = match served
            .iter()
            .find(|x| &x.id == model)
            .and_then(|x| x.window)
        {
            Some(w) => format!("fenêtre {w}"),
            None => format!(
                "fenêtre {} (configurée, le serveur ne l'annonce pas)",
                endpoint.context_window
            ),
        };
        let cache = prefix_cache(s, model).await;
        parts.push(format!("`{alias}` → `{model}` : {window}, {cache}"));
    }
    DoctorCheck::ok(&id, &label, parts.join(" ; "))
}

fn names(wanted: &[(String, String)]) -> String {
    wanted
        .iter()
        .map(|(a, _)| format!("`{a}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Correction d'un serveur absent : le relancer s'il est supervisé, sinon l'installer
/// en LaunchAgent.
fn start_fix(endpoint: &str, model: &str) -> String {
    let label = penelope_platform::service::inference_label(endpoint);
    let supervised =
        penelope_platform::service::launch_agent_path(&label).is_some_and(|p| p.exists());
    if supervised {
        format!("launchctl kickstart -k gui/$(id -u)/{label}")
    } else {
        format!("penelope local install {model} --endpoint {endpoint}")
    }
}

/// Part des jetons d'entrée relus du cache du serveur sur sept jours : c'est ce qui
/// évite de recalculer tout le préfixe à chaque tour.
async fn prefix_cache(s: &Services, model: &str) -> String {
    let since = (s.clock.now_utc() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let model = model.to_string();
    let (calls, prompt, cached): (i64, i64, i64) = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt), 0), COALESCE(SUM(cached), 0)
                 FROM usage WHERE ts >= ?1 AND (model = ?2 OR model LIKE '%:' || ?2)",
                [since, model],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap_or((0, 0, 0));
    if calls == 0 || prompt == 0 {
        return "cache de préfixe pas encore mesuré".into();
    }
    let share = (cached as f64 * 100.0 / prompt as f64).round() as i64;
    format!("{share} % de l'entrée relue du cache sur 7 jours ({calls} appel(s))")
}
