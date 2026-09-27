#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Scalar(String),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[(String, Node)] {
        match self {
            Node::Map(entries) => entries,
            _ => &[],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.as_str()?.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Some(true),
            "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Line {
    indent: usize,
    text: String,
}

/// Retire un commentaire `#` hors guillemets (précédé d'un blanc ou en début).
fn strip_comment(line: &str) -> &str {
    let (mut single, mut double) = (false, false);
    let mut prev_blank = true;
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && prev_blank => return &line[..i],
            _ => {}
        }
        prev_blank = c == ' ' || c == '\t';
    }
    line
}

fn lines(raw: &str) -> Vec<Line> {
    raw.replace("\r\n", "\n")
        .split('\n')
        .filter_map(|l| {
            let t = l.trim();
            if t == "---" || t == "..." {
                return None;
            }
            let indent = l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let text = strip_comment(&l[indent..]).trim_end().to_string();
            Some(Line { indent, text })
        })
        .collect()
}

pub fn parse(raw: &str) -> Node {
    let mut all = lines(raw);
    let mut pos = 0;
    skip_blank(&all, &mut pos);
    if pos >= all.len() {
        return Node::Null;
    }
    let indent = all[pos].indent;
    parse_block(&mut all, &mut pos, indent)
}

fn skip_blank(lines: &[Line], pos: &mut usize) {
    while *pos < lines.len() && lines[*pos].text.is_empty() {
        *pos += 1;
    }
}

fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_block(lines: &mut [Line], pos: &mut usize, indent: usize) -> Node {
    skip_blank(lines, pos);
    if *pos >= lines.len() {
        return Node::Null;
    }
    if is_item(&lines[*pos].text) {
        parse_list(lines, pos, indent)
    } else {
        parse_map(lines, pos, indent)
    }
}

/// `clé: valeur`, clé éventuellement entre guillemets ; `:` suivi d'un blanc ou final.
fn split_key(text: &str) -> Option<(String, String)> {
    let (key_end, value_start) =
        if let Some(q) = text.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let close = text[1..].find(q)? + 1;
            let after = &text[close + 1..];
            let rest = after.trim_start().strip_prefix(':')?;
            if !(rest.is_empty() || rest.starts_with(' ')) {
                return None;
            }
            (close + 1, text.len() - rest.len())
        } else {
            let bytes = text.as_bytes();
            let i = (0..bytes.len())
                .find(|&i| bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' '))?;
            if text[..i].contains(['[', '{', '"', '\'']) {
                return None;
            }
            (i, i + 1)
        };
    let key = super::unquote(&text[..key_end]);
    if key.is_empty() {
        return None;
    }
    Some((key, text[value_start..].trim().to_string()))
}

fn balanced(text: &str) -> bool {
    let (mut depth, mut single, mut double) = (0i32, false, false);
    for c in text.chars() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '[' | '{' if !single && !double => depth += 1,
            ']' | '}' if !single && !double => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

fn parse_map(lines: &mut [Line], pos: &mut usize, indent: usize) -> Node {
    let mut entries = Vec::new();
    while *pos < lines.len() {
        let line = lines[*pos].clone();
        if line.text.is_empty() {
            *pos += 1;
            continue;
        }
        if line.indent < indent || (line.indent == indent && is_item(&line.text)) {
            break;
        }
        *pos += 1;
        if line.indent > indent {
            continue; // ligne orpheline : ignorée
        }
        let Some((key, mut value)) = split_key(&line.text) else {
            continue;
        };
        let node = if value.is_empty() {
            skip_blank(lines, pos);
            match lines.get(*pos) {
                Some(next)
                    if next.indent > indent || (next.indent == indent && is_item(&next.text)) =>
                {
                    let child = next.indent;
                    parse_block(lines, pos, child)
                }
                _ => Node::Null,
            }
        } else if matches!(value.as_str(), "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let literal = value.starts_with('|');
            let mut parts = Vec::new();
            while *pos < lines.len() && (lines[*pos].text.is_empty() || lines[*pos].indent > indent)
            {
                parts.push(lines[*pos].text.clone());
                *pos += 1;
            }
            while parts.last().is_some_and(|p| p.is_empty()) {
                parts.pop();
            }
            Node::Scalar(if literal {
                parts.join("\n")
            } else {
                parts.join(" ")
            })
        } else {
            if value.starts_with('[') || value.starts_with('{') {
                while !balanced(&value) && *pos < lines.len() {
                    value.push(' ');
                    value.push_str(lines[*pos].text.trim());
                    *pos += 1;
                }
            }
            scalar_or_flow(&value)
        };
        entries.push((key, node));
    }
    Node::Map(entries)
}

fn parse_list(lines: &mut [Line], pos: &mut usize, indent: usize) -> Node {
    let mut items = Vec::new();
    while *pos < lines.len() {
        let line = lines[*pos].clone();
        if line.text.is_empty() {
            *pos += 1;
            continue;
        }
        if line.indent != indent || !is_item(&line.text) {
            break;
        }
        let rest = line.text[1..].trim_start().to_string();
        if rest.is_empty() {
            *pos += 1;
            skip_blank(lines, pos);
            match lines.get(*pos) {
                Some(next) if next.indent > indent => {
                    let child = next.indent;
                    items.push(parse_block(lines, pos, child));
                }
                _ => items.push(Node::Null),
            }
        } else if !rest.starts_with(['"', '\'', '[', '{']) && split_key(&rest).is_some() {
            // `- clé: valeur` : table dont la première clé est sur la ligne du tiret.
            let offset = line.text.len() - rest.len();
            lines[*pos] = Line {
                indent: indent + offset,
                text: rest,
            };
            items.push(parse_map(lines, pos, indent + offset));
        } else {
            *pos += 1;
            let mut value = rest;
            if value.starts_with('[') || value.starts_with('{') {
                while !balanced(&value) && *pos < lines.len() {
                    value.push(' ');
                    value.push_str(lines[*pos].text.trim());
                    *pos += 1;
                }
            }
            items.push(scalar_or_flow(&value));
        }
    }
    Node::List(items)
}

/// Découpe sur les virgules de premier niveau, hors guillemets.
fn split_flow(inner: &str) -> Vec<String> {
    let (mut depth, mut single, mut double) = (0i32, false, false);
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for c in inner.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if double => {
                escaped = true;
                current.push(c);
                continue;
            }
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '[' | '{' if !single && !double => depth += 1,
            ']' | '}' if !single && !double => depth -= 1,
            ',' if !single && !double && depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn scalar_or_flow(v: &str) -> Node {
    let v = v.trim();
    if v.starts_with('[') && v.ends_with(']') {
        return Node::List(
            split_flow(&v[1..v.len() - 1])
                .iter()
                .map(|p| scalar_or_flow(p))
                .collect(),
        );
    }
    if v.starts_with('{') && v.ends_with('}') {
        return Node::Map(
            split_flow(&v[1..v.len() - 1])
                .iter()
                .filter_map(|p| {
                    let (k, val) = split_key(p).or_else(|| {
                        p.split_once(':')
                            .map(|(k, val)| (super::unquote(k), val.trim().to_string()))
                    })?;
                    Some((k, scalar_or_flow(&val)))
                })
                .collect(),
        );
    }
    if v.is_empty() || v == "~" || v == "null" {
        return Node::Null;
    }
    Node::Scalar(scalar(v))
}

fn scalar(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => out.push_str(&format!("\\u{hex}")),
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}
