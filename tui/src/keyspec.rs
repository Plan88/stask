//! The canonical text notation for key sequences, shared by the config
//! file, the help list and the footer. Plain characters concatenate
//! (`gg`); special keys use angle brackets (`<enter>`, `<alt-j>`); a
//! literal `<` is written `<lt>` so `<` can unambiguously start a name.

use thiserror::Error;

use crate::key::{Key, KeySeq};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("empty key sequence")]
    Empty,
    #[error("unclosed `<` in `{0}` (write a literal `<` as `<lt>`)")]
    UnclosedAngle(String),
    #[error("unknown key name `<{0}>`")]
    UnknownName(String),
}

/// Renders a key sequence in the canonical notation.
pub fn format_seq(seq: &KeySeq) -> String {
    seq.as_slice().iter().map(format_key).collect()
}

pub fn format_key(key: &Key) -> String {
    match key {
        // `<` must round-trip through its name form because a bare `<`
        // would start a special-key name when parsed back.
        Key::Char('<') => "<lt>".to_string(),
        Key::Char(c) => c.to_string(),
        Key::Alt(c) => format!("<alt-{c}>"),
        Key::Ctrl(c) => format!("<ctrl-{c}>"),
        Key::Enter => "<enter>".to_string(),
        Key::Esc => "<esc>".to_string(),
        Key::Backspace => "<backspace>".to_string(),
        Key::Tab => "<tab>".to_string(),
        Key::Left => "<left>".to_string(),
        Key::Right => "<right>".to_string(),
    }
}

/// Parses the canonical notation back into a key sequence.
pub fn parse_seq(spec: &str) -> Result<KeySeq, ParseError> {
    let mut keys = Vec::new();
    let mut chars = spec.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('>') => break,
                    Some(c) => name.push(c),
                    None => return Err(ParseError::UnclosedAngle(spec.to_string())),
                }
            }
            keys.push(key_from_name(&name)?);
        } else {
            keys.push(Key::Char(c));
        }
    }
    if keys.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(KeySeq::from_keys(keys))
}

fn key_from_name(name: &str) -> Result<Key, ParseError> {
    if let Some(rest) = name.strip_prefix("alt-") {
        return chord_char(rest)
            .map(Key::Alt)
            .ok_or_else(|| ParseError::UnknownName(name.to_string()));
    }
    if let Some(rest) = name.strip_prefix("ctrl-") {
        return chord_char(rest)
            .map(Key::Ctrl)
            .ok_or_else(|| ParseError::UnknownName(name.to_string()));
    }
    match name {
        "lt" => Ok(Key::Char('<')),
        "enter" => Ok(Key::Enter),
        "esc" => Ok(Key::Esc),
        "backspace" => Ok(Key::Backspace),
        "tab" => Ok(Key::Tab),
        "left" => Ok(Key::Left),
        "right" => Ok(Key::Right),
        _ => Err(ParseError::UnknownName(name.to_string())),
    }
}

/// The chord's single character; None rejects empty or multi-character
/// chord names, which have no Key form.
fn chord_char(rest: &str) -> Option<char> {
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests formatting of plain character sequences.
    // Given: the sequences "gg" and "?" built from plain characters
    // When: they are formatted
    // Then: the characters concatenate with no separators or brackets
    #[test]
    fn plain_characters_concatenate() {
        assert_eq!(format_seq(&KeySeq::chars("gg")), "gg");
        assert_eq!(format_seq(&KeySeq::chars("?")), "?");
    }

    // Tests the notation of every special key.
    // Given: each non-character key and an Alt chord
    // When: each is formatted
    // Then: it renders as its bracketed lowercase name
    #[test]
    fn special_keys_use_bracketed_names() {
        assert_eq!(format_key(&Key::Enter), "<enter>");
        assert_eq!(format_key(&Key::Esc), "<esc>");
        assert_eq!(format_key(&Key::Backspace), "<backspace>");
        assert_eq!(format_key(&Key::Tab), "<tab>");
        assert_eq!(format_key(&Key::Left), "<left>");
        assert_eq!(format_key(&Key::Right), "<right>");
        assert_eq!(format_key(&Key::Alt('j')), "<alt-j>");
        assert_eq!(format_key(&Key::Ctrl('d')), "<ctrl-d>");
    }

    // Tests the notation of the `<` character itself.
    // Given: the key `<` (bound to outdent by default) and the plain `>`
    // When: they are formatted
    // Then: `<` becomes its name form `<lt>` while `>` stays literal,
    //       because only `<` can start a bracketed name
    #[test]
    fn literal_angle_brackets() {
        assert_eq!(format_key(&Key::Char('<')), "<lt>");
        assert_eq!(format_key(&Key::Char('>')), ">");
    }

    // Tests that parsing inverts formatting for representative sequences.
    // Given: sequences covering plain chars, every special key, an Alt
    //        chord and the `<lt>` name form
    // When: each is formatted and parsed back
    // Then: the original sequence is recovered exactly
    #[test]
    fn parse_inverts_format() {
        let seqs = [
            KeySeq::chars("gg"),
            KeySeq::chars("<"),
            KeySeq::chars(">"),
            KeySeq::from(Key::Enter),
            KeySeq::from(Key::Esc),
            KeySeq::from(Key::Backspace),
            KeySeq::from(Key::Tab),
            KeySeq::from(Key::Left),
            KeySeq::from(Key::Right),
            KeySeq::from(Key::Alt('x')),
            KeySeq::from(Key::Ctrl('u')),
            KeySeq::from_keys(vec![Key::Char('g'), Key::Tab, Key::Alt('j')]),
        ];

        for seq in seqs {
            assert_eq!(parse_seq(&format_seq(&seq)), Ok(seq.clone()), "{seq:?}");
        }
    }

    // Tests parsing of a mixed multi-key spec written by hand.
    // Given: the spec "g<tab><alt-j>"
    // When: it is parsed
    // Then: it yields the plain char, the special key and the Alt chord
    #[test]
    fn parse_reads_mixed_notation() {
        assert_eq!(
            parse_seq("g<tab><alt-j>"),
            Ok(KeySeq::from_keys(vec![
                Key::Char('g'),
                Key::Tab,
                Key::Alt('j')
            ]))
        );
    }

    // Tests rejection of malformed specs.
    // Given: an empty spec, an unclosed `<`, an unknown name, and Alt
    //        names with zero or several characters
    // When: each is parsed
    // Then: each yields its specific error instead of guessing
    #[test]
    fn parse_rejects_malformed_specs() {
        assert_eq!(parse_seq(""), Err(ParseError::Empty));
        assert_eq!(
            parse_seq("a<enter"),
            Err(ParseError::UnclosedAngle("a<enter".to_string()))
        );
        assert_eq!(
            parse_seq("<return>"),
            Err(ParseError::UnknownName("return".to_string()))
        );
        assert_eq!(
            parse_seq("<alt->"),
            Err(ParseError::UnknownName("alt-".to_string()))
        );
        assert_eq!(
            parse_seq("<alt-jk>"),
            Err(ParseError::UnknownName("alt-jk".to_string()))
        );
        assert_eq!(
            parse_seq("<ctrl->"),
            Err(ParseError::UnknownName("ctrl-".to_string()))
        );
        assert_eq!(
            parse_seq("<ctrl-jk>"),
            Err(ParseError::UnknownName("ctrl-jk".to_string()))
        );
    }
}
