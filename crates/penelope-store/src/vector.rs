//! Embeddings stockés en BLOB et recherche vectorielle exhaustive.
//!
//! Décision d'architecture : `sqlite-vec` n'est pas utilisé (voir
//! `docs/decisions/0002-pas-de-sqlite-vec.md`). Le PRD §6.11 exige une recherche
//! **exhaustive** tant que `mem_vec` reste sous 200 000 lignes ; une boucle Rust sur
//! des `f32` contigus tient largement cette cible et supprime une dépendance C
//! supplémentaire, tout en gardant la même sémantique de résultat.

/// Encode un vecteur `f32` en octets little-endian.
pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Décode un BLOB d'octets little-endian en vecteur `f32`.
pub fn decode_embedding(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Similarité cosinus. Renvoie 0.0 si l'une des normes est nulle ou si les dimensions
/// diffèrent (cas d'un changement de modèle d'embedding en cours de ré-indexation).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let v = vec![0.5f32, -1.25, 3.0];
        assert_eq!(decode_embedding(&encode_embedding(&v)), v);
    }

    #[test]
    fn cosine_bounds() {
        let a = vec![1.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&a, &[0.0, 1.0]) - 0.0).abs() < 1e-6);
        assert!((cosine_similarity(&a, &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn mismatched_dimensions_are_not_similar() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }
}
