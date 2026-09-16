//! HTML vers texte lisible pour `http_fetch` (issue #8) : titres, paragraphes, listes,
//! tableaux et liens en Markdown léger ; scripts, styles et balisage retirés. Une page de
//! documentation y perd en général 80 à 95 % de sa taille, sans rien perdre à la lecture.

/// Ce contenu n'est jamais du texte à lire.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "iframe", "object", "canvas", "head",
];

const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "aside",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "blockquote",
    "form",
    "figure",
    "figcaption",
    "details",
    "summary",
    "address",
    "fieldset",
    "hr",
];

/// La réponse ressemble-t-elle à une page HTML ?
pub fn looks_like_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("html") {
        return true;
    }
    if !ct.is_empty() && !ct.starts_with("text/plain") {
        return false;
    }
    let head: String = body
        .trim_start()
        .chars()
        .take(200)
        .collect::<String>()
        .to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

/// Convertit une page en texte. `base` sert à rendre les liens relatifs absolus.
pub fn to_text(html: &str, base: Option<&url::Url>) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let title = extract_title(html);
    if let Some(t) = &title {
        out.push_str("# ");
        out.push_str(t);
        out.push_str("\n\n");
    }

    let lower = html.to_ascii_lowercase();
    let mut pre = 0usize;
    let mut links: Vec<(usize, Option<String>)> = Vec::new();
    let mut i = 0usize;
    while i < html.len() {
        let Some(offset) = html[i..].find('<') else {
            push_text(&mut out, &html[i..], pre > 0);
            break;
        };
        push_text(&mut out, &html[i..i + offset], pre > 0);
        i += offset;
        if html[i..].starts_with("<!--") {
            i = html[i..]
                .find("-->")
                .map(|e| i + e + 3)
                .unwrap_or(html.len());
            continue;
        }
        let Some(close) = html[i..].find('>') else {
            break;
        };
        let tag = &html[i + 1..i + close];
        i += close + 1;
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if !closing && SKIPPED.contains(&name.as_str()) && !tag.ends_with('/') {
            let end_tag = format!("</{name}");
            i = match lower[i..].find(&end_tag) {
                Some(e) => {
                    let after = i + e;
                    lower[after..]
                        .find('>')
                        .map(|g| after + g + 1)
                        .unwrap_or(html.len())
                }
                None => html.len(),
            };
            continue;
        }
        match (name.as_str(), closing) {
            ("br", _) => out.push('\n'),
            ("li", false) => {
                ensure_newline(&mut out);
                out.push_str("- ");
            }
            ("tr", _) => ensure_newline(&mut out),
            ("td" | "th", true) => out.push_str(" | "),
            ("pre", false) => {
                ensure_blank_line(&mut out);
                out.push_str("```\n");
                pre += 1;
            }
            ("pre", true) => {
                pre = pre.saturating_sub(1);
                ensure_newline(&mut out);
                out.push_str("```\n\n");
            }
            (h, false) if is_heading(h) => {
                ensure_blank_line(&mut out);
                let level = h[1..].parse::<usize>().unwrap_or(1).clamp(1, 6);
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            (h, true) if is_heading(h) => out.push_str("\n\n"),
            ("a", false) => {
                let href = attribute(tag, "href").and_then(|h| resolve(&h, base));
                links.push((out.len(), href));
            }
            ("a", true) => {
                if let Some((start, Some(href))) = links.pop() {
                    // Des espaces ont pu être retirés depuis : revenir à une frontière.
                    let mut start = start.min(out.len());
                    while !out.is_char_boundary(start) {
                        start -= 1;
                    }
                    let text = out[start..].trim().to_string();
                    if !text.is_empty() && text != href {
                        out.truncate(start);
                        out.push_str(&format!("[{text}]({href})"));
                    }
                }
            }
            (b, _) if BLOCKS.contains(&b) => ensure_newline(&mut out),
            _ => {}
        }
    }
    tidy(&out)
}

fn is_heading(name: &str) -> bool {
    name.len() == 2 && name.starts_with('h') && name[1..].chars().all(|c| ('1'..='6').contains(&c))
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let end = lower[open_end..].find("</title")? + open_end;
    let title = collapse(&decode_entities(&html[open_end..end]));
    (!title.is_empty()).then_some(title)
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let at = from + pos;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - rest.len() + 1;
        let value = tag[value_start..].trim_start();
        let v = match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode_entities(v));
    }
    None
}

fn resolve(href: &str, base: Option<&url::Url>) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    let joined = base?.join(href).ok()?;
    matches!(joined.scheme(), "http" | "https").then(|| joined.to_string())
}

fn push_text(out: &mut String, raw: &str, preformatted: bool) {
    if raw.is_empty() {
        return;
    }
    let text = decode_entities(raw);
    if preformatted {
        out.push_str(&text);
        return;
    }
    let collapsed = collapse(&text);
    if collapsed.is_empty() {
        if raw.chars().any(char::is_whitespace) && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
        return;
    }
    let lead = text.starts_with(char::is_whitespace);
    if lead && !out.ends_with([' ', '\n']) && !out.is_empty() {
        out.push(' ');
    }
    out.push_str(&collapsed);
    if text.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ensure_newline(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn ensure_blank_line(out: &mut String) {
    ensure_newline(out);
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Lignes nettoyées, au plus une ligne vide d'affilée (hors blocs de code).
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    let mut in_code = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        let line = if in_code {
            line.trim_end()
        } else {
            line.trim()
                .trim_end_matches(" |")
                .trim_end_matches('|')
                .trim()
        };
        if line.is_empty() && !in_code {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Entités nommées courantes et numériques ; une entité inconnue reste telle quelle.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[1..].find(';').map(|p| p + 1).filter(|p| *p <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = if let Some(num) = entity.strip_prefix('#') {
            let code = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => num.parse::<u32>().ok(),
            };
            code.and_then(char::from_u32).map(String::from)
        } else {
            named_entity(entity).map(String::from)
        };
        match decoded {
            Some(d) => {
                out.push_str(&d);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "laquo" => "«",
        "raquo" => "»",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "bull" => "•",
        "middot" => "·",
        "times" => "×",
        "deg" => "°",
        "euro" => "€",
        "eacute" => "é",
        "egrave" => "è",
        "ecirc" => "ê",
        "euml" => "ë",
        "agrave" => "à",
        "acirc" => "â",
        "ccedil" => "ç",
        "icirc" => "î",
        "iuml" => "ï",
        "ocirc" => "ô",
        "ouml" => "ö",
        "ugrave" => "ù",
        "ucirc" => "û",
        "uuml" => "ü",
        "auml" => "ä",
        "szlig" => "ß",
        "Eacute" => "É",
        "Agrave" => "À",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<!DOCTYPE html>
<html><head><title>API &amp; guide</title>
<style>body { color: red }</style>
<script>window.track = function() { return "<p>faux</p>"; };</script>
</head>
<body>
<nav><ul><li><a href="/docs">Docs</a></li><li><a href="#top">Haut</a></li></ul></nav>
<!-- commentaire -->
<main>
<h1>Authentification</h1>
<p>Chaque requête porte un <code>Bearer</code>&nbsp;jeton.
   Voir <a href="https://exemple.fr/jetons">les jetons</a>.</p>
<h2>Exemple</h2>
<pre>curl -H "Authorization: Bearer x"
  https://api.exemple.fr/v1</pre>
<table><tr><th>Code</th><th>Sens</th></tr><tr><td>401</td><td>jeton absent</td></tr></table>
<p>Fin&hellip;<br>Ligne&#x21;</p>
</main>
</body></html>"##;

    #[test]
    fn a_page_becomes_readable_text() {
        let base = url::Url::parse("https://exemple.fr/guide/").unwrap();
        let text = to_text(PAGE, Some(&base));
        assert!(text.starts_with("# API & guide"), "{text}");
        assert!(!text.contains("track"), "script retiré : {text}");
        assert!(!text.contains("color"), "style retiré : {text}");
        assert!(!text.contains("commentaire"), "{text}");
        assert!(text.contains("- [Docs](https://exemple.fr/docs)"), "{text}");
        assert!(text.contains("- Haut"), "ancre sans lien : {text}");
        assert!(text.contains("# Authentification"), "{text}");
        assert!(
            text.contains("Chaque requête porte un Bearer jeton. Voir [les jetons](https://exemple.fr/jetons)."),
            "{text}"
        );
        assert!(text.contains("## Exemple"), "{text}");
        assert!(
            text.contains(
                "```\ncurl -H \"Authorization: Bearer x\"\n  https://api.exemple.fr/v1\n```"
            ),
            "préformaté gardé : {text}"
        );
        assert!(text.contains("Code | Sens"), "{text}");
        assert!(text.contains("401 | jeton absent"), "{text}");
        assert!(text.contains("Fin…\nLigne!"), "{text}");
        assert!(
            text.len() < PAGE.len() / 2,
            "{} / {}",
            text.len(),
            PAGE.len()
        );
    }

    #[test]
    fn links_around_blocks_never_split_a_character() {
        let text = to_text(
            "<p>ligne</p> <a href=\"https://x.fr\"><div>été</div></a>",
            None,
        );
        assert!(text.contains("été"), "{text}");
    }

    #[test]
    fn html_is_recognised_by_type_or_by_its_first_bytes() {
        assert!(looks_like_html("text/html; charset=utf-8", ""));
        assert!(looks_like_html("", "  <!doctype html><html>"));
        assert!(!looks_like_html("application/json", "<html>"));
        assert!(!looks_like_html("text/plain", "bonjour"));
        assert_eq!(
            decode_entities("a &amp;&amp; b &unknown; &#233;"),
            "a && b &unknown; é"
        );
    }
}
