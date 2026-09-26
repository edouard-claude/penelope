//! Indicateur d'activité, ligne d'état d'un outil, rapport de `/stop`.

use super::*;

#[test]
fn the_activity_follows_the_tool() {
    assert_eq!(activity_for("send_file"), "upload_document");
    assert_eq!(activity_for("artifact_read"), "upload_document");
    assert_eq!(activity_for("send_voice"), "record_voice");
    assert_eq!(activity_for("image_generate"), "upload_photo");
    assert_eq!(activity_for("shell_exec"), "typing");
}

/// #121 : la ligne d'état nomme l'outil et ce qu'il vise, raccourci.
#[test]
fn a_tool_status_names_its_target_briefly() {
    assert_eq!(
        tool_status("shell_exec", &json!({"command": "cargo   test\n  -p x"})),
        "⚙️ shell_exec · cargo test -p x"
    );
    let long = tool_status(
        "http_fetch",
        &json!({"url": "https://exemple.org/".repeat(10)}),
    );
    assert!(long.ends_with('…') && long.chars().count() < 80, "{long}");
    assert_eq!(tool_status("time_now", &json!({})), "⚙️ time_now…");
}

/// #155 : `/stop` ne promet que ce qu'il a fait, et dit ce qui continue.
#[test]
fn the_stop_report_says_what_was_done_and_what_goes_on() {
    assert_eq!(StopReport::default().render(), "Rien à arrêter.");
    let r = StopReport {
        running: true,
        queued: 2,
        burst: 3,
        sessions: 1,
        paused: 1,
        cancelled_ingests: 1,
        cancelled_jobs: 2,
        left: vec!["`r1` (blocked)".into()],
        tout: true,
        ..Default::default()
    }
    .render();
    for part in [
        "⏹ Tour arrêté, 2 message(s) en attente annulé(s).",
        "3 morceau(x) reçus à l'instant écartés.",
        "1 autre(s) session(s) de ce chat vidée(s).",
        "1 run(s) de workflow mis en pause.",
        "1 ingestion(s) de document interrompue(s).",
        "2 job(s) d'outil interrompu(s).",
        "Run(s) laissé(s) ouvert(s) : `r1` (blocked).",
    ] {
        assert!(r.contains(part), "{part} :\n{r}");
    }
    let r = StopReport {
        open: vec![("r2".into(), "running".into())],
        ingests: 1,
        ..Default::default()
    }
    .render();
    assert!(r.starts_with("⏹ Aucun tour en cours."), "{r}");
    assert!(
        r.contains("Continue : 1 run(s) ouvert(s) (`r2` running), 1 ingestion(s) de document."),
        "{r}"
    );
    assert_eq!(
        StopReport {
            queued: 4,
            ..Default::default()
        }
        .render(),
        "⏹ 4 message(s) en attente annulé(s)."
    );
    assert_eq!(
        StopReport {
            running: true,
            ..Default::default()
        }
        .render(),
        "⏹ Tour arrêté."
    );
}
