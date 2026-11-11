//! Entités XML et HTML du texte d'un document ingéré.

/// Une entité tient en `&`, huit caractères au plus et `;` (`&#x1F600;`, `&hellip;`).
/// Au-delà, l'esperluette est du texte.
const WINDOW: usize = 12;

/// Décode les entités numériques et les entités nommées courantes ; une esperluette
/// seule ou une entité inconnue restent telles quelles.
pub(super) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // La fenêtre se compte en octets sur les octets : la couper dans la chaîne
        // tombait au milieu d'un caractère accentué et paniquait (« R&D à Montréal »,
        // #452). `;` est ASCII, sa position est toujours une frontière.
        let semi = tail.bytes().take(WINDOW).position(|b| b == b';');
        match semi.and_then(|semi| Some((semi, decode(&tail[1..semi])?))) {
            Some((semi, c)) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode(entity: &str) -> Option<char> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    if let Some(dec) = entity.strip_prefix('#') {
        return dec.parse::<u32>().ok().and_then(char::from_u32);
    }
    NAMED
        .iter()
        .find(|(name, _)| *name == entity)
        .map(|(_, c)| *c)
}

/// Entités nommées : celles de XML, puis les lettres accentuées et la ponctuation qu'un
/// export HTML en français écrit ainsi. Les autres restent en clair dans le texte.
const NAMED: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", ' '),
    ("agrave", 'à'),
    ("acirc", 'â'),
    ("auml", 'ä'),
    ("aelig", 'æ'),
    ("ccedil", 'ç'),
    ("eacute", 'é'),
    ("egrave", 'è'),
    ("ecirc", 'ê'),
    ("euml", 'ë'),
    ("icirc", 'î'),
    ("iuml", 'ï'),
    ("ntilde", 'ñ'),
    ("ocirc", 'ô'),
    ("ouml", 'ö'),
    ("oelig", 'œ'),
    ("ugrave", 'ù'),
    ("ucirc", 'û'),
    ("uuml", 'ü'),
    ("yuml", 'ÿ'),
    ("szlig", 'ß'),
    ("Agrave", 'À'),
    ("Acirc", 'Â'),
    ("AElig", 'Æ'),
    ("Ccedil", 'Ç'),
    ("Eacute", 'É'),
    ("Egrave", 'È'),
    ("Ecirc", 'Ê'),
    ("Icirc", 'Î'),
    ("Ocirc", 'Ô'),
    ("OElig", 'Œ'),
    ("Ugrave", 'Ù'),
    ("Ucirc", 'Û'),
    ("laquo", '«'),
    ("raquo", '»'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("hellip", '…'),
    ("ndash", '–'),
    ("mdash", '—'),
    ("euro", '€'),
    ("pound", '£'),
    ("deg", '°'),
    ("copy", '©'),
    ("reg", '®'),
    ("trade", '™'),
    ("times", '×'),
    ("middot", '·'),
    ("bull", '•'),
    ("sect", '§'),
];
