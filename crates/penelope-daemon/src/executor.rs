//! Façade de l'exécuteur natif : il vit dans `penelope-executor` (épopée #208, T24),
//! réexporté sous son ancien chemin jusqu'à T30.

pub use penelope_executor::executor::*;

#[cfg(test)]
mod tests;
