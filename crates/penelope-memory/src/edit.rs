//! Éditions ciblées des fichiers du vault (§6.8, application des opérations).
//!
//! Le fichier n'est jamais réécrit en bloc à partir d'une sortie de modèle : chaque
//! opération touche **une ligne** repérée par son `uid`, ou ajoute une ligne sous une
//! section. Le reste du fichier, y compris ce qu'un humain y a écrit, est conservé octet
//! pour octet.

use crate::vault::Annotations;

/// Ligne d'entrée : `- texte <!-- importance: … --> … ^uid`.
pub fn entry_line(text: &str, annotations: &Annotations) -> String {
    let text = text.replace(['\n', '\r'], " ");
    let rendered = annotations.render();
    if rendered.is_empty() {
        format!("- {}", text.trim())
    } else {
        format!("- {} {rendered}", text.trim())
    }
}

/// Ajoute une ligne sous `## section` (créée en fin de fichier si absente), ou en fin de
/// fichier sans section. Un fichier vide reçoit d'abord son titre.
pub fn append_entry(raw: &str, title: &str, section: Option<&str>, line: &str) -> String {
    let mut body = if raw.trim().is_empty() {
        format!("{title}\n")
    } else {
        raw.replace("\r\n", "\n")
    };
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let Some(section) = section.map(str::trim).filter(|s| !s.is_empty()) else {
        body.push_str(line);
        body.push('\n');
        return body;
    };
    let lines: Vec<&str> = body.lines().collect();
    let header = format!("## {section}");
    let Some(start) = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&header))
    else {
        if !body.ends_with("\n\n") {
            body.push('\n');
        }
        body.push_str(&format!("{header}\n{line}\n"));
        return body;
    };
    // Fin de section : le prochain titre, ou la fin du fichier ; la ligne s'insère après
    // la dernière ligne non vide de la section.
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with("## ") || l.trim_start().starts_with("# "))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    let mut insert_at = end;
    while insert_at > start + 1 && lines[insert_at - 1].trim().is_empty() {
        insert_at -= 1;
    }
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(insert_at, line.to_string());
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Index de la ligne portant `uid` : identifiant de bloc final, identifiant seul sur sa
/// ligne (encadré), ou ancien commentaire `<!-- uid: … -->`.
fn find_line(lines: &[&str], uid: &str) -> Option<usize> {
    lines.iter().position(|l| {
        crate::vault::standalone_block_id(l).as_deref() == Some(uid)
            || crate::vault::line_uid(l).as_deref() == Some(uid)
    })
}

/// Lignes d'un encadré désigné par un identifiant seul en ligne `i` : de la première ligne
/// `>` jusqu'à l'identifiant.
fn callout_span(lines: &[&str], i: usize) -> Option<std::ops::RangeInclusive<usize>> {
    crate::vault::standalone_block_id(lines[i])?;
    let mut start = i;
    while start > 0 && lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let mut first = start;
    while first > 0 && lines[first - 1].trim_start().starts_with('>') {
        first -= 1;
    }
    Some(if first < start { first..=i } else { i..=i })
}

fn rebuild(lines: Vec<String>, trailing_newline: bool) -> String {
    let mut s = lines.join("\n");
    if trailing_newline {
        s.push('\n');
    }
    s
}

/// Lignes qui portent l'entrée `uid` (l'encadré entier pour un identifiant seul), pour
/// détecter qu'une autre main l'a modifiée entre la lecture et l'écriture.
pub fn line_of(raw: &str, uid: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let span = callout_span(&lines, i).unwrap_or(i..=i);
    Some(lines[span].join("\n"))
}

/// Remplace le texte d'une entrée, annotations conservées. `None` : uid absent.
pub fn replace_entry_text(raw: &str, uid: &str, text: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let original = lines[i];
    if crate::vault::standalone_block_id(original).is_some() {
        // Un encadré se réécrit à la main : son texte n'est pas une ligne.
        return None;
    }
    let indent: String = original.chars().take_while(|c| c.is_whitespace()).collect();
    let bullet = if original.trim_start().starts_with("* ") {
        "* "
    } else {
        "- "
    };
    let annotations = Annotations::parse(original);
    let mut new_line = entry_line(text, &annotations);
    new_line.replace_range(..2, bullet);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out[i] = format!("{indent}{new_line}");
    Some(rebuild(out, raw.ends_with('\n')))
}

/// Modifie les annotations d'une entrée, texte conservé. `None` : uid absent ou encadré.
pub fn update_annotations(
    raw: &str,
    uid: &str,
    f: impl FnOnce(&mut Annotations),
) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let original = lines[i];
    if crate::vault::standalone_block_id(original).is_some() {
        return None;
    }
    let indent: String = original.chars().take_while(|c| c.is_whitespace()).collect();
    let bullet = if original.trim_start().starts_with("* ") {
        "* "
    } else {
        "- "
    };
    let mut annotations = Annotations::parse(original);
    f(&mut annotations);
    let text = crate::vault::strip_annotations(original.trim_start())
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .to_string();
    let mut new_line = entry_line(&text, &annotations);
    new_line.replace_range(..2, bullet);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out[i] = format!("{indent}{new_line}");
    Some(rebuild(out, raw.ends_with('\n')))
}

/// Retire la ligne d'une entrée. `None` : uid absent.
pub fn remove_entry(raw: &str, uid: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let span = callout_span(&lines, i).unwrap_or(i..=i);
    let out: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(j, _)| !span.contains(j))
        .map(|(_, l)| l.to_string())
        .collect();
    Some(rebuild(out, raw.ends_with('\n')))
}

/// Remplace une entrée entière (texte et annotations) par `line`, à la même place : une
/// entrée qui en remplace une autre (`supersede`, issue #37).
pub fn replace_entry_line(raw: &str, uid: &str, line: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let span = callout_span(&lines, i).unwrap_or(i..=i);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for (j, l) in lines.iter().enumerate() {
        if j == *span.start() {
            out.push(line.to_string());
        } else if !span.contains(&j) {
            out.push(l.to_string());
        }
    }
    Some(rebuild(out, raw.ends_with('\n')))
}

/// Ajoute un lien `[[slug]]` au texte d'une entrée (idempotent).
pub fn link_entry(raw: &str, uid: &str, slug: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    let link = format!("[[{}]]", slug.trim());
    if lines[i].contains(&link) {
        return Some(raw.to_string());
    }
    let text = crate::vault::strip_annotations(lines[i]);
    let text = text
        .trim_start()
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .to_string();
    replace_entry_text(raw, uid, &format!("{text} {link}"))
}

/// Texte d'une entrée, sans puce ni annotations.
pub fn entry_text(raw: &str, uid: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let i = find_line(&lines, uid)?;
    Some(
        crate::vault::strip_annotations(lines[i])
            .trim_start()
            .trim_start_matches("- ")
            .trim_start_matches("* ")
            .trim()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "---\ntype: coeur\n---\n# Mémoire de fond\n\n## Faits\n- Le serveur est à Paris <!-- uid: A1 --> <!-- importance: 7 -->\n- Écrit à la main, sans uid\n\n## Clients\n- ACME paie à 30 jours <!-- uid: B2 -->\n";

    #[test]
    fn appending_goes_under_its_section_and_keeps_the_rest() {
        let a = Annotations {
            uid: Some("C3".into()),
            ..Default::default()
        };
        let out = append_entry(
            RAW,
            "# Mémoire de fond",
            Some("Faits"),
            &entry_line("La prod tourne sous Debian", &a),
        );
        let lines: Vec<&str> = out.lines().collect();
        let faits = lines.iter().position(|l| *l == "## Faits").unwrap();
        let clients = lines.iter().position(|l| *l == "## Clients").unwrap();
        let added = lines.iter().position(|l| l.ends_with("^C3")).unwrap();
        assert!(faits < added && added < clients, "{out}");
        assert!(
            out.contains("- Écrit à la main, sans uid"),
            "le texte humain reste"
        );

        let new_section = append_entry(RAW, "# Mémoire de fond", Some("Outils"), "- git");
        assert!(new_section.ends_with("## Outils\n- git\n"), "{new_section}");
        assert_eq!(append_entry("", "# Profil", None, "- x"), "# Profil\n- x\n");
    }

    #[test]
    fn replacing_removing_and_linking_touch_one_line() {
        let replaced = replace_entry_text(RAW, "A1", "Le serveur est à Lyon").unwrap();
        assert!(
            replaced.contains("- Le serveur est à Lyon <!-- importance: 7 --> ^A1"),
            "ancienne ligne réécrite avec son identifiant de bloc : {replaced}"
        );
        assert!(replaced.contains("ACME paie à 30 jours"));

        let removed = remove_entry(RAW, "B2").unwrap();
        assert!(!removed.contains("ACME"));
        assert!(removed.contains("## Clients"));

        let linked = link_entry(RAW, "B2", "acme").unwrap();
        assert!(
            linked.contains("- ACME paie à 30 jours [[acme]] ^B2"),
            "{linked}"
        );
        assert_eq!(
            link_entry(&linked, "B2", "acme").unwrap(),
            linked,
            "idempotent"
        );

        assert_eq!(
            entry_text(RAW, "A1").as_deref(),
            Some("Le serveur est à Paris")
        );
        assert!(replace_entry_text(RAW, "ZZ", "x").is_none());
        assert!(
            remove_entry(RAW, "A").is_none(),
            "un préfixe d'uid ne suffit pas"
        );
    }

    #[test]
    fn a_callout_entry_is_removed_whole() {
        let raw =
            "# Journal\n\n- avant ^A\n\n> [!abstract] Épisode 1\n> résumé\n\n^EP1\n\n- après ^B\n";
        let removed = remove_entry(raw, "EP1").unwrap();
        assert_eq!(removed, "# Journal\n\n- avant ^A\n\n\n- après ^B\n");
        assert!(replace_entry_text(raw, "EP1", "x").is_none());
        assert!(
            replace_entry_text(raw, "B", "après tout")
                .unwrap()
                .contains("- après tout ^B")
        );
    }

    #[test]
    fn a_whole_entry_is_replaced_in_place() {
        let raw = "# M\n- avant <!-- importance: 5 --> ^01A\n- autre ^01B\n";
        let out = replace_entry_line(raw, "01A", "- après <!-- remplace: 01A --> ^01C").unwrap();
        assert_eq!(
            out,
            "# M\n- après <!-- remplace: 01A --> ^01C\n- autre ^01B\n"
        );
        assert!(replace_entry_line(raw, "01Z", "- x").is_none());
    }

    /// Les annotations changent, le texte, la puce et l'indentation restent ; un uid
    /// absent ne réécrit rien.
    #[test]
    fn updating_annotations_keeps_the_text() {
        let out = update_annotations(RAW, "A1", |a| a.importance = Some(9)).unwrap();
        let line = out.lines().find(|l| l.contains("A1")).unwrap();
        assert!(line.starts_with("- Le serveur est à Paris"), "{line}");
        assert!(line.contains("importance: 9"), "{line}");
        assert!(!line.contains("importance: 7"), "{line}");
        assert!(out.contains("ACME paie à 30 jours"));
        assert_eq!(update_annotations(RAW, "Z9", |_| {}), None);

        let starred = "# M\n  * Sous-point <!-- uid: S1 -->\n";
        let out =
            update_annotations(starred, "S1", |a| a.projet = Some("penelope".into())).unwrap();
        assert!(out.contains("  * Sous-point"), "{out}");
        assert!(out.contains("penelope"), "{out}");
    }
}
