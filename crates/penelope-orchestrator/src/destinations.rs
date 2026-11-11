//! Une conversation mémorisée n'existe plus côté canal (#463).
//!
//! Une planification et un run gardent leur destination longtemps après leur création.
//! Quand le canal apprend qu'elle a disparu (un sujet supprimé, appris au refus d'un
//! envoi), il le dit ici, et ce qui y livrait est rattaché là où il dit de livrer.
//!
//! Le cœur ne sait pas ce qui fait qu'une conversation en est une autre : il présente
//! chaque destination mémorisée, le canal répond.

use penelope_app::bus::Origin;
use penelope_app::services::Services;

/// La réponse du canal pour une destination mémorisée : `Some(autre)`, la conversation a
/// disparu et ce qui lui était destiné se livre désormais dans `autre` ; `None`, elle tient.
pub type Moved<'a> = &'a (dyn Fn(&Origin) -> Option<Origin> + Send + Sync);

/// Ce qui a été rattaché.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Relocated {
    pub schedules: Vec<String>,
    pub runs: Vec<String>,
}

impl Relocated {
    pub fn is_empty(&self) -> bool {
        self.schedules.is_empty() && self.runs.is_empty()
    }
}

/// Rattache les planifications et les runs encore vivants dont la destination a disparu.
/// Sans effet sur ce qui livre ailleurs, ni sur une planification sans destination
/// propre : elle suit déjà la conversation du propriétaire.
pub async fn relocate(s: &Services, moved: Moved<'_>) -> anyhow::Result<Relocated> {
    Ok(Relocated {
        schedules: crate::scheduler::relocate(s, moved).await?,
        runs: crate::workflow::relocate(s, moved).await?,
    })
}
