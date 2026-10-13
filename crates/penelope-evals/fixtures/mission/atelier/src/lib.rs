//! Atelier : un inventaire minuscule, dépôt témoin du banc d'essai `mission-bench`.
#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub qty: u32,
    pub price_cents: u32,
}

#[derive(Debug, Default)]
pub struct Inventory {
    items: Vec<Item>,
}

impl Inventory {
    /// Ajoute un article ; un nom déjà présent cumule les quantités et garde le dernier prix.
    pub fn add(&mut self, item: Item) {
        if let Some(existing) = self.items.iter_mut().find(|i| i.name == item.name) {
            existing.qty += item.qty;
            existing.price_cents = item.price_cents;
        } else {
            self.items.push(item);
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Valeur du stock en centimes.
    pub fn total_cents(&self) -> u64 {
        self.items
            .iter()
            .map(|i| u64::from(i.qty) * u64::from(i.price_cents))
            .sum()
    }
}

/// Prix saisi (« 12.50 ») en centimes.
pub fn parse_price(s: &str) -> Result<u32, String> {
    let (euros, cents) = match s.split_once('.') {
        Some((e, c)) => (e, c),
        None => (s, "0"),
    };
    let e: u32 = euros
        .trim()
        .parse()
        .map_err(|_| format!("prix invalide : {s}"))?;
    let c: u32 = cents
        .trim()
        .parse()
        .map_err(|_| format!("prix invalide : {s}"))?;
    Ok(e * 100 + c)
}

/// Centimes en « 12,50 € ».
pub fn format_cents(cents: u64) -> String {
    format!("{},{:02} €", cents / 100, cents % 100)
}
