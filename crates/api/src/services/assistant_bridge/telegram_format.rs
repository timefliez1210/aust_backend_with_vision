//! Turns Josie's Markdown-ish replies into Telegram HTML that reads well on a phone.
//!
//! The model writes Markdown out of habit; Telegram shows `**fett**` and table pipes
//! literally, and a table does not fit a phone screen at all. This is the safety
//! net behind the prompt rule ("keine Tabellen"):
//! - tables become one block per row: the first cell bold, the rest as
//!   "Spalte: Wert" lines; columns that only hold UUIDs are dropped (the message
//!   refs keep the IDs for replies),
//! - `**fett**` / `__fett__` → `<b>`, `` `code` `` → `<code>`, `# Überschrift` → bold,
//!   `- Punkt` → `• Punkt`, code fences are dropped,
//! - everything else is HTML-escaped.

/// Render one message (≤ 4000 chars of Markdown) as Telegram HTML.
pub fn to_telegram_html(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with('|') {
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            out.push(render_table(&lines[start..i]));
            continue;
        }
        i += 1;
        if trimmed.starts_with("```") {
            continue;
        }
        if let Some(h) = heading(trimmed) {
            out.push(format!("<b>{}</b>", inline(h)));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent = &line[..line.len() - trimmed.len()];
            out.push(format!("{indent}• {}", inline(item)));
        } else if is_rule(trimmed) {
            out.push(String::new());
        } else {
            out.push(inline(line));
        }
    }
    collapse_blank_lines(&out.join("\n"))
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
        .map(str::trim)
}

fn is_rule(line: &str) -> bool {
    line.len() >= 3 && line.chars().all(|c| matches!(c, '-' | '*' | '_'))
}

fn cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = row.strip_suffix('|').unwrap_or(row);
    row.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator(row: &[String]) -> bool {
    row.iter()
        .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

fn is_uuid(s: &str) -> bool {
    let s = s.trim_matches('`');
    s.len() == 36 && uuid::Uuid::parse_str(s).is_ok()
}

fn render_table(rows: &[&str]) -> String {
    let parsed: Vec<Vec<String>> = rows
        .iter()
        .map(|r| cells(r))
        .filter(|r| !is_separator(r))
        .collect();
    let Some((header, body)) = parsed.split_first() else {
        return String::new();
    };
    if body.is_empty() {
        // A lone row: show it as a plain line.
        return inline(&header.join(" · "));
    }
    let keep: Vec<usize> = (0..header.len())
        .filter(|&c| !body.iter().all(|r| r.get(c).is_none_or(|v| is_uuid(v) || v.is_empty())))
        .collect();
    let mut blocks = Vec::new();
    for row in body {
        let mut lines = Vec::new();
        for (n, &c) in keep.iter().enumerate() {
            let value = row.get(c).map(String::as_str).unwrap_or("");
            if value.is_empty() || value == "-" || value == "—" {
                continue;
            }
            if n == 0 {
                lines.push(format!("• <b>{}</b>", inline(value)));
            } else {
                let label = header.get(c).map(String::as_str).unwrap_or("");
                if label.is_empty() {
                    lines.push(format!("   {}", inline(value)));
                } else {
                    lines.push(format!("   {}: {}", inline(label), inline(value)));
                }
            }
        }
        if !lines.is_empty() {
            blocks.push(lines.join("\n"));
        }
    }
    format!("\n{}\n", blocks.join("\n\n"))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Inline Markdown → HTML on one line; unpaired markers stay as text.
fn inline(s: &str) -> String {
    let escaped = escape(s);
    let bold = pair(&escaped, "**", "<b>", "</b>");
    let bold = pair(&bold, "__", "<b>", "</b>");
    pair(&bold, "`", "<code>", "</code>")
}

fn pair(s: &str, marker: &str, open: &str, close: &str) -> String {
    let parts: Vec<&str> = s.split(marker).collect();
    if parts.len() < 3 {
        return s.to_string();
    }
    let mut out = String::new();
    let pairs = (parts.len() - 1) / 2;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            if i <= pairs * 2 {
                out.push_str(if i % 2 == 1 { open } else { close });
            } else {
                out.push_str(marker);
            }
        }
        out.push_str(part);
    }
    out
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoice_table_becomes_a_phone_list() {
        let md = "Es liegen **zwei Rechnungen** offen:\n\n\
| Rechnungs-ID | Rechnungs-Nr. | Versand-Datum | Status |\n\
|--------------|---------------|---------------|--------|\n\
| 01a13e52-3763-7282-9983-e1012108c65e | 2026-116 | 15.10.2026 | gesendet |\n\
| 01a11600-a606-74c3-9d75-1c3022bf82fe | 2026-111 | 07.10.2026 | gesendet |\n\n\
Soll ich erinnern?";
        let html = to_telegram_html(md);
        assert_eq!(
            html,
            "Es liegen <b>zwei Rechnungen</b> offen:\n\n\
• <b>2026-116</b>\n   Versand-Datum: 15.10.2026\n   Status: gesendet\n\n\
• <b>2026-111</b>\n   Versand-Datum: 07.10.2026\n   Status: gesendet\n\n\
Soll ich erinnern?"
        );
        assert!(!html.contains("01a13e52"), "UUID-only column dropped");
    }

    #[test]
    fn markdown_basics_and_escaping() {
        let html = to_telegram_html("## Heute\n- **Lange** <Umzug> & Co\n* `2026-116`\n---\nPreis 3*4 = 12");
        assert_eq!(
            html,
            "<b>Heute</b>\n• <b>Lange</b> &lt;Umzug&gt; &amp; Co\n• <code>2026-116</code>\n\nPreis 3*4 = 12"
        );
    }

    #[test]
    fn unpaired_markers_stay_literal() {
        assert_eq!(to_telegram_html("**fett** und **offen"), "<b>fett</b> und **offen");
        assert_eq!(to_telegram_html("a_b__c"), "a_b__c");
    }

    #[test]
    fn code_fences_are_dropped() {
        assert_eq!(to_telegram_html("```\nx = 1\n```"), "x = 1");
    }
}
