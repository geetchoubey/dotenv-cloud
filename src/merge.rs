//! Source precedence model and merge engine (spec §5, §17).
//!
//! Builds a per-key source map, applies a configured precedence order, and
//! records shadowed sources for diagnostics. Remote secrets are *not* a file
//! source: a winning local value that is a URI is resolved and placed at the
//! `remote` precedence level (spec §5.1).

use std::collections::BTreeMap;
use std::fmt;

/// A value source, highest-to-lowest default precedence (spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Cli,
    System,
    Remote,
    EnvLocal,
    Env,
    Defaults,
}

impl Source {
    /// The default precedence order from highest to lowest.
    pub const DEFAULT_ORDER: [Source; 6] = [
        Source::Cli,
        Source::System,
        Source::Remote,
        Source::EnvLocal,
        Source::Env,
        Source::Defaults,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Source::Cli => "cli",
            Source::System => "system",
            Source::Remote => "remote",
            Source::EnvLocal => "env.local",
            Source::Env => "env",
            Source::Defaults => "defaults",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        Some(match s {
            "cli" => Source::Cli,
            "system" => Source::System,
            "remote" => Source::Remote,
            "env.local" => Source::EnvLocal,
            "env" => Source::Env,
            "defaults" => Source::Defaults,
            _ => return None,
        })
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// A precedence order: a ranked list of sources, highest first. Sources omitted
/// from a configured order keep their default relative order below all listed
/// ones (spec §5.2).
#[derive(Debug, Clone)]
pub struct Precedence {
    order: Vec<Source>,
}

impl Default for Precedence {
    fn default() -> Self {
        Precedence {
            order: Source::DEFAULT_ORDER.to_vec(),
        }
    }
}

impl Precedence {
    /// Build from an explicit (possibly partial) order. Omitted sources are
    /// appended in default relative order. Duplicates are an error.
    pub fn from_order(listed: &[Source]) -> Result<Self, String> {
        let mut seen = Vec::new();
        for s in listed {
            if seen.contains(s) {
                return Err(format!("source `{s}` listed more than once in precedence"));
            }
            seen.push(*s);
        }
        let mut order = listed.to_vec();
        for s in Source::DEFAULT_ORDER {
            if !order.contains(&s) {
                order.push(s);
            }
        }
        Ok(Precedence { order })
    }

    /// Rank of a source: lower is higher precedence.
    pub fn rank(&self, source: Source) -> usize {
        self.order
            .iter()
            .position(|s| *s == source)
            .unwrap_or(usize::MAX)
    }

    #[allow(dead_code)] // used by tests and diagnostics tooling.
    pub fn order(&self) -> &[Source] {
        &self.order
    }

    /// Safety warnings (spec §5.2): `remote` above `cli`, or `system` below `env`.
    pub fn safety_warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.rank(Source::Remote) < self.rank(Source::Cli) {
            w.push(
                "`remote` is configured above `cli`; resolved secrets will override CLI flags"
                    .into(),
            );
        }
        if self.rank(Source::System) > self.rank(Source::Env) {
            w.push("`system` is configured below `.env`; file values will override the process environment".into());
        }
        w
    }
}

/// A single candidate value for a key from one source.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Effective precedence source. For remote URI values this is
    /// [`Source::Remote`] even though the text lives in a file (spec §5.1).
    pub source: Source,
    /// The source the value literally came from (file/defaults/cli/system).
    pub origin: Source,
    pub value: String,
}

/// The merge result for one key.
#[derive(Debug, Clone)]
pub struct MergedValue {
    pub key: String,
    pub value: String,
    pub winning_source: Source,
    /// The literal origin of the winning value (for diagnostics).
    pub origin: Source,
    /// Lower-precedence sources that were shadowed, highest-first.
    pub shadowed: Vec<Source>,
}

/// Accumulates candidates per key and resolves winners by precedence.
#[derive(Debug, Default)]
pub struct MergeEngine {
    candidates: BTreeMap<String, Vec<Candidate>>,
}

impl MergeEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a candidate whose effective and literal source are the same.
    pub fn add(&mut self, key: impl Into<String>, source: Source, value: impl Into<String>) {
        self.add_with_origin(key, source, source, value);
    }

    /// Add a candidate with a distinct effective `source` and literal `origin`.
    /// Used for remote promotion (spec §5.1): a URI in `.env` is added with
    /// `source = Remote` and `origin = Env`.
    pub fn add_with_origin(
        &mut self,
        key: impl Into<String>,
        source: Source,
        origin: Source,
        value: impl Into<String>,
    ) {
        self.candidates
            .entry(key.into())
            .or_default()
            .push(Candidate {
                source,
                origin,
                value: value.into(),
            });
    }

    /// Resolve winners for all keys under the given precedence.
    pub fn resolve(&self, precedence: &Precedence) -> Vec<MergedValue> {
        let mut out = Vec::new();
        for (key, cands) in &self.candidates {
            let mut sorted = cands.clone();
            // Stable sort: equal ranks keep insertion order, so among equally
            // ranked remote candidates the one added first (higher origin
            // precedence) wins.
            sorted.sort_by_key(|c| precedence.rank(c.source));
            let winner = &sorted[0];
            let shadowed = sorted[1..].iter().map(|c| c.origin).collect();
            out.push(MergedValue {
                key: key.clone(),
                value: winner.value.clone(),
                winning_source: winner.source,
                origin: winner.origin,
                shadowed,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_precedence_cli_wins() {
        let mut m = MergeEngine::new();
        m.add("PORT", Source::Env, "3000");
        m.add("PORT", Source::Cli, "8080");
        m.add("PORT", Source::EnvLocal, "4000");
        let r = m.resolve(&Precedence::default());
        let port = r.iter().find(|v| v.key == "PORT").unwrap();
        assert_eq!(port.value, "8080");
        assert_eq!(port.winning_source, Source::Cli);
        assert!(port.shadowed.contains(&Source::Env));
    }

    #[test]
    fn partial_order_appends_defaults() {
        let p = Precedence::from_order(&[Source::Cli, Source::Remote]).unwrap();
        // remote should now rank above system
        assert!(p.rank(Source::Remote) < p.rank(Source::System));
        assert!(p.rank(Source::Cli) < p.rank(Source::Remote));
    }

    #[test]
    fn duplicate_order_rejected() {
        assert!(Precedence::from_order(&[Source::Cli, Source::Cli]).is_err());
    }

    #[test]
    fn safety_warnings_fire() {
        let p = Precedence::from_order(&[Source::Remote, Source::Cli]).unwrap();
        assert!(!p.safety_warnings().is_empty());
    }
}
