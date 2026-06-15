//! Dotenv parser (spec §16).
//!
//! Supports `KEY=value`, quoted values (single/double), escape sequences in
//! double quotes, `export` prefixes, blank lines, full-line comments, and
//! inline comments on unquoted/double-quoted values. Malformed input produces
//! precise file:line diagnostics.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotenvEntry {
    pub key: String,
    pub value: String,
    pub line: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("{file}:{line}: {message}")]
pub struct DotenvParseError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

/// Parse dotenv `content` originating from `file_label` (used in diagnostics).
pub fn parse(content: &str, file_label: &str) -> Result<Vec<DotenvEntry>, DotenvParseError> {
    let mut out = Vec::new();
    let mut line_no = 0usize;
    let mut lines = content.lines().enumerate().peekable();

    while let Some((idx, raw)) = lines.next() {
        line_no = idx + 1;
        let line = raw.trim_start();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Optional `export ` prefix.
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);

        let eq = line.find('=').ok_or_else(|| DotenvParseError {
            file: file_label.to_string(),
            line: line_no,
            message: format!(
                "invalid assignment: expected KEY=VALUE, got `{}`",
                raw.trim()
            ),
        })?;

        let key = line[..eq].trim();
        validate_key(key, file_label, line_no)?;

        let rest = &line[eq + 1..];
        let value = parse_value(rest, file_label, line_no, &mut lines)?;

        out.push(DotenvEntry {
            key: key.to_string(),
            value,
            line: line_no,
        });
    }

    let _ = line_no;
    Ok(out)
}

/// Read and parse a dotenv file from disk.
pub fn parse_file(path: &Path) -> Result<Vec<DotenvEntry>, DotenvParseError> {
    let label = path.display().to_string();
    let content = std::fs::read_to_string(path).map_err(|e| DotenvParseError {
        file: label.clone(),
        line: 0,
        message: format!("cannot read file: {e}"),
    })?;
    parse(&content, &label)
}

fn validate_key(key: &str, file: &str, line: usize) -> Result<(), DotenvParseError> {
    if key.is_empty() {
        return Err(DotenvParseError {
            file: file.to_string(),
            line,
            message: "empty key before `=`".to_string(),
        });
    }
    let valid = key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    let first_ok = key
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false);
    if !valid || !first_ok {
        return Err(DotenvParseError {
            file: file.to_string(),
            line,
            message: format!("invalid key name: `{key}`"),
        });
    }
    Ok(())
}

/// Parse the value portion (text after `=`). Handles quoting and inline
/// comments. Multi-line double-quoted values are supported by consuming
/// subsequent lines from `lines`.
fn parse_value<'a, I>(
    rest: &str,
    file: &str,
    line: usize,
    lines: &mut std::iter::Peekable<I>,
) -> Result<String, DotenvParseError>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Ok(String::new());
    }

    let bytes = rest.as_bytes();
    match bytes[0] {
        b'\'' => parse_single_quoted(rest, file, line, lines),
        b'"' => parse_double_quoted(rest, file, line, lines),
        _ => Ok(parse_unquoted(rest)),
    }
}

/// Unquoted value: trim trailing whitespace, strip inline `#` comment that is
/// preceded by whitespace.
fn parse_unquoted(rest: &str) -> String {
    let mut end = rest.len();
    let chars: Vec<char> = rest.chars().collect();
    let mut prev_ws = true; // a `#` at column 0 (after `=`) is a comment start
    let mut byte_idx = 0usize;
    for (i, c) in chars.iter().enumerate() {
        if *c == '#' && prev_ws {
            end = byte_idx;
            let _ = i;
            break;
        }
        prev_ws = c.is_whitespace();
        byte_idx += c.len_utf8();
    }
    rest[..end].trim_end().to_string()
}

fn parse_single_quoted<'a, I>(
    rest: &str,
    file: &str,
    line: usize,
    lines: &mut std::iter::Peekable<I>,
) -> Result<String, DotenvParseError>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    // Single quotes are literal: no escape processing.
    let inner = &rest[1..];
    if let Some(close) = inner.find('\'') {
        return Ok(inner[..close].to_string());
    }
    // Multi-line single-quoted.
    let mut buf = String::from(inner);
    buf.push('\n');
    for (_, l) in lines.by_ref() {
        if let Some(close) = l.find('\'') {
            buf.push_str(&l[..close]);
            return Ok(buf);
        }
        buf.push_str(l);
        buf.push('\n');
    }
    Err(DotenvParseError {
        file: file.to_string(),
        line,
        message: "unterminated single-quoted value".to_string(),
    })
}

fn parse_double_quoted<'a, I>(
    rest: &str,
    file: &str,
    line: usize,
    lines: &mut std::iter::Peekable<I>,
) -> Result<String, DotenvParseError>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let inner = &rest[1..];
    if let Some((value, _trailing)) = scan_double(inner) {
        return Ok(value);
    }
    // Multi-line double-quoted: accumulate until a closing unescaped quote.
    let mut acc = String::from(inner);
    acc.push('\n');
    for (_, l) in lines.by_ref() {
        if let Some((value, _trailing)) = scan_double(&acc_concat(&acc, l)) {
            return Ok(value);
        }
        acc.push_str(l);
        acc.push('\n');
    }
    Err(DotenvParseError {
        file: file.to_string(),
        line,
        message: "unterminated double-quoted value".to_string(),
    })
}

fn acc_concat(acc: &str, l: &str) -> String {
    let mut s = String::with_capacity(acc.len() + l.len());
    s.push_str(acc);
    s.push_str(l);
    s
}

/// Scan a double-quoted body (without the opening quote) for the closing
/// unescaped quote, applying escape sequences. Returns (decoded value,
/// trailing text) or `None` if no closing quote was found.
fn scan_double(inner: &str) -> Option<(String, String)> {
    let mut out = String::new();
    let mut chars = inner.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, next)) = chars.next() {
                    match next {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        '\\' => out.push('\\'),
                        '"' => out.push('"'),
                        '\'' => out.push('\''),
                        '0' => out.push('\0'),
                        other => {
                            out.push('\\');
                            out.push(other);
                        }
                    }
                } else {
                    out.push('\\');
                }
            }
            '"' => {
                let trailing = inner[i + 1..].to_string();
                return Some((out, trailing));
            }
            other => out.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Vec<DotenvEntry> {
        parse(s, ".env").unwrap()
    }

    #[test]
    fn basic_assignments() {
        let e = p("KEY=value\nPORT=3000");
        assert_eq!(e[0].key, "KEY");
        assert_eq!(e[0].value, "value");
        assert_eq!(e[1].value, "3000");
    }

    #[test]
    fn export_prefix_and_comments() {
        let e = p("# comment\nexport KEY=value\n\nFOO=bar # inline");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].key, "KEY");
        assert_eq!(e[1].value, "bar");
    }

    #[test]
    fn quoting() {
        let e = p("A=\"quoted value\"\nB='single quoted'\nC=\"a\\nb\"");
        assert_eq!(e[0].value, "quoted value");
        assert_eq!(e[1].value, "single quoted");
        assert_eq!(e[2].value, "a\nb");
    }

    #[test]
    fn hash_inside_double_quotes_kept() {
        let e = p("A=\"value # not comment\"");
        assert_eq!(e[0].value, "value # not comment");
    }

    #[test]
    fn value_with_spaces_unquoted() {
        let e = p("A=value with spaces");
        assert_eq!(e[0].value, "value with spaces");
    }

    #[test]
    fn remote_uri_value_preserved() {
        let e = p("DB_PASSWORD=aws-sm://prod/db/password");
        assert_eq!(e[0].value, "aws-sm://prod/db/password");
    }

    #[test]
    fn malformed_reports_line() {
        let err = parse("OK=1\nthis is bad", ".env").unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn empty_value() {
        let e = p("EMPTY=");
        assert_eq!(e[0].value, "");
    }
}
