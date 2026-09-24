//! Le journal contre les caches, à la fin de chaque scénario (épopée #208, T12 ;
//! `design/v1/source-de-verite.md` §4.2).
//!
//! `history verify` sur la base que le scénario laisse, après le relevé du monde (le
//! contrôle ne change donc ni `expected.jsonl` ni `surface.jsonl`) : zéro divergence, pour
//! toutes les sessions, archives de retour arrière et filles de fork comprises.

use penelope_daemon::Services;

pub(super) async fn check(s: &Services, name: &str) -> anyhow::Result<()> {
    let report = s.context.history.verify(None, None).await?;
    anyhow::ensure!(
        report.ok,
        "scénario {name} : le journal diverge des caches : {}",
        serde_json::to_string_pretty(&report.divergences)?
    );
    Ok(())
}
