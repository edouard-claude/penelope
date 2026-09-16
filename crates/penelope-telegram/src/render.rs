//! Rendu Markdown vers blocs riches et HTML de repli (§14.2).
//!
//! Un **seul AST** sert aux deux sorties : c'est la condition pour que le repli HTML dise
//! exactement la même chose que les blocs riches.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Bloc d'un `InputRichMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    List {
        ordered: bool,
        items: Vec<String>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    Quote {
        text: String,
        collapsible: bool,
    },
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
        compact: bool,
    },
    Separator,
    Footer {
        text: String,
    },
    Buttons {
        rows: Vec<Vec<ButtonSpec>>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonSpec {
    pub label: String,
    /// `callback_data` opaque ou URL.
    pub action: ButtonAction,
    /// `primary`, `success`, `danger` ou vide.
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ButtonAction {
    Callback { token: String },
    Url { url: String },
}

impl ButtonSpec {
    pub fn callback(label: &str, token: &str, style: &str) -> Self {
        ButtonSpec {
            label: label.into(),
            action: ButtonAction::Callback {
                token: token.into(),
            },
            style: style.into(),
            disabled: false,
        }
    }
    pub fn url(label: &str, url: &str) -> Self {
        ButtonSpec {
            label: label.into(),
            action: ButtonAction::Url { url: url.into() },
            style: String::new(),
            disabled: false,
        }
    }
    /// Un bouton non applicable est **affiché désactivé** plutôt que retiré, pour garder
    /// la mise en page (§14.5).
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn to_json(&self) -> Value {
        match &self.action {
            ButtonAction::Callback { token } => json!({
                "text": self.decorated_label(),
                "callback_data": token,
            }),
            ButtonAction::Url { url } => json!({
                "text": self.decorated_label(),
                "url": url,
            }),
        }
    }

    fn decorated_label(&self) -> String {
        if self.disabled {
            format!("· {} ·", self.label)
        } else {
            self.label.clone()
        }
    }
}

/// Convertit du Markdown en blocs riches.
pub fn to_blocks(markdown: &str) -> Vec<Block> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, opts);

    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut list_items: Vec<String> = Vec::new();
    let mut ordered = false;
    let mut in_list = false;
    let mut in_code: Option<Option<String>> = None;
    let mut in_quote = false;
    let mut heading_level: Option<u8> = None;
    let mut table: Option<(Vec<String>, Vec<Vec<String>>)> = None;
    let mut row: Vec<String> = Vec::new();
    let mut in_header = false;

    for ev in parser {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_paragraph(&mut text, &mut blocks);
                heading_level = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(l) = heading_level.take() {
                    let t = std::mem::take(&mut text).trim().to_string();
                    if !t.is_empty() {
                        blocks.push(Block::Heading { level: l, text: t });
                    }
                }
            }
            Event::Start(Tag::List(start)) => {
                flush_paragraph(&mut text, &mut blocks);
                ordered = start.is_some();
                in_list = true;
                list_items.clear();
            }
            Event::End(TagEnd::List(_)) => {
                if in_list {
                    blocks.push(Block::List {
                        ordered,
                        items: std::mem::take(&mut list_items),
                    });
                    in_list = false;
                }
            }
            Event::Start(Tag::Item) => text.clear(),
            Event::End(TagEnd::Item) => {
                let t = std::mem::take(&mut text).trim().to_string();
                if !t.is_empty() {
                    list_items.push(t);
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_paragraph(&mut text, &mut blocks);
                in_code = Some(match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.to_string()),
                    _ => None,
                });
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(lang) = in_code.take() {
                    blocks.push(Block::Code {
                        language: lang,
                        text: std::mem::take(&mut text),
                    });
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush_paragraph(&mut text, &mut blocks);
                in_quote = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let t = std::mem::take(&mut text).trim().to_string();
                if !t.is_empty() {
                    blocks.push(Block::Quote {
                        collapsible: t.chars().count() > 300,
                        text: t,
                    });
                }
                in_quote = false;
            }
            Event::Start(Tag::Table(_)) => {
                flush_paragraph(&mut text, &mut blocks);
                table = Some((Vec::new(), Vec::new()));
            }
            Event::End(TagEnd::Table) => {
                if let Some((header, rows)) = table.take() {
                    let width = header
                        .len()
                        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
                    blocks.push(Block::Table {
                        header,
                        rows,
                        compact: width > 3,
                    });
                }
            }
            Event::Start(Tag::TableHead) => in_header = true,
            Event::End(TagEnd::TableHead) => {
                if let Some((h, _)) = table.as_mut() {
                    *h = std::mem::take(&mut row);
                }
                in_header = false;
            }
            Event::Start(Tag::TableRow) => row.clear(),
            Event::End(TagEnd::TableRow) => {
                if !in_header && let Some((_, rows)) = table.as_mut() {
                    rows.push(std::mem::take(&mut row));
                }
            }
            Event::Start(Tag::TableCell) => text.clear(),
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut text).trim().to_string()),
            Event::Rule => {
                flush_paragraph(&mut text, &mut blocks);
                blocks.push(Block::Separator);
            }
            // Le HTML brut produit par le modèle est traité comme du **texte littéral** :
            // il sera échappé au rendu, jamais interprété.
            Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => {
                text.push_str(&t)
            }
            Event::SoftBreak => text.push(' '),
            Event::HardBreak => text.push('\n'),
            Event::End(TagEnd::Paragraph) => {
                if !in_quote && !in_list {
                    flush_paragraph(&mut text, &mut blocks);
                } else if in_quote {
                    text.push('\n');
                }
            }
            _ => {}
        }
    }
    flush_paragraph(&mut text, &mut blocks);
    blocks
}

fn flush_paragraph(text: &mut String, blocks: &mut Vec<Block>) {
    let t = std::mem::take(text).trim().to_string();
    if !t.is_empty() {
        blocks.push(Block::Paragraph { text: t });
    }
}

/// Échappement HTML strict pour Telegram : seuls `&`, `<` et `>` sont spéciaux.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Repli HTML (`parse_mode=HTML`), à partir des **mêmes blocs**.
pub fn to_html(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Paragraph { text } => {
                out.push_str(&escape_html(text));
                out.push_str("\n\n");
            }
            Block::Heading { text, .. } => {
                out.push_str(&format!("<b>{}</b>\n\n", escape_html(text)));
            }
            Block::List { ordered, items } => {
                for (i, it) in items.iter().enumerate() {
                    if *ordered {
                        out.push_str(&format!("{}. {}\n", i + 1, escape_html(it)));
                    } else {
                        out.push_str(&format!("• {}\n", escape_html(it)));
                    }
                }
                out.push('\n');
            }
            Block::Code { language, text } => {
                let lang = language
                    .as_ref()
                    .map(|l| format!(" class=\"language-{}\"", escape_html(l)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<pre><code{lang}>{}</code></pre>\n",
                    escape_html(text.trim_end())
                ));
            }
            Block::Quote { text, collapsible } => {
                let tag = if *collapsible {
                    "blockquote expandable"
                } else {
                    "blockquote"
                };
                out.push_str(&format!("<{tag}>{}</blockquote>\n\n", escape_html(text)));
            }
            Block::Table {
                header,
                rows,
                compact,
            } => {
                out.push_str("<pre>");
                out.push_str(&render_table_text(header, rows, *compact));
                out.push_str("</pre>\n");
            }
            Block::Separator => out.push_str("——————\n"),
            Block::Footer { text } => {
                out.push_str(&format!("<i>{}</i>\n", escape_html(text)));
            }
            // Les boutons ne font pas partie du corps HTML : ils passent par
            // `reply_markup`.
            Block::Buttons { .. } => {}
        }
    }
    out.trim_end().to_string()
}

fn render_table_text(header: &[String], rows: &[Vec<String>], compact: bool) -> String {
    let cols = header
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    let mut widths = vec![0usize; cols];
    fn cell(r: &[String], i: usize) -> &str {
        r.get(i).map(|s| s.as_str()).unwrap_or("")
    }
    for (i, w) in widths.iter_mut().enumerate() {
        *w = cell(header, i).chars().count();
        for r in rows {
            *w = (*w).max(cell(r, i).chars().count());
        }
        if compact {
            *w = (*w).min(14);
        }
    }
    let line = |r: &[String]| -> String {
        (0..cols)
            .map(|i| {
                let c: String = cell(r, i).chars().take(widths[i]).collect();
                format!("{c:<width$}", width = widths[i])
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let mut s = String::new();
    if !header.is_empty() {
        s.push_str(&line(header));
        s.push('\n');
        s.push_str(
            &widths
                .iter()
                .map(|w| "-".repeat(*w))
                .collect::<Vec<_>>()
                .join("-+-"),
        );
        s.push('\n');
    }
    for r in rows {
        s.push_str(&line(r));
        s.push('\n');
    }
    s
}

/// Construit la charge utile `blocks` d'un `sendRichMessage`.
pub fn blocks_to_json(blocks: &[Block]) -> Value {
    Value::Array(
        blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph { text } => json!({"type":"paragraph","text":text}),
                Block::Heading { level, text } => {
                    json!({"type":"heading","level":level,"text":text})
                }
                Block::List { ordered, items } => {
                    json!({"type":"list","ordered":ordered,"items":items})
                }
                Block::Code { language, text } => {
                    json!({"type":"preformatted","language":language,"text":text})
                }
                Block::Quote { text, collapsible } => {
                    json!({"type":"quote","text":text,"expandable":collapsible})
                }
                Block::Table {
                    header,
                    rows,
                    compact,
                } => json!({
                    "type":"table","header":header,"rows":rows,"is_compact":compact
                }),
                Block::Separator => json!({"type":"separator"}),
                Block::Footer { text } => json!({"type":"footer","text":text}),
                Block::Buttons { rows } => json!({
                    "type":"buttons",
                    "rows": rows.iter().map(|r| {
                        r.iter().map(|b| b.to_json()).collect::<Vec<_>>()
                    }).collect::<Vec<_>>()
                }),
            })
            .collect(),
    )
}

/// Découpe un texte en fragments **sans jamais couper** un bloc de code ni une entité
/// (§14.1).
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut fence: Option<String> = None;

    for line in text.split('\n') {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```");

        // Une ligne plus longue que la limite est coupée sur une frontière de caractère.
        if line.chars().count() > limit {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            for chunk in chunk_chars(line, limit) {
                out.push(chunk);
            }
            continue;
        }

        let would_exceed = current.chars().count() + line.chars().count() + 1 > limit;
        if would_exceed && fence.is_none() {
            out.push(std::mem::take(&mut current));
        } else if would_exceed && fence.is_some() {
            // On est dans un bloc de code : on le referme proprement, puis on le rouvre.
            let lang = fence.clone().unwrap_or_default();
            current.push_str("\n```");
            out.push(std::mem::take(&mut current));
            current.push_str(&format!("```{lang}\n"));
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);

        if is_fence {
            fence = match fence {
                Some(_) => None,
                None => Some(trimmed.trim_start_matches("```").trim().to_string()),
            };
        }
    }

    if fence.is_some() {
        current.push_str("\n```");
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

fn chunk_chars(s: &str, limit: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars
        .chunks(limit.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

/// Décide si une réponse doit partir en document plutôt qu'en messages (§14.1 : au-delà
/// de 3 fragments).
pub fn should_send_as_document(fragments: usize, max_fragments: usize) -> bool {
    fragments > max_fragments
}

/// Pied de message repliable : modèle, tokens, coût, durée (§14.5 `answer`).
pub fn footer(model: &str, prompt: u64, completion: u64, cost_usd: f64, ms: u64) -> Block {
    Block::Footer {
        text: format!(
            "{model} · {prompt}→{completion} tokens · {:.4} $ · {:.1} s",
            cost_usd,
            ms as f64 / 1000.0
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_typed_blocks() {
        let md = "# Titre\n\nUn paragraphe.\n\n- un\n- deux\n\n```rust\nfn main() {}\n```\n\n> une citation\n\n---\n";
        let b = to_blocks(md);
        assert!(matches!(b[0], Block::Heading { level: 1, .. }));
        assert!(matches!(&b[1], Block::Paragraph { text } if text == "Un paragraphe."));
        match &b[2] {
            Block::List { ordered, items } => {
                assert!(!ordered);
                assert_eq!(items, &vec!["un".to_string(), "deux".to_string()]);
            }
            other => panic!("{other:?}"),
        }
        match &b[3] {
            Block::Code { language, text } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert!(text.contains("fn main"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(b[4], Block::Quote { .. }));
        assert!(matches!(b[5], Block::Separator));
    }

    #[test]
    fn tables_are_parsed_and_compacted() {
        let md = "| a | b | c | d |\n|---|---|---|---|\n| 1 | 2 | 3 | 4 |\n";
        let b = to_blocks(md);
        match &b[0] {
            Block::Table {
                header,
                rows,
                compact,
            } => {
                assert_eq!(header.len(), 4);
                assert_eq!(rows[0], vec!["1", "2", "3", "4"]);
                assert!(compact, "un tableau large doit être compact");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn html_fallback_escapes_and_preserves_structure() {
        let b = to_blocks("# T <b>\n\nTexte & <script>\n\n```\na < b\n```\n");
        let h = to_html(&b);
        assert!(h.contains("<b>T &lt;b&gt;</b>"));
        assert!(h.contains("Texte &amp; &lt;script&gt;"));
        assert!(h.contains("<pre><code>a &lt; b</code></pre>"));
    }

    #[test]
    fn long_quotes_are_expandable() {
        let long = "mot ".repeat(200);
        let b = to_blocks(&format!("> {long}"));
        match &b[0] {
            Block::Quote { collapsible, .. } => assert!(collapsible),
            other => panic!("{other:?}"),
        }
        assert!(to_html(&b).contains("blockquote expandable"));
    }

    #[test]
    fn blocks_json_shape() {
        let b = to_blocks("Un texte.\n\n```py\nx=1\n```");
        let j = blocks_to_json(&b);
        assert_eq!(j[0]["type"], "paragraph");
        assert_eq!(j[1]["type"], "preformatted");
        assert_eq!(j[1]["language"], "py");
    }

    /// CA 14 : le découpage d'une réponse de 20 000 caractères ne casse aucun bloc de
    /// code.
    #[test]
    fn ca_14_6_splitting_never_breaks_a_code_block() {
        let mut md = String::new();
        for i in 0..40 {
            md.push_str(&format!(
                "Paragraphe {i} avec un peu de texte pour occuper.\n\n"
            ));
            md.push_str("```rust\n");
            for j in 0..20 {
                md.push_str(&format!("let variable_{j} = {j} * {i};\n"));
            }
            md.push_str("```\n\n");
        }
        assert!(md.chars().count() > 20_000);

        let parts = split_message(&md, 4096);
        assert!(parts.len() > 4);
        for p in &parts {
            assert!(p.chars().count() <= 4096 + 8, "fragment trop long");
            let fences = p.matches("```").count();
            assert_eq!(
                fences % 2,
                0,
                "clôtures déséquilibrées dans un fragment :\n{}",
                &p[..p.len().min(200)]
            );
        }
        // Rien n'est perdu : toutes les variables se retrouvent.
        let joined: String = parts.join("\n");
        assert!(joined.contains("let variable_19 = 19 * 39;"));
    }

    #[test]
    fn short_text_is_not_split() {
        assert_eq!(split_message("court", 4096), vec!["court".to_string()]);
    }

    #[test]
    fn a_single_very_long_line_is_chunked() {
        let line = "a".repeat(10_000);
        let parts = split_message(&line, 4096);
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts.iter().map(|p| p.chars().count()).sum::<usize>(),
            10_000
        );
    }

    #[test]
    fn document_threshold() {
        assert!(!should_send_as_document(3, 3));
        assert!(should_send_as_document(4, 3));
    }

    #[test]
    fn buttons_serialise_with_callback_or_url() {
        let b = ButtonSpec::callback("Autoriser", "a:abc123", "success");
        assert_eq!(b.to_json()["callback_data"], "a:abc123");
        let u = ButtonSpec::url("Ouvrir", "https://x");
        assert_eq!(u.to_json()["url"], "https://x");
        let d = ButtonSpec::callback("Reprendre", "a:x", "").disabled();
        assert!(d.to_json()["text"].as_str().unwrap().starts_with('·'));
    }

    #[test]
    fn footer_reports_cost_and_duration() {
        let Block::Footer { text } = footer("deepseek-v4", 12_000, 300, 0.0123, 2500) else {
            panic!("bloc inattendu");
        };
        assert!(text.contains("12000→300"));
        assert!(text.contains("0.0123 $"));
        assert!(text.contains("2.5 s"));
    }

    #[test]
    fn inline_code_survives_in_paragraphs() {
        let b = to_blocks("Utilise `cargo test` pour lancer.");
        match &b[0] {
            Block::Paragraph { text } => assert!(text.contains("cargo test")),
            other => panic!("{other:?}"),
        }
    }
}
