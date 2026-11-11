//! Blocs de code clôturés d'une note Markdown (```` ``` ```` et `~~~`).

/// Suit les clôtures ligne à ligne. Ce qui est dans un bloc de code n'est ni une entrée,
/// ni un titre, ni un lien : c'est un exemple, que rien ne lit ni ne réécrit (#453).
#[derive(Debug, Default, Clone)]
pub struct Fences {
    /// Marque et longueur de la clôture ouvrante : la fermante est la même marque, au
    /// moins aussi longue, seule sur sa ligne.
    open: Option<(char, usize)>,
}

impl Fences {
    /// Vrai si la ligne appartient à un bloc de code, clôtures comprises. À appeler une
    /// fois par ligne, dans l'ordre du fichier.
    pub fn covers(&mut self, line: &str) -> bool {
        let t = line.trim_start();
        let marker = t.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |m| t.chars().take_while(|c| *c == m).count());
        match (self.open, marker) {
            (Some((m, n)), Some(c)) if c == m && run >= n && t[run..].trim().is_empty() => {
                self.open = None;
                true
            }
            (Some(_), _) => true,
            // Du code en ligne (« ```x``` » sur une seule ligne) n'ouvre pas de bloc.
            (None, Some(m)) if run >= 3 && !(m == '`' && t[run..].contains('`')) => {
                self.open = Some((m, run));
                true
            }
            (None, _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Fences;

    fn covered(raw: &str) -> Vec<bool> {
        let mut f = Fences::default();
        raw.lines().map(|l| f.covers(l)).collect()
    }

    #[test]
    fn backticks_and_tildes_open_and_close_their_own_block() {
        assert_eq!(
            covered("a\n```sh\n- x\n~~~\n```\nb"),
            [false, true, true, true, true, false]
        );
        assert_eq!(
            covered("~~~\n```\n## T\n~~~\nb"),
            [true, true, true, true, false]
        );
        // Une clôture plus longue ne se ferme que par une clôture au moins aussi longue.
        assert_eq!(
            covered("````\n```\n- x\n````\nb"),
            [true, true, true, true, false]
        );
        // Un bloc dans une liste, en retrait.
        assert_eq!(
            covered("- a\n  ```\n  - x\n  ```\n- b"),
            [false, true, true, true, false]
        );
    }

    #[test]
    fn inline_code_and_short_runs_open_nothing() {
        assert_eq!(covered("```x``` puis\n- a"), [false, false]);
        assert_eq!(covered("``\n- a\n~~barré~~"), [false, false, false]);
        // Une clôture jamais fermée couvre la fin du fichier.
        assert_eq!(covered("```\n- a"), [true, true]);
    }
}
