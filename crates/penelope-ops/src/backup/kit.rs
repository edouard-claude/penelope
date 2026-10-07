//! Le kit de secours (#328) : ce qu'il faut, hors de la machine, pour relire les archives.
//!
//! ```text
//!  penelope backup setup ─► fournisseur testé ─► phrase de passe (générée ou saisie)
//!                        ─► kit affiché une fois ─► ressaisie de 4 mots
//!  penelope backup kit ───► le kit réaffiché, sur confirmation
//! ```
//!
//! Sans la phrase de passe, une archive est illisible, pour le propriétaire comme pour
//! quiconque : le kit est la seule copie qui survit à la perte de la machine.

use super::*;

/// Mots d'une phrase générée.
pub const WORDS: usize = 6;
/// Mots redemandés pour confirmer que le kit a été noté.
pub const CONFIRM_WORDS: usize = 4;

const CONSONANTS: &[u8] = b"bdfgjklmnprstvz";
const VOWELS: &[u8] = b"aeiou";

/// Une phrase de passe robuste et tapable : six mots de trois syllabes (consonne,
/// voyelle), soit 15³ × 5³ par mot, plus de 110 bits en tout, tirés de l'aléa du système.
pub fn generate_passphrase() -> anyhow::Result<String> {
    generate_from(|n| penelope_kernel::ids::secret_token(n).map_err(|e| anyhow::anyhow!("{e}")))
}

/// [`generate_passphrase`] sur une source de jetons base62 donnée (les tests).
pub fn generate_from(
    mut token: impl FnMut(usize) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    const B62: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    // Un caractère base62 uniforme vaut un indice sur 62 ; au-delà de 60, il est écarté
    // pour que 15 consonnes et 5 voyelles restent équiprobables.
    let mut pool: Vec<usize> = Vec::new();
    let mut draw = |modulo: usize| -> anyhow::Result<usize> {
        loop {
            if let Some(i) = pool.pop() {
                if i < 60 {
                    return Ok(i % modulo);
                }
                continue;
            }
            pool = token(64)?.chars().filter_map(|c| B62.find(c)).collect();
        }
    };
    let mut words = Vec::with_capacity(WORDS);
    for _ in 0..WORDS {
        let mut w = String::with_capacity(6);
        for _ in 0..3 {
            w.push(CONSONANTS[draw(CONSONANTS.len())?] as char);
            w.push(VOWELS[draw(VOWELS.len())?] as char);
        }
        words.push(w);
    }
    Ok(words.join("-"))
}

/// Les mots d'une phrase de passe (séparés par des tirets ou des espaces).
pub fn words(passphrase: &str) -> Vec<&str> {
    passphrase
        .split(['-', ' '])
        .filter(|w| !w.is_empty())
        .collect()
}

/// Quatre positions distinctes (à partir de 1), tirées au sort et rangées, parmi les mots
/// de la phrase.
pub fn confirm_positions(count: usize) -> Vec<usize> {
    let mut all: Vec<usize> = (1..=count).collect();
    let raw = penelope_kernel::ids::short_token(32);
    let mut picked = Vec::new();
    for b in raw.bytes() {
        if picked.len() == CONFIRM_WORDS.min(count) || all.is_empty() {
            break;
        }
        picked.push(all.remove(b as usize % all.len()));
    }
    picked.sort();
    picked
}

/// Les mots ressaisis correspondent-ils à ceux des positions demandées ?
pub fn confirmed(passphrase: &str, positions: &[usize], typed: &[String]) -> bool {
    let w = words(passphrase);
    positions.len() == typed.len()
        && positions.iter().zip(typed).all(|(p, t)| {
            w.get(p.wrapping_sub(1))
                .is_some_and(|x| x.eq_ignore_ascii_case(t.trim()))
        })
}

/// Le kit, en texte à copier dans un gestionnaire de mots de passe ou à imprimer.
/// `s3_keys` : identifiant et clé secrète, quand le fournisseur est S3.
pub fn render(
    cfg: &penelope_kernel::config::Backup,
    target: &provider::Target,
    passphrase: &str,
    s3_keys: Option<(&str, &str)>,
    day: &str,
) -> String {
    let rule = "=".repeat(64);
    let mut t = format!(
        "{rule}\nKIT DE SECOURS : SAUVEGARDE DE PÉNÉLOPE (établi le {day})\n{rule}\n\
         À ranger dans un gestionnaire de mots de passe, ou à imprimer.\n\
         Sans la phrase de passe, les archives sont illisibles : personne, pas même\n\
         Pénélope, ne peut les ouvrir.\n\n\
         Phrase de passe : {passphrase}\n\
         Fournisseur     : {} ({})\n",
        target.label(),
        target.location()
    );
    let restore = match target {
        provider::Target::S3(c) => {
            if let Some((id, secret)) = s3_keys {
                t.push_str(&format!(
                    "Clé d'accès S3  : {id}\nClé secrète S3  : {secret}\n"
                ));
            }
            format!(
                "penelope restore s3://{}/{} --endpoint {} --region {}\n     \
                 (les clés S3 sont demandées à l'invite)",
                c.bucket,
                s3::normalized_prefix(&c.prefix),
                c.endpoint,
                c.region
            )
        }
        provider::Target::Dir { icloud: true, .. } => {
            let sub = match cfg.dir.trim() {
                "" | provider::ICLOUD_DEFAULT_DIR => "icloud".to_string(),
                d => format!(
                    "\"$HOME/{}/{}\"",
                    provider::ICLOUD_DRIVE,
                    d.trim_matches('/')
                ),
            };
            format!(
                "penelope restore {sub}\n     (connecté au même identifiant Apple, iCloud Drive \
                 synchronisé)"
            )
        }
        provider::Target::Dir { path, .. } => {
            format!("penelope restore {}", path.display())
        }
    };
    t.push_str(&format!(
        "\nSur un ordinateur neuf :\n  \
         1. Installer Pénélope (guide d'installation, §2), sans rien configurer.\n  \
         2. {restore}\n  \
         3. Coller la phrase de passe à l'invite : base, mémoire, configuration,\n     \
         serveurs MCP et secrets reviennent, le service démarre.\n{rule}\n"
    ));
    t
}

/// Le kit de cette instance : la configuration, la phrase de passe et les clés S3 du
/// magasin. Erreur sans phrase de passe ou sans fournisseur.
pub fn of_instance(s: &Services) -> anyhow::Result<String> {
    let cfg = s.config.config();
    let target = provider::Target::resolve(
        &cfg.backup,
        s.platform.dirs.as_ref(),
        penelope_platform::dirs::home_dir().as_deref(),
    )?;
    let passphrase = passphrase(s)?;
    let secrets = s.platform.secrets.as_ref();
    let keys = match &target {
        provider::Target::S3(c) => secrets
            .expand(&c.access_key_id)
            .ok()
            .zip(secrets.expand(&c.secret_access_key).ok()),
        _ => None,
    };
    let day: String = s.clock.now_rfc3339().chars().take(10).collect();
    Ok(render(
        &cfg.backup,
        &target,
        &passphrase,
        keys.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
        &day,
    ))
}
