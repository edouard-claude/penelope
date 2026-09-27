//! Différence du préfixe, envoyée en fin de conversation (issue #236).
//!
//! Tant que le cache est chaud, le préfixe (T0 à T2) d'une session ne bouge pas
//! (`stable_prefix`) : un `AGENTS.md` modifié, une skill ajoutée ou un serveur MCP arrivé
//! n'y entrent qu'à la frontière suivante (pause plus longue que le cache, compaction).
//! Pour que le modèle ne suive pas une consigne périmée d'ici là, la différence part en
//! fin, dans le contexte volatil (T4) du message qui suit le changement, une seule fois :
//! les lignes retirées et ajoutées de chaque tuile, et les skills chargées dont le corps
//! a changé depuis.
//!
//! ```text
//!  préfixe retenu (conv.system)      préfixe relu ce tour
//!  ## Contexte du workspace          ## Contexte du workspace
//!  Réponds en français.        ─┐    Réponds en anglais.
//!                               └─►  <mise-a-jour>
//!                                    T2, contexte (AGENTS.md, mémoire) :
//!                                    - Réponds en français.
//!                                    + Réponds en anglais.
//!                                    </mise-a-jour>          (T4, dernier message)
//! ```

use super::Tiers;

/// Au-delà, la différence d'une tuile n'est pas recopiée : la tuile est dite réécrite,
/// et sa nouvelle version attend la frontière (environ 400 tokens par tuile).
pub const TILE_DIFF_MAX_CHARS: usize = 1_500;
/// Au-delà, le calcul de la différence (quadratique) n'est pas tenté.
const DIFF_MAX_LINES: usize = 1_000;

/// Ce qui a changé depuis ce que le modèle sait.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrefixUpdate {
    tiles: Vec<TileChange>,
    skills: Vec<SkillChange>,
}

#[derive(Debug, Clone, PartialEq)]
struct TileChange {
    name: &'static str,
    /// Lignes préfixées `- ` (retirée) ou `+ ` (ajoutée), dans l'ordre du texte.
    lines: Vec<String>,
    removed: usize,
    added: usize,
    /// Trop long pour être recopié.
    rewritten: bool,
}

/// Une skill chargée dans la session dont le corps a changé depuis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillChange {
    Changed(String),
    Removed(String),
}

impl PrefixUpdate {
    /// La différence de `base` (ce que le modèle a lu) à `current` (ce que le prompt
    /// dirait s'il était réécrit), tuile par tuile. Les lignes vides ne comptent pas.
    pub fn between(base: &Tiers, current: &Tiers) -> PrefixUpdate {
        let tiles = [
            ("T0", &base.identity, &current.identity),
            ("T1", &base.index, &current.index),
            ("T2", &base.context, &current.context),
        ]
        .into_iter()
        .filter(|(_, a, b)| a != b)
        .filter_map(|(name, a, b)| TileChange::of(name, a, b))
        .collect();
        PrefixUpdate {
            tiles,
            skills: Vec::new(),
        }
    }

    /// Ajoute les skills chargées dont le corps a changé.
    pub fn with_skills(mut self, skills: Vec<SkillChange>) -> PrefixUpdate {
        self.skills = skills;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty() && self.skills.is_empty()
    }

    /// Les tuiles dont la différence est envoyée ou annoncée.
    pub fn changed_tiles(&self) -> Vec<&'static str> {
        self.tiles.iter().map(|t| t.name).collect()
    }

    /// Les tuiles trop changées pour être recopiées.
    pub fn rewritten_tiles(&self) -> Vec<&'static str> {
        self.tiles
            .iter()
            .filter(|t| t.rewritten)
            .map(|t| t.name)
            .collect()
    }

    /// Le bloc `<mise-a-jour>` posé dans le contexte volatil.
    pub fn block(&self) -> String {
        let mut out = String::from(
            "<mise-a-jour>\nTon contexte a changé depuis le début de cette conversation. Pour \
             garder le cache, le prompt système n'est réécrit qu'à la prochaine pause ; d'ici \
             là, ce qui suit fait foi et remplace ce qu'il dit.\n",
        );
        for t in &self.tiles {
            let label = tile_label(t.name);
            if t.rewritten {
                out.push_str(&format!(
                    "{} : largement réécrit ({} lignes retirées, {} ajoutées), trop long pour \
                     être repris ici ; la nouvelle version entre à la prochaine pause. D'ici \
                     là, en cas de doute, demande au propriétaire.\n",
                    label, t.removed, t.added
                ));
                continue;
            }
            out.push_str(&format!("{label} :\n"));
            for l in &t.lines {
                out.push_str(l);
                out.push('\n');
            }
        }
        for s in &self.skills {
            out.push_str(&match s {
                SkillChange::Changed(n) => format!(
                    "Skill `{n}` modifiée depuis que tu l'as chargée : recharge-la avec \
                     `skill_load` avant de l'appliquer.\n"
                ),
                SkillChange::Removed(n) => {
                    format!(
                        "Skill `{n}` retirée depuis que tu l'as chargée : ne l'applique plus.\n"
                    )
                }
            });
        }
        out.push_str("</mise-a-jour>");
        out
    }
}

impl TileChange {
    fn of(name: &'static str, old: &str, new: &str) -> Option<TileChange> {
        let Some(diff) = line_diff(old, new) else {
            let count = |s: &str| s.lines().filter(|l| !l.trim().is_empty()).count();
            return Some(TileChange {
                name,
                lines: Vec::new(),
                removed: count(old),
                added: count(new),
                rewritten: true,
            });
        };
        let lines: Vec<String> = diff
            .into_iter()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(added, l)| format!("{} {l}", if added { '+' } else { '-' }))
            .collect();
        if lines.is_empty() {
            return None;
        }
        let added = lines.iter().filter(|l| l.starts_with('+')).count();
        let removed = lines.len() - added;
        let chars: usize = lines.iter().map(|l| l.chars().count() + 1).sum();
        let rewritten = chars > TILE_DIFF_MAX_CHARS;
        Some(TileChange {
            name,
            lines: if rewritten { Vec::new() } else { lines },
            removed,
            added,
            rewritten,
        })
    }
}

fn tile_label(name: &str) -> &'static str {
    match name {
        "T0" => "T0, identité et règles (SOUL.md)",
        "T1" => "T1, index des capacités (skills, workflows, serveurs MCP, machine)",
        _ => "T2, contexte (AGENTS.md, mémoire)",
    }
}

/// Les lignes retirées (`false`) et ajoutées (`true`) de `old` à `new`, par plus longue
/// sous-suite commune. `None` au-delà de [`DIFF_MAX_LINES`] lignes d'un côté.
fn line_diff<'a>(old: &'a str, new: &'a str) -> Option<Vec<(bool, &'a str)>> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len() > DIFF_MAX_LINES || b.len() > DIFF_MAX_LINES {
        return None;
    }
    // lcs[i][j] : longueur de la plus longue sous-suite commune de a[i..] et b[j..].
    let w = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * w + j] = if a[i] == b[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            (i, j) = (i + 1, j + 1);
        } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
            out.push((false, a[i]));
            i += 1;
        } else {
            out.push((true, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| (false, *l)));
    out.extend(b[j..].iter().map(|l| (true, *l)));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers(context: &str) -> Tiers {
        Tiers {
            identity: "Tu es Pénélope.".into(),
            index: "## Skills disponibles\n- `revue` : relire\n".into(),
            context: context.into(),
            volatile: String::new(),
        }
    }

    /// Une consigne d'AGENTS.md remplacée : la différence ne porte que les deux lignes
    /// qui changent, sous le nom de sa tuile.
    #[test]
    fn a_changed_line_is_sent_as_a_removal_and_an_addition() {
        let base = tiers("## Contexte du workspace\nRéponds en français.\nTests obligatoires.\n");
        let now = tiers("## Contexte du workspace\nRéponds en anglais.\nTests obligatoires.\n");
        let u = PrefixUpdate::between(&base, &now);
        assert_eq!(u.changed_tiles(), ["T2"]);
        let block = u.block();
        assert!(block.starts_with("<mise-a-jour>\n"), "{block}");
        assert!(block.ends_with("</mise-a-jour>"), "{block}");
        assert!(
            block.contains(
                "T2, contexte (AGENTS.md, mémoire) :\n- Réponds en français.\n+ Réponds en anglais.\n"
            ),
            "{block}"
        );
        assert!(!block.contains("Tests obligatoires"), "{block}");
    }

    /// Un serveur MCP arrivé et une skill ajoutée : lignes ajoutées de T1, rien d'autre.
    #[test]
    fn an_index_addition_names_only_what_arrived() {
        let base = tiers("");
        let mut now = tiers("");
        now.index = "## Skills disponibles\n- `revue` : relire\n- `deploiement` : livrer\n\n\
                     ## Serveurs MCP connectés\n- compta : facturation\n"
            .into();
        let u = PrefixUpdate::between(&base, &now);
        assert_eq!(u.changed_tiles(), ["T1"]);
        let block = u.block();
        for want in [
            "+ - `deploiement` : livrer",
            "+ ## Serveurs MCP connectés",
            "+ - compta : facturation",
        ] {
            assert!(block.contains(want), "{want} : {block}");
        }
        assert!(!block.contains("- - "), "{block}");
    }

    /// Au-delà du seuil, la tuile n'est pas recopiée : elle est dite réécrite, et le
    /// bloc reste court.
    #[test]
    fn a_rewritten_tile_is_announced_not_copied() {
        let long: String = (0..200).map(|i| format!("consigne {i}\n")).collect();
        let u = PrefixUpdate::between(&tiers(""), &tiers(&long));
        assert_eq!(u.rewritten_tiles(), ["T2"]);
        let block = u.block();
        assert!(block.contains("largement réécrit (0 lignes retirées, 200 ajoutées)"));
        assert!(block.chars().count() < 600, "{}", block.chars().count());
    }

    /// Des lignes vides en plus ou en moins ne valent pas une mise à jour ; des tuiles
    /// identiques non plus.
    #[test]
    fn blank_lines_and_equal_tiles_make_no_update() {
        let base = tiers("A\nB\n");
        assert!(PrefixUpdate::between(&base, &tiers("A\n\nB\n")).is_empty());
        assert!(PrefixUpdate::between(&base, &base).is_empty());
    }

    /// Une skill chargée puis modifiée ou retirée se dit, même sans changement de tuile.
    #[test]
    fn a_stale_skill_is_named() {
        let t = tiers("");
        let u = PrefixUpdate::between(&t, &t).with_skills(vec![
            SkillChange::Changed("revue".into()),
            SkillChange::Removed("vieille".into()),
        ]);
        assert!(!u.is_empty());
        let block = u.block();
        assert!(block.contains("Skill `revue` modifiée"), "{block}");
        assert!(block.contains("Skill `vieille` retirée"), "{block}");
    }

    #[test]
    fn the_line_diff_keeps_order() {
        let d = line_diff("a\nb\nc\nd", "a\nx\nc\ny\nd").unwrap();
        assert_eq!(d, [(false, "b"), (true, "x"), (true, "y")]);
        assert!(line_diff(&"x\n".repeat(DIFF_MAX_LINES + 1), "").is_none());
    }
}
