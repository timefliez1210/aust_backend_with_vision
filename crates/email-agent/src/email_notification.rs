//! The "📩 Neue E-Mail" Telegram notification, with the mail's full text.
//!
//! Alex reads incoming mail in Telegram, so the notification carries the whole
//! body instead of a 200-character teaser. Telegram caps a message at 4096
//! characters, so long mails are split over several messages. The quoted history
//! below a reply ("Am … schrieb …:", Outlook's "Von: / Gesendet:", `>` lines) is
//! cut — it is the conversation Alex already has, and on a long thread it is most
//! of the mail.

/// Characters per Telegram message. Telegram counts UTF-16 units (an emoji is
/// two), so this stays well below the 4096 limit.
const CHUNK_CHARS: usize = 3500;
/// At most this many messages per email; the rest is in the dashboard.
const MAX_CHUNKS: usize = 4;

/// Build the notification messages for one incoming email, in send order.
pub(crate) fn format_email_notification(
    from: &str,
    subject: &str,
    body: &str,
    attachments: &[String],
) -> Vec<String> {
    let mut header = format!("📩 Neue E-Mail\nVon: {from}\nBetreff: {subject}");
    if !attachments.is_empty() {
        header.push_str(&format!("\n📎 {}", attachments.join(", ")));
    }

    let (text, had_quote) = strip_quoted_history(body);
    let mut text = text.trim().to_string();
    if text.is_empty() {
        text = "(kein Text)".to_string();
    }
    if had_quote {
        text.push_str("\n\n[… zitierter Verlauf ausgeblendet]");
    }

    let full = format!("{header}\n\n{text}");
    let mut chunks = split_chunks(&full, CHUNK_CHARS);
    if chunks.len() > MAX_CHUNKS {
        chunks.truncate(MAX_CHUNKS);
        if let Some(last) = chunks.last_mut() {
            last.push_str("\n\n[… gekürzt — vollständig im Dashboard unter E-Mails]");
        }
    }
    let total = chunks.len();
    if total > 1 {
        for (i, c) in chunks.iter_mut().enumerate().skip(1) {
            *c = format!("📩 ({}/{total}) {subject}\n\n{c}", i + 1);
        }
    }
    chunks
}

/// Split `s` into pieces of at most `max` chars, preferring to cut at a line
/// break, then at a space, in the back half of each piece.
fn split_chunks(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest: &str = s;
    while rest.chars().count() > max {
        // Byte index just past the `max`-th char.
        let hard = rest.char_indices().nth(max).map(|(i, _)| i).unwrap_or(rest.len());
        let window = &rest[..hard];
        let half = window.len() / 2;
        let cut = window
            .rfind('\n')
            .filter(|&i| i >= half)
            .or_else(|| window.rfind(' ').filter(|&i| i >= half))
            .unwrap_or(hard);
        out.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Cut the quoted history off a reply. Returns the kept text and whether anything
/// was cut. Never cuts the whole mail: a body that *starts* with a quote marker
/// is kept as is.
pub(crate) fn strip_quoted_history(body: &str) -> (String, bool) {
    let lines: Vec<&str> = body.lines().collect();
    // The first marker decides; one on the very first line means the whole mail
    // is a quote (a forward, say), and it is kept.
    if let Some(i) = (0..lines.len()).find(|&i| starts_quote(&lines, i)).filter(|&i| i > 0) {
        let kept = lines[..i].join("\n");
        if !kept.trim().is_empty() {
            return (kept, true);
        }
    }
    (body.to_string(), false)
}

/// Whether line `i` opens the quoted history.
fn starts_quote(lines: &[&str], i: usize) -> bool {
    let line = lines[i].trim();
    let next = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");

    // "Am 08.10.2026 um 10:00 schrieb Max <max@x.de>:" — clients wrap the address
    // onto the next line, so look at both.
    if line.starts_with("Am ") && (line.contains("schrieb") || next.contains("schrieb")) {
        return true;
    }
    if line.starts_with("On ") && (line.contains("wrote") || next.contains("wrote")) {
        return true;
    }
    let lower = line.to_lowercase();
    if lower.contains("-----ursprüngliche nachricht-----")
        || lower.contains("-----original message-----")
    {
        return true;
    }
    // Outlook header block: "Von: …" followed by "Gesendet: …" (or From/Sent).
    let window = || lines[i + 1..].iter().take(3).map(|l| l.trim());
    if line.starts_with("Von:") && window().any(|l| l.starts_with("Gesendet:")) {
        return true;
    }
    if line.starts_with("From:") && window().any(|l| l.starts_with("Sent:")) {
        return true;
    }
    // A trailing block of "> " lines.
    if line.starts_with('>') {
        return lines[i..]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .all(|l| l.starts_with('>'));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_mail_is_one_message_with_full_body() {
        let body = "Hallo,\n\nwir ziehen am 12.11. um. Gibt es noch einen Termin?\n\nGruß\nAnna";
        let msgs = format_email_notification("anna@x.de", "Umzug", body, &[]);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("Von: anna@x.de"));
        assert!(msgs[0].contains("Betreff: Umzug"));
        assert!(msgs[0].contains(body));
    }

    #[test]
    fn lists_attachments() {
        let msgs = format_email_notification(
            "a@x.de",
            "Fotos",
            "Anbei",
            &["wohnzimmer.jpg".to_string(), "keller.jpg".to_string()],
        );
        assert!(msgs[0].contains("📎 wohnzimmer.jpg, keller.jpg"));
    }

    #[test]
    fn german_reply_history_is_cut() {
        let body = "Passt, danke!\n\nAm 07.10.2026 um 18:00 schrieb Aust Umzüge\n<info@aust.de>:\n> Ihr Angebot …\n> Gruß";
        let msgs = format_email_notification("a@x.de", "Re: Angebot", body, &[]);
        assert!(msgs[0].contains("Passt, danke!"));
        assert!(!msgs[0].contains("Ihr Angebot"));
        assert!(msgs[0].contains("zitierter Verlauf ausgeblendet"));
    }

    #[test]
    fn outlook_history_is_cut() {
        let body = "Ja, gerne.\n\nVon: Aust Umzüge <info@aust.de>\nGesendet: Mittwoch\nAn: a@x.de\nBetreff: KVA\n\nalter Text";
        let (kept, cut) = strip_quoted_history(body);
        assert!(cut);
        assert_eq!(kept.trim(), "Ja, gerne.");
    }

    #[test]
    fn inline_quotes_followed_by_answers_are_kept() {
        // Interleaved answers: the ">" block is not trailing, so nothing is cut.
        let body = "Hallo\n> Wann passt es?\nDienstag passt.\n> Wie viele Kisten?\n30";
        let (kept, cut) = strip_quoted_history(body);
        assert!(!cut);
        assert_eq!(kept, body);
    }

    #[test]
    fn a_mail_that_is_only_a_quote_is_kept() {
        let body = "Am 07.10.2026 schrieb X:\n> alles";
        let (kept, cut) = strip_quoted_history(body);
        assert!(!cut);
        assert_eq!(kept, body);
    }

    #[test]
    fn long_mail_is_split_without_losing_text() {
        let para = "Das ist ein längerer Absatz mit Umlauten äöü und ß. ".repeat(20);
        let body = [para.as_str(); 8].join("\n");
        let msgs = format_email_notification("a@x.de", "Lang", &body, &[]);
        assert!(msgs.len() > 1);
        assert!(msgs.iter().all(|m| m.chars().count() <= CHUNK_CHARS + 100));
        assert!(msgs[1].starts_with("📩 (2/"));
        let words = |s: &str| s.matches("Umlauten").count();
        let total: usize = msgs.iter().map(|m| words(m)).sum();
        assert_eq!(total, words(&body));
    }

    #[test]
    fn huge_mail_is_capped() {
        let body = "x ".repeat(20_000);
        let msgs = format_email_notification("a@x.de", "Spam", &body, &[]);
        assert_eq!(msgs.len(), MAX_CHUNKS);
        assert!(msgs.last().unwrap().contains("vollständig im Dashboard"));
    }

    #[test]
    fn empty_body_says_so() {
        let msgs = format_email_notification("a@x.de", "Leer", "  \n ", &[]);
        assert!(msgs[0].contains("(kein Text)"));
    }
}
