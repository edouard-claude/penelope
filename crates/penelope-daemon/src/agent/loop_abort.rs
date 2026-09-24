//! Boucle arrêtée : le tour répond quand même (issue #31).

use super::*;

/// Note laissée dans la conversation quand le détecteur arrête une boucle : le tour suivant
/// voit que l'approche a échoué.
pub const LOOP_STOP_NOTE: &str =
    "[boucle arrêtée par le harnais : cette approche a échoué, ne pas la réessayer telle quelle]";

/// Suites proposées quand le modèle n'en donne pas.
const LOOP_DEFAULT_CHOICES: [&str; 3] = [
    "Chercher autrement",
    "Je te précise (compte, dossier, dates)",
    "Laisser tomber",
];

/// Dernier résultat réel d'un appel identique (même outil, mêmes arguments) dans le
/// transcript, avertissements du harnais exclus.
pub fn last_result_of(tail: &[ChatMessage], tool: &str, args: &Value) -> Option<String> {
    let ids: BTreeSet<&str> = tail
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .filter(|c| c.name == tool && &c.arguments == args)
        .map(|c| c.id.as_str())
        .collect();
    tail.iter()
        .rev()
        .filter(|m| m.role == Role::Tool)
        .filter(|m| m.tool_call_id.as_deref().is_some_and(|id| ids.contains(id)))
        .map(|m| m.text())
        .find(|t| !t.starts_with("[avertissement du harnais]") && !t.contains(LOOP_STOP_NOTE))
}

/// Sépare la réponse de la ligne `CHOIX : a | b | c` qui la termine.
pub fn split_choices(text: &str) -> (String, Vec<String>) {
    let mut lines: Vec<&str> = text.trim_end().lines().collect();
    let Some(pos) = lines.iter().rposition(|l| {
        l.trim()
            .trim_start_matches(['*', '_', '-', ' '])
            .to_lowercase()
            .starts_with("choix")
    }) else {
        return (text.trim().to_string(), Vec::new());
    };
    let line = lines.remove(pos);
    let choices: Vec<String> = line
        .split_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or_default()
        .split('|')
        .map(|c| {
            c.trim()
                .trim_matches(['*', '_', '`', '«', '»', '"'])
                .trim()
                .to_string()
        })
        .filter(|c| !c.is_empty())
        .map(|c| c.chars().take(60).collect())
        .take(3)
        .collect();
    (lines.join("\n").trim().to_string(), choices)
}

impl AgentLoop {
    /// Réponse après une boucle arrêtée (issue #31) : un appel sans outil explique ce qui a
    /// été tenté et l'erreur exacte, et propose des suites ; à défaut, un repli lisible qui
    /// cite l'erreur réelle. La réponse est gardée dans la conversation.
    pub(super) async fn answer_after_loop(
        &self,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        report: String,
        tool: &str,
        last_result: Option<&str>,
        attempts: &Attempts,
    ) -> anyhow::Result<TurnOutcome> {
        let s = &self.services;
        let mut messages = conv.request_messages().await?;
        if spec.cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled);
        }
        messages.push(ChatMessage::user(format!(
            "(Message du harnais, pas du propriétaire.) Tu as appelé `{tool}` en boucle avec \
             les mêmes arguments : les outils sont arrêtés pour ce tour. Réponds maintenant au \
             propriétaire, sans outil, en quelques lignes : ce que tu as essayé, l'erreur exacte \
             renvoyée par l'outil (cite-la telle quelle), et ce que tu as déjà obtenu s'il y a \
             quelque chose. Termine par une seule ligne `CHOIX : <suite 1> | <suite 2> | <suite \
             3>`, deux ou trois suites courtes qu'il pourra choisir d'un clic (par exemple \
             Chercher autrement, Je te précise le compte ou les dates, Laisser tomber)."
        )));
        let text = match self
            .call_model(
                spec,
                messages,
                &NullSink,
                None,
                Some(ToolChoice::None),
                attempts,
            )
            .await?
        {
            Ok(r) => {
                let _ = s
                    .budget
                    .record(penelope_kernel::budget::UsageRecord {
                        session_id: Some(spec.session_id.clone()),
                        run_id: spec.run_id.clone(),
                        turn_id: spec.turn_id.clone(),
                        model: r.model.clone(),
                        provider: r.provider.clone(),
                        role: Some("chat".into()),
                        generation_id: (!r.id.is_empty()).then(|| r.id.clone()),
                        prompt: r.usage.prompt,
                        completion: r.usage.completion,
                        cached: r.usage.cached,
                        reasoning: r.usage.reasoning,
                        cost_usd: r.cost_usd,
                        estimated: r.cost_estimated,
                        ..Default::default()
                    })
                    .await;
                r.message.text()
            }
            Err(failure) => {
                tracing::warn!(error = %failure.message, "réponse après boucle impossible");
                String::new()
            }
        };
        let (mut answer, mut choices) = split_choices(&text);
        if answer.trim().is_empty() {
            answer = format!(
                "Je me suis arrêtée : j'appelais `{tool}` en boucle sans avancer.\n\nErreur \
                 renvoyée par l'outil : {}\n\nComment veux-tu continuer ?",
                last_result
                    .map(|r| r.chars().take(800).collect::<String>())
                    .unwrap_or_else(|| "aucun résultat exploitable".into())
            );
        }
        if choices.len() < 2 {
            choices = LOOP_DEFAULT_CHOICES.iter().map(|c| c.to_string()).collect();
        }
        let prov = Provenance {
            turn: spec.turn_id.clone(),
            ..Default::default()
        };
        conv.record_as(&ChatMessage::assistant(&answer), true, &prov)
            .await?;
        Ok(TurnOutcome::LoopAborted {
            report,
            answer,
            choices,
        })
    }
}
