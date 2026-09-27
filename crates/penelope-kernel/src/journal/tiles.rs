//! La découpe du préfixe système en tuiles (issue #205), que porte `conv.system` et que
//! l'instantané du prompt garde : `penelope-context` la calcule (`Tiers::tile_map`).

use crate::canonical::sha256_hex;
use serde::{Deserialize, Serialize};

/// Une tuile du préfixe, repérée dans le texte rendu : où elle commence, ce qu'elle pèse,
/// et une empreinte courte qui dit si elle a bougé.
///
/// Aucun texte n'y est recopié : la découpe accompagne un instantané de prompt
/// (issue #205) qui porte déjà le rendu, et doit rester négligeable devant lui.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    /// Décalage du premier octet de la tuile dans le préfixe rendu.
    pub at: usize,
    pub len: usize,
    /// Seize caractères de l'empreinte : assez pour nommer la tuile qui a changé.
    pub hash: String,
}

impl Tile {
    pub fn of(rendered: &str, text: &str) -> Tile {
        Tile {
            at: rendered.find(text).unwrap_or(0),
            len: text.len(),
            hash: sha256_hex(text.as_bytes())[..16].to_string(),
        }
    }
}

/// Découpe du préfixe stable en tuiles (T0 identité, T1 index, T2 mémoire).
///
/// T3 (historique) et T4 (volatile) n'en sont pas : ils ne sont pas dans le message
/// système. Le premier vit dans `messages`, le second dans `message_context`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TileMap {
    pub t0: Tile,
    pub t1: Tile,
    pub t2: Tile,
}

impl TileMap {
    fn tile(&self, name: &str) -> Option<&Tile> {
        match name {
            "T0" => Some(&self.t0),
            "T1" => Some(&self.t1),
            "T2" => Some(&self.t2),
            _ => None,
        }
    }

    /// Le texte d'une tuile, relu dans le préfixe rendu. `None` si la découpe ne
    /// correspond pas au texte : l'instantané est alors le seul à faire foi.
    pub fn slice<'a>(&self, rendered: &'a str, name: &str) -> Option<&'a str> {
        let t = self.tile(name)?;
        rendered.get(t.at..t.at + t.len)
    }

    /// Les tuiles dont l'empreinte diffère, dans l'ordre du prompt : ce qui a changé
    /// entre deux préfixes, tuile par tuile plutôt que ligne par ligne.
    pub fn changed(&self, other: &TileMap) -> Vec<&'static str> {
        [
            ("T0", &self.t0, &other.t0),
            ("T1", &self.t1, &other.t1),
            ("T2", &self.t2, &other.t2),
        ]
        .into_iter()
        .filter(|(_, a, b)| a.hash != b.hash)
        .map(|(n, _, _)| n)
        .collect()
    }
}
