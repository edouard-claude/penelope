//! Étape `foreach` (#338) : une liste déroulée dans l'ordre, échecs (`stop`, `skip`,
//! `retry`), reprise après redémarrage, points d'arrêt, et le scénario de bout en bout
//! avec un quota épuisé au milieu (#339).

use super::*;
use penelope_workflow::Control;
use penelope_workflow::foreach::{ItemState, ItemStore};

/// Sous-workflow d'essai : une commande qui échoue pour l'élément `c` (une seule fois
/// avec `flaky`).
fn shell_task(flaky: bool) -> Value {
    let cmd = if flaky {
        "if [ {{item.title}} = c ] && [ ! -f essai-c ]; then touch essai-c; exit 1; fi; \
         echo {{item.title}} >> faits.txt"
    } else {
        "if [ {{item.title}} = c ]; then exit 1; fi; echo {{item.title}} >> faits.txt"
    };
    json!({
        "metadata": {"id": "tache-essai", "name": "Tâche d'essai",
                     "parameters": [{"id": "item", "type": "object", "required": true}]},
        "entryStep": "faire",
        "settings": {"maxIterations": 5, "budget": {"maxUsd": 1.0}},
        "steps": [{"id": "faire", "type": "shell", "command": cmd,
                   "transitions": [
                     {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                     {"goto": "$blocked"}]}]
    })
}

fn list(child: &str, extra: Value) -> Value {
    let mut step = json!({
        "id": "liste", "type": "foreach", "workflowId": child,
        "items": [{"title": "a"}, {"title": "b"}, {"title": "c"}, {"title": "d"}, {"title": "e"}],
        "transitions": [
            {"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
            {"goto": "$done", "condition": {"type": "step_result", "result": "partial"}},
            {"goto": "$blocked"}]
    });
    for (k, v) in extra.as_object().unwrap() {
        step[k] = v.clone();
    }
    let mut raw = wf("liste", "liste", json!([step]));
    raw["settings"]["budget"]["maxWallMs"] = json!(0);
    raw
}

/// Fait avancer tous les runs actifs, plusieurs passes, comme le pilote.
async fn pump(d: &Context) {
    for _ in 0..40 {
        for r in d
            .services
            .runs
            .list(Some(RunState::Running), 50)
            .await
            .unwrap()
        {
            drive(d, &r.id).await.unwrap();
        }
    }
}

async fn states(s: &Services, run_id: &str) -> Vec<ItemState> {
    ItemStore::new(s.store.clone(), s.clock.clone())
        .latest(run_id)
        .await
        .unwrap()
        .iter()
        .map(|i| i.state)
        .collect()
}

fn faits(run: &Run) -> String {
    std::fs::read_to_string(std::path::Path::new(run.workdir.as_deref().unwrap()).join("faits.txt"))
        .unwrap_or_default()
}

use ItemState::{Done as D, Failed as F, Skipped as S, Todo as T};

/// `stop` (défaut) : la liste s'arrête au premier élément en échec, le propriétaire le
/// lit dans le sujet ; ce qui reste n'est pas commencé.
#[tokio::test]
async fn a_failed_item_stops_the_list() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(false)).await;
    install(&e.d, list("tache-essai", json!({}))).await;
    let run = start_run(&e.d, "liste", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Blocked);
    assert_eq!(states(s, &run.id).await, vec![D, D, F, T, T]);
    assert_eq!(faits(&r), "a\nb\n");
    let texts = e.r.texts().join("\n");
    assert!(texts.contains("✅ élément 1/5 · a : fait"), "{texts}");
    assert!(texts.contains("❌ élément 3/5 · c : en échec"), "{texts}");
    assert_eq!(r.step_outputs["liste"]["failed"], 1);
}

/// `skip` : l'élément en échec est noté, la liste continue et finit `partial`.
#[tokio::test]
async fn a_skipped_item_is_noted_and_the_list_goes_on() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(false)).await;
    install(&e.d, list("tache-essai", json!({"onError": "skip"}))).await;
    let run = start_run(&e.d, "liste", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Done);
    assert_eq!(states(s, &run.id).await, vec![D, D, S, D, D]);
    assert_eq!(faits(&r), "a\nb\nd\ne\n");
    assert_eq!(r.step_outputs["liste"]["skipped"], 1);
    assert!(
        e.r.texts()
            .iter()
            .any(|t| t.starts_with("⏭ élément 3/5 · c : sauté"))
    );
}

/// `retry:1` : un élément qui échoue une fois est relancé dans un nouveau sous-run.
#[tokio::test]
async fn a_retried_item_succeeds_on_its_second_attempt() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(true)).await;
    install(&e.d, list("tache-essai", json!({"on_error": "retry:1"}))).await;
    let run = start_run(&e.d, "liste", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Done);
    assert_eq!(states(s, &run.id).await, vec![D, D, D, D, D]);
    let items = ItemStore::new(s.store.clone(), s.clock.clone())
        .latest(&run.id)
        .await
        .unwrap();
    assert_eq!(items[2].attempts, 1);
    assert_eq!(faits(&r), "a\nb\nc\nd\ne\n");
}

/// Le daemon redémarre pendant le 3ᵉ élément : même base, aucun pilote ne tient plus
/// les runs. La liste reprend à l'élément courant, avec son sous-run, sans refaire les
/// deux premiers.
#[tokio::test]
async fn the_list_resumes_at_its_current_item_after_a_restart() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(false)).await;
    install(&e.d, list("tache-essai", json!({"onError": "skip"}))).await;
    let run = start_run(&e.d, "liste", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    let store = ItemStore::new(s.store.clone(), s.clock.clone());
    // Deux éléments faits, le troisième lancé, son sous-run pas encore piloté.
    for _ in 0..2 {
        drive(&e.d, &run.id).await.unwrap();
        let cur = store.latest(&run.id).await.unwrap();
        let child = cur.iter().find(|i| i.state == ItemState::Running).unwrap();
        drive(&e.d, child.child_run.as_deref().unwrap())
            .await
            .unwrap();
    }
    drive(&e.d, &run.id).await.unwrap();
    assert_eq!(
        states(s, &run.id).await,
        vec![D, D, ItemState::Running, T, T]
    );
    let third = store.latest(&run.id).await.unwrap()[2].child_run.clone();
    assert_eq!(
        position_of(s, &s.runs.get(&run.id).await.unwrap().unwrap())
            .await
            .as_deref(),
        Some("élément 3/5 · c · étape faire")
    );

    let restarted = Context {
        workflows: Arc::new(State::with_ports(e.d.ports.clone())),
        ..e.d.cx.clone()
    };
    pump(&restarted).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Done);
    assert_eq!(states(s, &run.id).await, vec![D, D, S, D, D]);
    assert_eq!(store.latest(&run.id).await.unwrap()[2].child_run, third);
    assert_eq!(faits(&r), "a\nb\nd\ne\n", "rien n'est refait");
}

/// `pauseEvery: 2` : une validation du propriétaire tous les deux éléments, avec le
/// bilan ; « Reprendre » continue là.
#[tokio::test]
async fn pause_every_asks_the_owner_like_a_sprint_end() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(false)).await;
    install(
        &e.d,
        list(
            "tache-essai",
            json!({"onError": "skip", "pauseEvery": 2, "itemNoun": "story"}),
        ),
    )
    .await;
    let run = start_run(&e.d, "liste", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Paused);
    assert_eq!(states(s, &run.id).await, vec![D, D, T, T, T]);
    let bilan =
        e.r.texts()
            .into_iter()
            .find(|t| t.starts_with("⏸ Point d'arrêt"))
            .unwrap();
    assert!(bilan.contains("story 2/5 (b)"), "{bilan}");
    assert!(
        bilan.contains("Bilan : 2 faits, 0 en échec, 0 sauté sur 5"),
        "{bilan}"
    );
    assert!(bilan.contains("Suivant : c"), "{bilan}");
    assert_eq!(
        list_of(s, &run.id).await.unwrap().lines().next(),
        Some("Bilan : 2 faits, 0 en échec, 0 sauté sur 5.")
    );

    control(&e.d, &run.id, &Control::Resume).await.unwrap();
    pump(&e.d).await;
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Paused,
        "c sauté et d fait : deuxième point d'arrêt"
    );
    control(&e.d, &run.id, &Control::Resume).await.unwrap();
    pump(&e.d).await;
    assert_eq!(
        s.runs.get(&run.id).await.unwrap().unwrap().state,
        RunState::Done
    );
    assert_eq!(states(s, &run.id).await, vec![D, D, S, D, D]);
}

/// La liste vient de la sortie d'une étape (un agent qui l'a lue dans le tracker), et
/// elle est figée : la même liste après coup ne change rien.
#[tokio::test]
async fn a_list_from_a_step_is_frozen() {
    let e = env().await;
    let s = &e.d.services;
    install(&e.d, shell_task(false)).await;
    let raw = wf(
        "depuis",
        "lire",
        json!([
            {"id": "lire", "type": "shell",
             "command": "echo '[{\"title\": \"a\"}, {\"title\": \"b\"}]'",
             "transitions": [{"goto": "liste"}]},
            {"id": "liste", "type": "foreach", "workflowId": "tache-essai",
             "items": {"step": "lire", "path": "stdout"}, "itemLabel": "« {{item.title}} »",
             "transitions": [{"goto": "$done", "condition": {"type": "step_result", "result": "success"}},
                             {"goto": "$blocked"}]}
        ]),
    );
    install(&e.d, raw).await;
    let run = start_run(&e.d, "depuis", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Done, "{}", r.step_outputs);
    let labels: Vec<String> = ItemStore::new(s.store.clone(), s.clock.clone())
        .latest(&run.id)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.label)
        .collect();
    assert_eq!(labels, vec!["« a »", "« b »"]);
}

/// Le choix du propriétaire : rester sur son modèle, sans repli.
fn no_fallback(e: &Env) {
    e.d.services
        .config
        .mutate("test", |c| {
            let name = c.models.active_name().to_string();
            if let Some(p) = c.models.materialize(&name) {
                p.fallback.clear();
            }
            Ok(vec![])
        })
        .unwrap();
}

fn done_and_reply(e: &Env, id: &str) {
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: id.into(),
            name: "step_done".into(),
            arguments: json!({}),
        }],
    ));
    e.p.reply("Fait.");
}

/// De bout en bout : un backlog de cinq éléments factices, quota Codex épuisé au 3ᵉ. Le
/// sous-run se met en pause, le propriétaire lit où elle s'est arrêtée et quand elle
/// reprend ; au retour du quota (horloge de test), la liste reprend seule et finit avec
/// ses cinq éléments faits, dans l'ordre.
#[tokio::test]
async fn a_backlog_survives_a_spent_quota_on_its_third_item() {
    let e = env().await;
    let s = &e.d.services;
    no_fallback(&e);
    install(
        &e.d,
        json!({
            "metadata": {"id": "tache-dev", "name": "Tâche",
                         "parameters": [{"id": "item", "type": "object", "required": true}]},
            "entryStep": "dev",
            "settings": {"maxIterations": 5, "budget": {"maxUsd": 1.0}},
            "steps": [{"id": "dev", "type": "agent", "prompt": "Développe {{item.title}}.",
                       "transitions": [{"goto": "$done"}]}]
        }),
    )
    .await;
    let mut raw = list("tache-dev", json!({"itemNoun": "tâche"}));
    raw["metadata"]["id"] = json!("backlog-essai");
    install(&e.d, raw).await;
    done_and_reply(&e, "t1");
    done_and_reply(&e, "t2");
    e.p.push(Scripted::UsageLimit(Some(3_600)));
    let run = start_run(&e.d, "backlog-essai", json!({}), &owner(), None, 0)
        .await
        .unwrap();
    pump(&e.d).await;

    assert_eq!(
        states(s, &run.id).await,
        vec![D, D, ItemState::Running, T, T]
    );
    let items = ItemStore::new(s.store.clone(), s.clock.clone())
        .latest(&run.id)
        .await
        .unwrap();
    let third = items[2].child_run.clone().unwrap();
    assert_eq!(
        s.runs.get(&third).await.unwrap().unwrap().state,
        RunState::Paused
    );
    let said: Vec<String> =
        e.r.texts()
            .into_iter()
            .filter(|t| t.starts_with("⏸ Je me suis arrêtée là"))
            .collect();
    assert_eq!(said.len(), 1, "un seul message : {said:?}");
    assert!(
        said[0].contains("crédits Codex épuisés. Dernier point : tâche 3/5 · c, étape dev"),
        "{}",
        said[0]
    );
    assert!(said[0].contains("Reprise prévue à"), "{}", said[0]);

    // Le quota revient : la reprise est automatique, la liste finit.
    e.clock.advance_secs(3_601);
    for id in ["t3", "t4", "t5"] {
        done_and_reply(&e, id);
    }
    assert_eq!(crate::workflow::credits::resume_due(&e.d).await.unwrap(), 1);
    pump(&e.d).await;
    let r = s.runs.get(&run.id).await.unwrap().unwrap();
    assert_eq!(r.state, RunState::Done, "{}", r.step_outputs);
    assert_eq!(states(s, &run.id).await, vec![D, D, D, D, D]);
    assert_eq!(r.step_outputs["liste"]["done"], 5);
    let reports: Vec<String> =
        e.r.texts()
            .into_iter()
            .filter(|t| t.starts_with("✅ tâche"))
            .collect();
    assert_eq!(reports.len(), 5, "{reports:?}");
    assert!(
        reports[4].starts_with("✅ tâche 5/5 · e : fait"),
        "{reports:?}"
    );
    let prompts: Vec<String> =
        e.p.requests()
            .iter()
            .filter_map(|r| {
                r.messages
                    .iter()
                    .find_map(|m| m.text().contains("Développe").then(|| m.text()))
            })
            .collect();
    assert!(
        prompts.iter().any(|p| p.contains("Développe c.")),
        "{prompts:?}"
    );
}
