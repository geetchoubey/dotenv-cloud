//! Minimal interactive prompt helpers for `init` (spec §10.1).
//!
//! Hand-rolled to avoid a dependency: prompts are written to stderr (so stdout
//! stays clean for any machine-readable output) and answers are read line-by-line
//! from stdin. Callers should gate interactive flows on [`is_interactive`] and
//! fall back to non-interactive behavior when stdin is not a TTY (e.g. CI).

use std::io::{self, IsTerminal, Write};

use crate::error::{CliError, CliResult};

/// Whether stdin is connected to a terminal, i.e. a human can answer prompts.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal()
}

/// Read one trimmed line from stdin, or an error if input closed.
fn read_line() -> CliResult<String> {
    let mut s = String::new();
    let n = io::stdin()
        .read_line(&mut s)
        .map_err(|e| CliError::Runtime(format!("cannot read input: {e}")))?;
    if n == 0 {
        return Err(CliError::Usage(
            "unexpected end of input while prompting".into(),
        ));
    }
    Ok(s.trim().to_string())
}

/// Prompt for free text. Returns `default` when the user enters nothing and a
/// default is given.
pub fn text(label: &str, default: Option<&str>) -> CliResult<String> {
    match default {
        Some(d) => eprint!("{label} [{d}]: "),
        None => eprint!("{label}: "),
    }
    io::stderr().flush().ok();
    let answer = read_line()?;
    if answer.is_empty() {
        if let Some(d) = default {
            return Ok(d.to_string());
        }
    }
    Ok(answer)
}

/// Prompt for a yes/no answer.
pub fn confirm(label: &str, default: bool) -> CliResult<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        eprint!("{label} {hint}: ");
        io::stderr().flush().ok();
        let answer = read_line()?.to_ascii_lowercase();
        match answer.as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => eprintln!("please answer y or n"),
        }
    }
}

/// Present a numbered list and let the user pick one or more by entering a
/// comma/space-separated list of indices (e.g. `1,3`). An empty answer selects
/// nothing. Returns the chosen options' indices, in input order, de-duplicated.
pub fn select_many(label: &str, options: &[String]) -> CliResult<Vec<usize>> {
    eprintln!("{label}");
    for (i, opt) in options.iter().enumerate() {
        eprintln!("  {}) {opt}", i + 1);
    }
    eprint!("select (comma-separated numbers, empty for none): ");
    io::stderr().flush().ok();
    let answer = read_line()?;

    let mut chosen: Vec<usize> = Vec::new();
    for tok in answer.split([',', ' ']).filter(|t| !t.is_empty()) {
        let n: usize = tok.parse().map_err(|_| {
            CliError::Usage(format!("invalid selection `{tok}`; enter list numbers"))
        })?;
        if n == 0 || n > options.len() {
            return Err(CliError::Usage(format!(
                "selection {n} is out of range (1..={})",
                options.len()
            )));
        }
        let idx = n - 1;
        if !chosen.contains(&idx) {
            chosen.push(idx);
        }
    }
    Ok(chosen)
}
