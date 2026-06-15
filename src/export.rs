//! Shell export rendering with safe quoting (spec §10.5).
//!
//! `export` and `build` intentionally emit resolved secret values; quoting here
//! must be robust against injection. Each shell has its own escaping rules.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    pub fn parse(s: &str) -> Option<Shell> {
        Some(match s {
            "bash" => Shell::Bash,
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            "powershell" | "pwsh" | "ps1" => Shell::PowerShell,
            _ => return None,
        })
    }
}

/// Render a single `KEY=VALUE` assignment for the given shell.
pub fn render_assignment(shell: Shell, key: &str, value: &str) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => format!("export {key}={}", sq_posix(value)),
        Shell::Fish => format!("set -gx {key} {};", sq_posix(value)),
        Shell::PowerShell => format!("$Env:{key} = {}", sq_powershell(value)),
    }
}

/// POSIX single-quote escaping: wrap in `'...'`, and encode embedded single
/// quotes as `'\''`.
fn sq_posix(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// PowerShell single-quote escaping: wrap in `'...'`, double embedded `'`.
fn sq_powershell(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Render a dotenv-format line for `build --mode dotenv`. Values are quoted with
/// double quotes and escaped so the output re-parses cleanly.
pub fn render_dotenv_line(key: &str, value: &str) -> String {
    let needs_quote = value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '\\' | '$'));
    if !needs_quote {
        format!("{key}={value}")
    } else {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("{key}=\"{escaped}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_quoting() {
        assert_eq!(
            render_assignment(Shell::Bash, "A", "simple"),
            "export A='simple'"
        );
        assert_eq!(
            render_assignment(Shell::Bash, "A", "has'quote"),
            "export A='has'\\''quote'"
        );
    }

    #[test]
    fn fish_and_powershell() {
        assert_eq!(render_assignment(Shell::Fish, "A", "v"), "set -gx A 'v';");
        assert_eq!(
            render_assignment(Shell::PowerShell, "A", "o'brien"),
            "$Env:A = 'o''brien'"
        );
    }

    #[test]
    fn dotenv_quoting() {
        assert_eq!(render_dotenv_line("A", "plain"), "A=plain");
        assert_eq!(render_dotenv_line("A", "has space"), "A=\"has space\"");
        assert_eq!(render_dotenv_line("A", "a\"b"), "A=\"a\\\"b\"");
    }
}
