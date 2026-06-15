//! Diagnostic output helpers. All diagnostics go to stderr so stdout stays
//! clean for `export`/`build` output. Never prints secret material.

#[derive(Debug, Clone, Default)]
pub struct Reporter {
    pub verbose: bool,
    pub quiet: bool,
    pub no_color: bool,
    pub strict: bool,
}

impl Reporter {
    pub fn warn(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{}: {msg}", self.paint("warning", "33"));
        }
    }

    pub fn error(&self, msg: &str) {
        eprintln!("{}: {msg}", self.paint("error", "31"));
    }

    pub fn info(&self, msg: &str) {
        if self.verbose && !self.quiet {
            eprintln!("{msg}");
        }
    }

    fn paint(&self, text: &str, code: &str) -> String {
        if self.no_color || std::env::var_os("NO_COLOR").is_some() {
            text.to_string()
        } else {
            format!("\u{1b}[{code}m{text}\u{1b}[0m")
        }
    }
}
