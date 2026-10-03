//! HMAC-SHA256 (RFC 2104) et comparaison en temps constant, écrits sur `sha2` : la
//! signature AWS SigV4 des sauvegardes (#289) et celle des webhooks entrants (#294) en ont
//! besoin, et ni l'une ni l'autre ne justifie une crate `hmac` ou `subtle` de plus.

use sha2::{Digest, Sha256};

/// HMAC-SHA256 : blocs de 64 octets, clé longue hachée d'abord.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut h = Sha256::new();
    h.update(&inner);
    h.update(message);
    let inner_hash = h.finalize();
    let mut h = Sha256::new();
    h.update(&outer);
    h.update(inner_hash);
    h.finalize().into()
}

/// Égalité de deux suites d'octets dont le temps ne dépend pas de la première différence :
/// une signature reçue se compare ainsi à la signature attendue, pour qu'un appelant ne
/// la devine pas octet par octet à la montre. Deux longueurs différentes sont inégales,
/// et c'est dit sans parcourir.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::hex;

    /// Vecteurs de la RFC 4231 (cas 1, 2 et 6 : clé courte, clé « Jefe », clé longue).
    #[test]
    fn rfc_4231_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn constant_time_equality_compares_whole_slices() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
