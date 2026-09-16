//! Markdown → HTML Telegram, **mise en forme en ligne comprise** (§14.2).
//!
//! `to_blocks` + `to_html` aplatissent le texte d'un paragraphe : c'est suffisant pour un
//! gabarit, pas pour une réponse de modèle, qui mise sur le gras, le code en ligne et
//! les liens. Ce rendu parcourt les événements Markdown et produit uniquement les balises
//! que `parse_mode=HTML` accepte : `b`, `i`, `s`, `code`, `pre`, `a`, `blockquote`.
//! Tout le reste, HTML brut du modèle compris, est échappé.

use crate::render::escape_html;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Convertit un Markdown quelconque en HTML accepté par la Bot API.
pub fn markdown_to_html(markdown: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);

    let mut out = String::new();
    // Pile des listes : `None` = puces, `Some(n)` = numéro courant.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut table: Option<TableState> = None;
    let mut link_stack: Vec<String> = Vec::new();

    for ev in Parser::new_ext(markdown, opts) {
        // Dans un tableau, on ne garde que le texte des cellules.
        if let Some(t) = table.as_mut() {
            match ev {
                Event::End(TagEnd::Table) => {
                    let t = table.take().expect("tableau en cours");
                    out.push_str("<pre>");
                    out.push_str(&escape_html(&t.render()));
                    out.push_str("</pre>\n\n");
                }
                Event::Start(Tag::TableHead) => t.in_head = true,
                Event::End(TagEnd::TableHead) => {
                    t.header = std::mem::take(&mut t.row);
                    t.in_head = false;
                }
                Event::Start(Tag::TableRow) => t.row.clear(),
                Event::End(TagEnd::TableRow) => {
                    if !t.in_head {
                        let r = std::mem::take(&mut t.row);
                        t.rows.push(r);
                    }
                }
                Event::Start(Tag::TableCell) => t.cell.clear(),
                Event::End(TagEnd::TableCell) => {
                    let c = std::mem::take(&mut t.cell).trim().to_string();
                    t.row.push(c);
                }
                Event::Text(s) | Event::Code(s) | Event::Html(s) | Event::InlineHtml(s) => {
                    t.cell.push_str(&s)
                }
                _ => {}
            }
            continue;
        }

        match ev {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if lists.is_empty() {
                    out.push_str("\n\n");
                } else {
                    out.push('\n');
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                ensure_blank_line(&mut out);
                // Telegram n'a qu'un niveau de titre : le gras.
                let _ = level;
                out.push_str("<b>");
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("</b>\n\n"),

            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),

            Event::Start(Tag::Link { dest_url, .. }) => {
                let url = dest_url.to_string();
                if is_safe_url(&url) {
                    out.push_str(&format!("<a href=\"{}\">", escape_attr(&url)));
                    link_stack.push("</a>".into());
                } else {
                    link_stack.push(String::new());
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(close) = link_stack.pop() {
                    out.push_str(&close);
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                out.push_str("🖼 ");
                let url = dest_url.to_string();
                if is_safe_url(&url) {
                    out.push_str(&format!("<a href=\"{}\">", escape_attr(&url)));
                    link_stack.push("</a>".into());
                } else {
                    link_stack.push(String::new());
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some(close) = link_stack.pop() {
                    out.push_str(&close);
                }
            }

            Event::Code(t) => {
                out.push_str("<code>");
                out.push_str(&escape_html(&t));
                out.push_str("</code>");
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                ensure_line_start(&mut out);
                match kind {
                    CodeBlockKind::Fenced(lang) if !lang.trim().is_empty() => {
                        let l = lang.split_whitespace().next().unwrap_or_default();
                        out.push_str(&format!(
                            "<pre><code class=\"language-{}\">",
                            escape_attr(l)
                        ));
                    }
                    _ => out.push_str("<pre><code>"),
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                trim_trailing_newlines(&mut out);
                out.push_str("</code></pre>\n\n");
            }

            Event::Start(Tag::List(start)) => {
                if lists.is_empty() {
                    ensure_line_start(&mut out);
                }
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    trim_trailing_newlines(&mut out);
                    out.push_str("\n\n");
                }
            }
            Event::Start(Tag::Item) => {
                ensure_line_start(&mut out);
                let depth = lists.len().saturating_sub(1);
                out.push_str(&"   ".repeat(depth));
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("• "),
                }
            }
            Event::End(TagEnd::Item) => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),

            Event::Start(Tag::BlockQuote(_)) => {
                ensure_line_start(&mut out);
                out.push_str("<blockquote>");
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                trim_trailing_newlines(&mut out);
                out.push_str("</blockquote>\n\n");
            }

            Event::Start(Tag::Table(_)) => {
                ensure_line_start(&mut out);
                table = Some(TableState::default());
            }

            Event::Rule => {
                ensure_line_start(&mut out);
                out.push_str("——————\n\n");
            }

            // Le HTML brut du modèle est affiché, jamais interprété.
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                out.push_str(&escape_html(&t));
            }
            // Un saut de ligne simple du modèle est voulu : on le garde.
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push('\n'),
            _ => {}
        }
    }

    out.trim().to_string()
}

/// Version texte brut, pour le repli quand Telegram refuse le HTML.
pub fn html_to_plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

#[derive(Default)]
struct TableState {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_head: bool,
}

impl TableState {
    fn render(&self) -> String {
        let cols = self
            .header
            .len()
            .max(self.rows.iter().map(|r| r.len()).max().unwrap_or(0));
        let mut widths = vec![0usize; cols];
        for r in std::iter::once(&self.header).chain(self.rows.iter()) {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(c.chars().count()).min(24);
            }
        }
        let line = |r: &[String]| -> String {
            (0..cols)
                .map(|i| {
                    let c = r.get(i).map(|s| s.as_str()).unwrap_or("");
                    let c: String = c.chars().take(24).collect();
                    format!("{c:<w$}", w = widths[i])
                })
                .collect::<Vec<_>>()
                .join(" │ ")
                .trim_end()
                .to_string()
        };
        let mut s = String::new();
        if !self.header.is_empty() {
            s.push_str(&line(&self.header));
            s.push('\n');
            s.push_str(
                &widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
            );
            s.push('\n');
        }
        for r in &self.rows {
            s.push_str(&line(r));
            s.push('\n');
        }
        s.trim_end().to_string()
    }
}

fn is_safe_url(url: &str) -> bool {
    let u = url.trim().to_lowercase();
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("tg://")
}

fn escape_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

fn ensure_line_start(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn ensure_blank_line(out: &mut String) {
    if out.is_empty() {
        return;
    }
    trim_trailing_newlines(out);
    out.push_str("\n\n");
}

fn trim_trailing_newlines(out: &mut String) {
    while out.ends_with('\n') {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_formatting_survives() {
        let h = markdown_to_html("Un **mot** en *italique*, du `code` et ~~barré~~.");
        assert_eq!(
            h,
            "Un <b>mot</b> en <i>italique</i>, du <code>code</code> et <s>barré</s>."
        );
    }

    #[test]
    fn links_are_kept_only_when_safe() {
        assert_eq!(
            markdown_to_html("[doc](https://exemple.fr/a?b=1&c=\"2\")"),
            "<a href=\"https://exemple.fr/a?b=1&amp;c=&quot;2&quot;\">doc</a>"
        );
        assert_eq!(markdown_to_html("[piège](javascript:alert(1))"), "piège");
    }

    #[test]
    fn code_blocks_are_escaped_and_tagged() {
        let h = markdown_to_html("```rust\nfn main() { if a < b {} }\n```");
        assert_eq!(
            h,
            "<pre><code class=\"language-rust\">fn main() { if a &lt; b {} }</code></pre>"
        );
    }

    #[test]
    fn raw_html_from_the_model_is_displayed_not_interpreted() {
        let h = markdown_to_html("<script>alert(1)</script> et <b>gras</b>");
        assert!(!h.contains("<script>"), "{h}");
        assert!(h.contains("&lt;script&gt;"), "{h}");
    }

    #[test]
    fn lists_headings_and_quotes_render() {
        let md = "# Titre\n\nIntro.\n\n- un\n- deux\n  1. a\n  2. b\n\n> citation\n\nFin.";
        let h = markdown_to_html(md);
        assert!(h.starts_with("<b>Titre</b>"), "{h}");
        assert!(h.contains("• un\n• deux\n   1. a\n   2. b"), "{h}");
        assert!(h.contains("<blockquote>citation</blockquote>"), "{h}");
        assert!(h.ends_with("Fin."), "{h}");
    }

    #[test]
    fn tables_become_monospaced_blocks() {
        let h = markdown_to_html("| a | b |\n|---|---|\n| 1 | **2** |");
        assert!(h.starts_with("<pre>a │ b"), "{h}");
        assert!(h.contains("1 │ 2"), "{h}");
        assert!(
            !h.contains("<b>"),
            "pas de balise dans un bloc préformaté : {h}"
        );
    }

    #[test]
    fn every_tag_is_balanced() {
        let md = "**gras *imbriqué* fin** puis `code` et [lien **fort**](https://a.b)\n\n- **item**\n\n```\nx\n```";
        let h = markdown_to_html(md);
        for tag in ["b", "i", "code", "pre", "a", "blockquote", "s"] {
            let open =
                h.matches(&format!("<{tag}>")).count() + h.matches(&format!("<{tag} ")).count();
            let close = h.matches(&format!("</{tag}>")).count();
            assert_eq!(open, close, "balise `{tag}` déséquilibrée dans {h}");
        }
    }

    #[test]
    fn plain_fallback_strips_tags_and_unescapes() {
        assert_eq!(
            html_to_plain("<b>a &lt; b</b> &amp; <code>c</code>"),
            "a < b & c"
        );
    }
}
