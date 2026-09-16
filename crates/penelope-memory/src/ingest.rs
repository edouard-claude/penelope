//! Ingestion de documents (§6.13) : extraction du texte, fiche source, passages indexés.
//!
//! Un document envoyé sur Telegram ou déposé dans `vault/inbox/` devient une fiche
//! `vault/sources/<slug>.md`, d'origine `untrusted` sauf déclaration `/mien`. Son texte
//! est découpé en passages indexés sous le type [`SOURCE_ETYPE`] : retrouvables par une
//! recherche explicite, jamais rappelés automatiquement.
//!
//! Tout est en Rust et borné : un PDF piégé ne fait ni exploser la mémoire (limite de
//! décompression par page) ni tomber le daemon (panique rattrapée).

use crate::index::IndexedEntry;
use crate::provenance::Origin;
use crate::vault::Level;
use penelope_kernel::frontmatter::{self, FmValue};
use std::collections::BTreeMap;
use std::io::Read;

/// Type d'entrée des passages de documents.
pub const SOURCE_ETYPE: &str = "source";
/// Répertoire des fiches, relatif au vault.
pub const SOURCES_DIR: &str = "sources";
/// Répertoire de dépôt surveillé, relatif au vault.
pub const INBOX_DIR: &str = "inbox";
/// Formats ingérés dans le vault ; les autres restent des pièces jointes.
pub const INGESTIBLE: &[&str] = &["pdf", "docx", "html", "htm", "md", "markdown", "txt"];
/// Plafond du texte conservé par document, en caractères.
pub const MAX_TEXT_CHARS: usize = 2_000_000;
/// Taille visée d'un passage indexé, en caractères.
pub const PASSAGE_CHARS: usize = 1_200;
/// Titre de la section qui porte le texte extrait.
pub const CONTENT_HEADER: &str = "## Contenu";
/// Titre de la section du résumé.
pub const SUMMARY_HEADER: &str = "## Résumé";

/// Décompression maximale d'une page PDF ou du corps d'un DOCX.
const DECOMPRESSED_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Texte extrait d'un document.
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    /// `pdf`, `docx`, `html` ou `texte`.
    pub format: &'static str,
    pub text: String,
    pub pages: Option<usize>,
    /// Pages sans texte lisible : scan, police sans table Unicode.
    pub unreadable_pages: usize,
    /// Le texte dépassait [`MAX_TEXT_CHARS`].
    pub truncated: bool,
}

/// Extension en minuscules, sans le point.
pub fn extension(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_ingestible(name: &str) -> bool {
    INGESTIBLE.contains(&extension(name).as_str())
}

/// Extrait le texte d'un document d'après son extension.
pub fn extract(name: &str, bytes: &[u8]) -> Result<Extracted, String> {
    let ext = extension(name);
    let raw = match ext.as_str() {
        "pdf" => std::panic::catch_unwind(|| extract_pdf(bytes))
            .map_err(|_| "PDF malformé : l'extraction a échoué".to_string())??,
        "docx" => extract_docx(bytes)?,
        "html" | "htm" => Extracted {
            format: "html",
            text: html_to_text(&String::from_utf8_lossy(bytes)),
            pages: None,
            unreadable_pages: 0,
            truncated: false,
        },
        "md" | "markdown" | "txt" => Extracted {
            format: "texte",
            text: String::from_utf8_lossy(bytes)
                .trim_start_matches('\u{feff}')
                .to_string(),
            pages: None,
            unreadable_pages: 0,
            truncated: false,
        },
        other => {
            return Err(format!(
                "format `{}` non ingéré (formats : {})",
                if other.is_empty() {
                    "sans extension"
                } else {
                    other
                },
                INGESTIBLE.join(", ")
            ));
        }
    };
    let mut text = normalise(&raw.text);
    let mut truncated = raw.truncated;
    if text.chars().count() > MAX_TEXT_CHARS {
        text = text.chars().take(MAX_TEXT_CHARS).collect();
        truncated = true;
    }
    if text.trim().is_empty() {
        return Err(match raw.format {
            "pdf" => "aucun texte lisible dans ce PDF (document scanné ?)".into(),
            _ => "le document ne contient aucun texte".into(),
        });
    }
    Ok(Extracted {
        text,
        truncated,
        ..raw
    })
}

fn extract_pdf(bytes: &[u8]) -> Result<Extracted, String> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| format!("PDF illisible : {e}"))?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut text = String::new();
    let mut unreadable = 0usize;
    let mut truncated = false;
    for page in &pages {
        let mut page_text = String::new();
        for t in doc
            .extract_text_chunks_with_limit(&[*page], DECOMPRESSED_MAX_BYTES)
            .into_iter()
            .flatten()
        {
            page_text.push_str(&t);
        }
        if page_text.trim().is_empty() {
            unreadable += 1;
            continue;
        }
        text.push_str(page_text.trim_end());
        text.push_str("\n\n");
        if text.len() > MAX_TEXT_CHARS * 4 {
            truncated = true;
            break;
        }
    }
    Ok(Extracted {
        format: "pdf",
        text,
        pages: Some(pages.len()),
        unreadable_pages: unreadable,
        truncated,
    })
}

fn extract_docx(bytes: &[u8]) -> Result<Extracted, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("DOCX illisible : {e}"))?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| "DOCX sans corps de document (word/document.xml)".to_string())?
        .take(DECOMPRESSED_MAX_BYTES as u64)
        .read_to_string(&mut xml)
        .map_err(|e| format!("DOCX illisible : {e}"))?;
    Ok(Extracted {
        format: "docx",
        text: docx_xml_to_text(&xml),
        pages: None,
        unreadable_pages: 0,
        truncated: false,
    })
}

/// Texte du corps d'un DOCX : paragraphes, tabulations et sauts de ligne conservés.
pub fn docx_xml_to_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        if in_text {
            out.push_str(&decode_entities(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        match name {
            "w:t" => in_text = !tag.ends_with('/'),
            "/w:t" => in_text = false,
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            "/w:p" => out.push('\n'),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out
}

/// Texte lisible d'une page HTML : scripts, styles et en-tête retirés, blocs séparés.
pub fn html_to_text(html: &str) -> String {
    const SKIPPED: &[&str] = &["script", "style", "head", "noscript", "svg", "template"];
    const BLOCKS: &[&str] = &[
        "p",
        "div",
        "br",
        "li",
        "tr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "header",
        "footer",
        "blockquote",
        "pre",
        "table",
        "ul",
        "ol",
        "hr",
    ];
    let mut out = String::new();
    let mut skipping: Option<String> = None;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        if skipping.is_none() {
            out.push_str(&decode_entities(&rest[..lt]));
        }
        // Commentaire : jusqu'à `-->`.
        if rest[lt..].starts_with("<!--") {
            match rest[lt..].find("-->") {
                Some(end) => {
                    rest = &rest[lt + end + 3..];
                    continue;
                }
                None => break,
            }
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        match &skipping {
            Some(skipped) if closing && *skipped == name => skipping = None,
            Some(_) => {}
            None if !closing && SKIPPED.contains(&name.as_str()) && !tag.ends_with('/') => {
                skipping = Some(name)
            }
            None if BLOCKS.contains(&name.as_str()) => out.push('\n'),
            None => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    if skipping.is_none() {
        out.push_str(&decode_entities(rest));
    }
    out
}

/// Entités XML et HTML courantes.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
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

/// Espaces de fin retirés, lignes vides en série réduites à une seule.
fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0usize;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            out.push('\n');
            continue;
        }
        blank = 0;
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Identifiant de fichier lisible : minuscules ASCII, tirets, 60 caractères au plus.
pub fn slugify(name: &str) -> String {
    let stem = std::path::Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let mut out = String::new();
    for c in stem.to_lowercase().chars() {
        let folded = match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => "a",
            'ç' => "c",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'î' | 'ï' | 'í' | 'ì' => "i",
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' => "o",
            'ù' | 'û' | 'ü' | 'ú' => "u",
            'ÿ' => "y",
            'ñ' => "n",
            'œ' => "oe",
            'æ' => "ae",
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => "-",
        };
        out.push_str(folded);
    }
    let mut slug = String::new();
    for part in out.split('-').filter(|p| !p.is_empty()) {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(part);
    }
    let slug: String = slug.chars().take(60).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "document".into()
    } else {
        slug
    }
}

/// Métadonnées d'une fiche source.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMeta {
    pub titre: String,
    /// Nom du fichier d'origine.
    pub fichier: String,
    /// `telegram` ou `inbox`.
    pub canal: String,
    pub origine: Origin,
    /// Horodatage RFC 3339 de la réception.
    pub recu: String,
    pub sha256: String,
    pub format: String,
    pub pages: Option<usize>,
    pub caracteres: usize,
}

/// Une valeur de frontmatter tient sur une ligne et ne passe pas pour une liste.
fn fm_scalar(s: &str) -> FmValue {
    let one_line = s.replace(['\n', '\r'], " ");
    FmValue::Str(one_line.trim().trim_start_matches('[').to_string())
}

/// Rend une fiche `vault/sources/<slug>.md`.
pub fn render_source(meta: &SourceMeta, summary: Option<&str>, text: &str) -> String {
    let mut fields = BTreeMap::new();
    fields.insert("type".to_string(), FmValue::Str("source".into()));
    fields.insert("titre".to_string(), fm_scalar(&meta.titre));
    fields.insert("fichier".to_string(), fm_scalar(&meta.fichier));
    fields.insert("canal".to_string(), fm_scalar(&meta.canal));
    fields.insert(
        "origine".to_string(),
        FmValue::Str(meta.origine.as_str().into()),
    );
    fields.insert("recu".to_string(), fm_scalar(&meta.recu));
    fields.insert("sha256".to_string(), FmValue::Str(meta.sha256.clone()));
    fields.insert("format".to_string(), fm_scalar(&meta.format));
    if let Some(p) = meta.pages {
        fields.insert("pages".to_string(), FmValue::Num(p as f64));
    }
    fields.insert(
        "caracteres".to_string(),
        FmValue::Num(meta.caracteres as f64),
    );
    let mut body = format!("# {}\n\n", meta.titre.replace('\n', " "));
    if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
        body.push_str(&format!("{SUMMARY_HEADER}\n\n{}\n\n", s.trim()));
    }
    body.push_str(&format!("{CONTENT_HEADER}\n\n{}\n", text.trim()));
    frontmatter::render(&fields, &body)
}

/// Fiche relue : origine déclarée, titre, texte extrait.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSource {
    pub titre: String,
    pub origine: Origin,
    pub sha256: Option<String>,
    pub text: String,
}

/// Relit une fiche source. Une fiche sans `origine` reconnue reste non fiable.
pub fn parse_source(raw: &str) -> Option<ParsedSource> {
    let fm = frontmatter::parse(raw).ok()?;
    if fm.str("type") != Some("source") {
        return None;
    }
    let origine = fm
        .str("origine")
        .and_then(Origin::parse)
        .filter(|o| *o == Origin::Owner)
        .unwrap_or(Origin::Untrusted);
    let text = match fm.body.find(CONTENT_HEADER) {
        Some(i) => fm.body[i + CONTENT_HEADER.len()..].trim().to_string(),
        None => fm.body.trim().to_string(),
    };
    Some(ParsedSource {
        titre: fm.string("titre"),
        origine,
        sha256: fm.str("sha256").map(String::from),
        text,
    })
}

/// Découpe un texte en passages d'environ `target` caractères, aux frontières de
/// paragraphe ; un paragraphe trop long est coupé sur des lignes, puis sur des mots.
pub fn passages(text: &str, target: usize) -> Vec<String> {
    let target = target.max(100);
    let mut out = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, out: &mut Vec<String>| {
        let t = current.trim();
        if !t.is_empty() {
            out.push(t.to_string());
        }
        current.clear();
    };
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if current.chars().count() + para.chars().count() > target && !current.is_empty() {
            flush(&mut current, &mut out);
        }
        if para.chars().count() <= target {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
            continue;
        }
        // Paragraphe plus long que la cible : mots regroupés sous la cible.
        for word in para.split_whitespace() {
            if current.chars().count() + word.chars().count() + 1 > target && !current.is_empty() {
                flush(&mut current, &mut out);
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        flush(&mut current, &mut out);
    }
    flush(&mut current, &mut out);
    out
}

/// Entrées d'index des passages d'une fiche. Les uid sont stables : réindexer une même
/// fiche remplace ses passages au lieu de les dupliquer.
pub fn source_entries(slug: &str, text: &str, maj: &str) -> Vec<IndexedEntry> {
    let file = format!("{SOURCES_DIR}/{slug}.md");
    passages(text, PASSAGE_CHARS)
        .into_iter()
        .enumerate()
        .map(|(i, passage)| IndexedEntry {
            uid: format!("src-{slug}-{:04}", i + 1),
            file: file.clone(),
            anchor: Some(format!("passage {}", i + 1)),
            level: Level::Cure,
            etype: SOURCE_ETYPE.into(),
            slug: Some(slug.to_string()),
            content_hash: penelope_kernel::canonical::sha256_hex(passage.as_bytes()),
            text: passage,
            quand: None,
            importance: None,
            projet: None,
            confiance: None,
            statut: "active".into(),
            depuis: None,
            maj: maj.to_string(),
            pinned: false,
            declencheurs: Vec::new(),
            retired_at: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PDF minimal d'une page, écrit avec lopdf.
    fn pdf_with_text(lines: &[&str]) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![50.into(), 700.into()]),
        ];
        for l in lines {
            operations.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
            operations.push(Operation::new("Td", vec![0.into(), (-20).into()]));
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn docx_with(document_xml: &str) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            z.start_file("word/document.xml", opts).unwrap();
            z.write_all(document_xml.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn pdf_text_is_extracted_page_by_page() {
        let bytes = pdf_with_text(&["Facture 2026-114", "Total TTC 1234 EUR"]);
        let e = extract("facture.pdf", &bytes).unwrap();
        assert_eq!(e.format, "pdf");
        assert_eq!(e.pages, Some(1));
        assert!(e.text.contains("Facture 2026-114"), "{:?}", e.text);
        assert!(e.text.contains("1234 EUR"), "{:?}", e.text);
        assert_eq!(e.unreadable_pages, 0);
    }

    #[test]
    fn garbage_pdf_is_an_error_not_a_panic() {
        assert!(extract("x.pdf", b"%PDF-1.4 pas vraiment un pdf").is_err());
        assert!(extract("x.pdf", &[0u8; 64]).is_err());
    }

    #[test]
    fn docx_paragraphs_tabs_and_entities_survive() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body>
            <w:p><w:r><w:t>Compte rendu</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Budget :</w:t><w:tab/><w:t>12 &amp; 13 k&#8364;</w:t></w:r></w:p>
            <w:p><w:r><w:t>Ligne</w:t><w:br/><w:t>suivante</w:t></w:r></w:p>
            </w:body></w:document>"#;
        let e = extract("cr.docx", &docx_with(xml)).unwrap();
        assert_eq!(e.format, "docx");
        let lines: Vec<&str> = e.text.lines().collect();
        assert_eq!(lines[0], "Compte rendu");
        assert_eq!(lines[1], "Budget :\t12 & 13 k€");
        assert_eq!(&lines[2..4], &["Ligne", "suivante"]);
        assert!(extract("x.docx", b"PK pas un zip").is_err());
    }

    #[test]
    fn html_keeps_text_and_drops_scripts() {
        let html = "<html><head><title>T</title><style>p{}</style></head><body>\
                    <h1>Titre</h1><p>Un &amp; deux</p><script>alert('x')</script>\
                    <!-- caché --><ul><li>a</li><li>b</li></ul></body></html>";
        let e = extract("page.html", html.as_bytes()).unwrap();
        assert!(e.text.contains("Titre"));
        assert!(e.text.contains("Un & deux"));
        assert!(!e.text.contains("alert"));
        assert!(!e.text.contains("caché"));
        assert!(!e.text.contains("p{}"));
        assert!(e.text.lines().any(|l| l.trim() == "a"));
    }

    #[test]
    fn unsupported_or_empty_documents_are_refused() {
        assert!(
            extract("tableau.xlsx", b"x")
                .unwrap_err()
                .contains("non ingéré")
        );
        assert!(extract("vide.txt", b"   \n\n ").is_err());
        assert!(!is_ingestible("photo.jpg"));
        assert!(is_ingestible("Rapport.PDF"));
    }

    #[test]
    fn slugs_are_readable_and_safe() {
        assert_eq!(
            slugify("Compte-rendu Réunion (été 2026).pdf"),
            "compte-rendu-reunion-ete-2026"
        );
        assert_eq!(slugify("../../etc/passwd"), "passwd");
        assert_eq!(slugify("???.txt"), "document");
        assert!(slugify(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn source_files_round_trip_and_keep_their_origin() {
        let meta = SourceMeta {
            titre: "Contrat: v2\n[brouillon]".into(),
            fichier: "contrat.pdf".into(),
            canal: "telegram".into(),
            origine: Origin::Untrusted,
            recu: "2026-09-16T10:00:00Z".into(),
            sha256: "abc".into(),
            format: "pdf".into(),
            pages: Some(3),
            caracteres: 42,
        };
        let raw = render_source(&meta, Some("Un contrat."), "Article 1.\n\n- tiret");
        let parsed = parse_source(&raw).expect("fiche");
        assert_eq!(parsed.origine, Origin::Untrusted);
        assert_eq!(parsed.text, "Article 1.\n\n- tiret");
        assert!(parsed.titre.starts_with("Contrat"));
        assert!(raw.contains("## Résumé"));

        // Une fiche retouchée à la main ne devient pas fiable pour autant…
        let forged = raw.replace("origine: untrusted", "origine: agent");
        assert_eq!(parse_source(&forged).unwrap().origine, Origin::Untrusted);
        // … sauf déclaration explicite du propriétaire.
        let owner = raw.replace("origine: untrusted", "origine: owner");
        assert_eq!(parse_source(&owner).unwrap().origine, Origin::Owner);
        assert!(parse_source("# notes libres").is_none());
    }

    #[test]
    fn passages_respect_paragraphs_and_the_target_size() {
        let text = format!(
            "Intro courte.\n\n{}\n\nConclusion.",
            "mot ".repeat(1_000).trim()
        );
        let ps = passages(&text, 300);
        assert!(ps.len() > 3);
        assert!(ps.iter().all(|p| p.chars().count() <= 300), "{ps:?}");
        assert!(ps[0].starts_with("Intro courte."));
        assert_eq!(ps.last().unwrap(), "Conclusion.");

        let entries = source_entries("contrat", &text, "2026-09-16");
        assert_eq!(entries[0].uid, "src-contrat-0001");
        assert!(entries.iter().all(|e| e.etype == SOURCE_ETYPE && !e.pinned));
        assert_eq!(entries[0].file, "sources/contrat.md");
    }
}
