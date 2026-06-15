//! Command implementations (spec §10). Each returns the process exit code.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::cli::*;
use crate::config::Config;
use crate::error::{CliError, CliResult};
use crate::exec::{self, ExecOptions};
use crate::export::{self, Shell};
use crate::merge::Source;
use crate::pipeline::{self, LoadOptions, MergedEnv, ResolvedEnv};
use crate::provider::{self, ProviderRegistry};
use crate::redact::{RedactionPolicy, REDACTED};
use crate::report::Reporter;
use crate::uri;

/// Shared context derived from global flags.
pub struct Ctx {
    pub config: Config,
    pub profile: String,
    pub reporter: Reporter,
    pub timeout: Duration,
    pub load: LoadOptions,
    pub redaction: RedactionPolicy,
}

impl Ctx {
    pub fn from_global(g: &GlobalArgs) -> CliResult<Ctx> {
        let config = Config::discover_and_load(
            g.config.as_deref(),
            std::env::var("DOTENV_CLOUD_CONFIG").ok(),
        )?;
        let profile = config.resolve_profile(g.profile.as_deref());
        let reporter = Reporter {
            verbose: g.verbose,
            quiet: g.quiet,
            no_color: g.no_color,
            strict: g.strict,
        };
        let load = LoadOptions {
            env_file: g.env_file.clone(),
            env_local_file: g.env_local_file.clone(),
            no_env_file: g.no_env_file,
            no_env_local: g.no_env_local,
            sets: g.parsed_sets()?,
        };
        let redaction = config.redaction_policy();
        Ok(Ctx {
            config,
            profile,
            reporter,
            timeout: g.timeout()?,
            load,
            redaction,
        })
    }

    fn merge(&self) -> CliResult<MergedEnv> {
        pipeline::merge(&self.config, &self.profile, &self.load)
    }

    async fn build_env(&self) -> CliResult<ResolvedEnv> {
        let merged = self.merge()?;
        // Warn on risky precedence (spec §5.2).
        for w in merged.precedence.safety_warnings() {
            self.reporter.warn(&w);
            if self.reporter.strict {
                return Err(CliError::Config(format!("strict: {w}")));
            }
        }
        let registry = ProviderRegistry::discover(&self.config).map_err(CliError::Config)?;
        pipeline::resolve(
            merged,
            &self.config,
            &self.profile,
            &registry,
            self.timeout,
            &self.reporter,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

pub async fn run(ctx: &Ctx, args: RunArgs) -> CliResult<i32> {
    if args.command.is_empty() {
        return Err(CliError::Usage(
            "missing command; use `dotenv-cloud run -- <command> [args...]`".into(),
        ));
    }

    let resolved = ctx.build_env().await?;

    // Required-key enforcement (spec §10.4).
    for key in &args.require {
        if !resolved.map.contains_key(key) {
            return Err(CliError::SecretResolution(format!(
                "required key `{key}` is not present after resolution"
            )));
        }
    }

    if args.dry_run || args.redact_summary {
        print_redacted_summary(ctx, &resolved);
        if args.dry_run {
            return Ok(0);
        }
    }

    let opts = ExecOptions {
        clear_env: args.clear_env,
        preserve: args.preserve,
    };
    exec::run(&args.command, &resolved.map, &opts)
}

fn print_redacted_summary(ctx: &Ctx, resolved: &ResolvedEnv) {
    eprintln!("resolved environment ({} keys):", resolved.map.len());
    for (key, info) in &resolved.info {
        let value = resolved.map.get(key).map(String::as_str).unwrap_or("");
        let shown = ctx.redaction.render(key, value, info.from_remote);
        let mut line = format!("  {key}={shown} source={}", info.winning_source);
        if !info.shadowed.is_empty() {
            let shadowed: Vec<String> = info.shadowed.iter().map(|s| s.to_string()).collect();
            line.push_str(&format!(" shadowed={}", shadowed.join(",")));
        }
        if let Some(r) = &info.reference_redacted {
            line.push_str(&format!(" ref={r}"));
        }
        eprintln!("{line}");
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

pub async fn export(ctx: &Ctx, args: ExportArgs) -> CliResult<i32> {
    let shell_name = args
        .shell
        .or(args.format)
        .unwrap_or_else(|| "bash".to_string());
    let shell = Shell::parse(&shell_name)
        .ok_or_else(|| CliError::Usage(format!("unsupported shell `{shell_name}`")))?;

    ctx.reporter
        .warn("export prints resolved secret values to stdout");

    let resolved = ctx.build_env().await?;
    let filtered = filter_keys(&resolved.map, &args.include, &args.exclude);

    if !args.no_comments {
        println!("# generated by dotenv-cloud; contains resolved secret values");
    }
    for (key, value) in &filtered {
        println!("{}", export::render_assignment(shell, key, value));
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

pub async fn build(ctx: &Ctx, args: BuildArgs) -> CliResult<i32> {
    ctx.reporter
        .warn("build materializes resolved secret values to output");

    let resolved = ctx.build_env().await?;
    let filtered = filter_keys(&resolved.map, &args.include, &args.exclude);

    let content = match args.mode.as_str() {
        "dotenv" => {
            let mut s = String::new();
            for (k, v) in &filtered {
                s.push_str(&export::render_dotenv_line(k, v));
                s.push('\n');
            }
            s
        }
        "json" => {
            let map: BTreeMap<&String, &String> = filtered.iter().collect();
            serde_json::to_string_pretty(&map).map_err(|e| CliError::Runtime(e.to_string()))? + "\n"
        }
        other => return Err(CliError::Usage(format!("invalid --mode `{other}`"))),
    };

    match &args.output {
        None => {
            print!("{content}");
            Ok(0)
        }
        Some(path) => {
            if path.exists() && !args.force {
                return Err(CliError::Runtime(format!(
                    "refusing to overwrite {} (use --force)",
                    path.display()
                )));
            }
            write_secure(path, &content, args.chmod.as_deref())?;
            ctx.reporter.info(&format!(
                "wrote {} keys to {}",
                filtered.len(),
                path.display()
            ));
            Ok(0)
        }
    }
}

fn write_secure(path: &PathBuf, content: &str, chmod: Option<&str>) -> CliResult<()> {
    std::fs::write(path, content)
        .map_err(|e| CliError::Runtime(format!("cannot write {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = chmod
            .and_then(|m| {
                u32::from_str_radix(m.trim_start_matches("0o").trim_start_matches('0'), 8).ok()
            })
            .unwrap_or(0o600);
        let mode = if mode == 0 { 0o600 } else { mode };
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)
            .map_err(|e| CliError::Runtime(format!("cannot chmod {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = chmod;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

pub async fn resolve_key(ctx: &Ctx, args: ResolveArgs) -> CliResult<i32> {
    let merged = ctx.merge()?;
    let registry = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
    let key = &args.key;

    let (value, info) = pipeline::resolve_one(
        &merged,
        key,
        &ctx.config,
        &ctx.profile,
        &registry,
        ctx.timeout,
    )
    .await?
    .ok_or_else(|| CliError::SecretResolution(format!("key `{key}` not found")))?;

    let provider = info
        .reference_redacted
        .as_deref()
        .and_then(|r| r.split("://").next())
        .map(|s| s.to_string());
    let source = info.winning_source;

    let shown = if args.show {
        value.clone()
    } else {
        ctx.redaction.render(key, &value, info.from_remote)
    };

    if args.json {
        let mut obj = serde_json::json!({
            "key": key,
            "value": if args.show { serde_json::Value::String(value.clone()) } else { serde_json::Value::String(REDACTED.to_string()) },
            "source": source.to_string(),
        });
        if let Some(p) = &provider {
            obj["provider"] = serde_json::Value::String(p.clone());
        }
        if args.source {
            obj["shadowed"] = serde_json::Value::Array(
                info.shadowed
                    .iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            );
        }
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
    } else {
        let mut line = format!("{key}={shown} source={source}");
        if let Some(p) = &provider {
            line.push_str(&format!(" provider={p}"));
        }
        if args.source && !info.shadowed.is_empty() {
            let s: Vec<String> = info.shadowed.iter().map(|x| x.to_string()).collect();
            line.push_str(&format!(" shadowed={}", s.join(",")));
        }
        println!("{line}");
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

pub async fn validate(ctx: &Ctx, args: ValidateArgs) -> CliResult<i32> {
    let mut problems: Vec<String> = Vec::new();
    let mut checks: Vec<String> = Vec::new();

    // Config + precedence.
    match ctx.config.precedence() {
        Ok(p) => {
            checks.push("precedence order valid".into());
            for w in p.safety_warnings() {
                problems.push(format!("warning: {w}"));
            }
        }
        Err(e) => problems.push(e.to_string()),
    }

    // Merge (parses dotenv, applies precedence).
    let merged = match ctx.merge() {
        Ok(m) => {
            checks.push("dotenv files parsed".into());
            Some(m)
        }
        Err(e) => {
            problems.push(e.to_string());
            None
        }
    };

    // Validate URI references among remote winners.
    let mut required_schemes: Vec<String> = Vec::new();
    if let Some(m) = &merged {
        for w in &m.winners {
            if w.winning_source == Source::Remote {
                match uri::parse(&w.value) {
                    Ok(r) => {
                        if !required_schemes.contains(&r.scheme) {
                            required_schemes.push(r.scheme.clone());
                        }
                    }
                    Err(e) => problems.push(format!("{}: {e}", w.key)),
                }
            }
        }
        checks.push(format!(
            "{} remote reference(s) validated",
            required_schemes.len()
        ));
    }

    // Provider availability.
    let registry = ProviderRegistry::discover(&ctx.config);
    match &registry {
        Ok(reg) => {
            for scheme in &required_schemes {
                if reg.has_scheme(scheme) {
                    checks.push(format!("provider available for scheme `{scheme}`"));
                } else {
                    problems.push(format!(
                        "no provider installed for scheme `{scheme}` (run `dotenv-cloud providers install {}`)",
                        provider::suggest_package(scheme)
                    ));
                }
            }
        }
        Err(e) => problems.push(e.clone()),
    }

    // Optional connectivity check.
    if args.providers && !args.no_providers {
        if let Ok(reg) = &registry {
            for scheme in &required_schemes {
                if let Some(p) = reg.provider_for_scheme(scheme) {
                    match crate::provider::host::PluginProcess::launch(p, ctx.timeout).await {
                        Ok(proc) => {
                            proc.shutdown().await;
                            checks.push(format!("provider `{}` handshake ok", p.manifest.name));
                        }
                        Err(e) => problems.push(format!(
                            "provider `{}` unreachable: {}",
                            p.manifest.name, e.message
                        )),
                    }
                }
            }
        }
    }

    let ok = problems.iter().all(|p| p.starts_with("warning:"));
    let hard_fail = !problems.iter().all(|p| p.starts_with("warning:"));

    if args.json {
        let obj = serde_json::json!({
            "status": if hard_fail { "error" } else { "ok" },
            "checks": checks,
            "problems": problems,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
    } else {
        for c in &checks {
            println!("ok: {c}");
        }
        for p in &problems {
            println!("{p}");
        }
        if ok && problems.is_empty() {
            println!("validation passed");
        }
    }

    if hard_fail || (ctx.reporter.strict && !problems.is_empty()) {
        return Ok(crate::error::ExitCode::Runtime.code());
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

pub async fn doctor(ctx: &Ctx) -> CliResult<i32> {
    println!("dotenv-cloud {}", env!("CARGO_PKG_VERSION"));
    println!(
        "platform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    match &ctx.config.source_path {
        Some(p) => println!("config: {}", p.display()),
        None => println!("config: (none; using defaults)"),
    }
    println!("active profile: {}", ctx.profile);

    let profile = ctx.config.profile(&ctx.profile);
    for (label, file) in [
        ("env_file", profile.env_file()),
        ("env_local_file", profile.env_local_file()),
    ] {
        let exists = std::path::Path::new(file).exists();
        println!(
            "  {label}: {file} ({})",
            if exists { "found" } else { "missing" }
        );
    }

    println!("provider directories:");
    for dir in crate::provider::manifest::discovery_dirs() {
        println!(
            "  {} ({})",
            dir.display(),
            if dir.exists() { "present" } else { "absent" }
        );
    }

    match ProviderRegistry::discover(&ctx.config) {
        Ok(reg) => {
            println!("installed providers: {}", reg.installed().len());
            for p in reg.installed() {
                println!(
                    "  {} v{} schemes={}",
                    p.manifest.name,
                    p.manifest.version,
                    p.manifest.schemes.join(",")
                );
            }
        }
        Err(e) => println!("provider discovery error: {e}"),
    }

    // Credential hints (no secrets printed).
    let aws_region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok();
    println!("AWS region detectable: {}", aws_region.is_some());
    println!(
        "VAULT_ADDR set: {}",
        std::env::var_os("VAULT_ADDR").is_some()
    );
    println!(
        "VAULT_TOKEN set: {}",
        std::env::var_os("VAULT_TOKEN").is_some()
    );
    println!("provider timeout: {:?}", ctx.timeout);
    Ok(0)
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

pub async fn init(ctx: &Ctx, args: InitArgs) -> CliResult<i32> {
    let merged = ctx.merge()?;
    let mut schemes: Vec<String> = Vec::new();
    for w in &merged.winners {
        if w.winning_source == Source::Remote {
            if let Ok(r) = uri::parse(&w.value) {
                if !schemes.contains(&r.scheme) {
                    schemes.push(r.scheme.clone());
                }
            }
        }
    }

    println!(
        "detected {} required scheme(s): {}",
        schemes.len(),
        schemes.join(", ")
    );

    let registry = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
    let mut missing = Vec::new();
    for s in &schemes {
        if registry.has_scheme(s) {
            println!("  {s}: provider installed");
        } else {
            println!(
                "  {s}: MISSING (package `{}`)",
                provider::suggest_package(s)
            );
            missing.push(s.clone());
        }
    }

    // Write a lockfile from currently-installed providers.
    let lockfile = args
        .lockfile
        .unwrap_or_else(|| PathBuf::from("dotenv-cloud.lock"));
    write_lockfile(&lockfile, &registry)?;
    println!("wrote {}", lockfile.display());

    if !missing.is_empty() {
        ctx.reporter.warn(&format!(
            "{} provider(s) missing. Registry download/verification is not available in this build; \
             install provider plugins into `.dotenv-cloud/providers` manually.",
            missing.len()
        ));
        let _ = (
            args.yes,
            args.project,
            args.user,
            args.registry,
            args.upgrade,
            args.offline,
        );
        return Ok(crate::error::ExitCode::Runtime.code());
    }
    Ok(0)
}

fn write_lockfile(path: &PathBuf, registry: &ProviderRegistry) -> CliResult<()> {
    let mut out = String::from("version = 1\n");
    for p in registry.installed() {
        out.push_str("\n[[provider]]\n");
        out.push_str(&format!("name = \"{}\"\n", p.manifest.name));
        out.push_str(&format!("version = \"{}\"\n", p.manifest.version));
        let schemes: Vec<String> = p
            .manifest
            .schemes
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect();
        out.push_str(&format!("schemes = [{}]\n", schemes.join(", ")));
        if let Some(sha) = p.manifest.integrity.as_ref().and_then(|i| i.sha256.clone()) {
            out.push_str(&format!("sha256 = \"{sha}\"\n"));
        }
    }
    std::fs::write(path, out)
        .map_err(|e| CliError::Runtime(format!("cannot write {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

pub async fn providers(ctx: &Ctx, args: ProvidersArgs) -> CliResult<i32> {
    match args.command {
        ProvidersCommand::List(a) => providers_list(ctx, a),
        ProvidersCommand::Info(a) => providers_info(ctx, a),
        ProvidersCommand::Remove(a) => providers_remove(ctx, a),
        ProvidersCommand::Search { query } => {
            ctx.reporter.warn(&format!(
                "registry search is not available in this build (query `{query}`)"
            ));
            Ok(crate::error::ExitCode::Runtime.code())
        }
        ProvidersCommand::Install(a) => {
            ctx.reporter.warn(&format!(
                "registry install is not available in this build; install `{}` plugin into `.dotenv-cloud/providers` manually",
                a.name
            ));
            Ok(crate::error::ExitCode::Runtime.code())
        }
        ProvidersCommand::Update(a) => {
            ctx.reporter.warn(&format!(
                "registry update is not available in this build{}",
                a.name.map(|n| format!(" ({n})")).unwrap_or_default()
            ));
            Ok(crate::error::ExitCode::Runtime.code())
        }
    }
}

fn providers_list(ctx: &Ctx, a: ProvidersCommonArgs) -> CliResult<i32> {
    let registry = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
    if a.json {
        let arr: Vec<_> = registry
            .installed()
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.manifest.name,
                    "version": p.manifest.version,
                    "schemes": p.manifest.schemes,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap());
    } else if registry.installed().is_empty() {
        println!("no providers installed");
    } else {
        for p in registry.installed() {
            println!(
                "{:<32} installed   version={:<8} schemes={}",
                p.manifest.name,
                p.manifest.version,
                p.manifest.schemes.join(",")
            );
        }
    }
    Ok(0)
}

fn providers_info(ctx: &Ctx, a: ProvidersTargetArgs) -> CliResult<i32> {
    let registry = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
    let found = registry.installed().iter().find(|p| {
        p.manifest.name == a.name || p.manifest.name == format!("dotenv-cloud-provider-{}", a.name)
    });
    match found {
        None => Err(CliError::Runtime(format!(
            "provider `{}` is not installed",
            a.name
        ))),
        Some(p) => {
            if a.json {
                let obj = serde_json::json!({
                    "name": p.manifest.name,
                    "version": p.manifest.version,
                    "protocol_version": p.manifest.protocol_version,
                    "schemes": p.manifest.schemes,
                    "executable": p.executable_path().display().to_string(),
                    "description": p.manifest.description,
                });
                println!("{}", serde_json::to_string_pretty(&obj).unwrap());
            } else {
                println!("name: {}", p.manifest.name);
                println!("version: {}", p.manifest.version);
                println!("protocol_version: {}", p.manifest.protocol_version);
                println!("schemes: {}", p.manifest.schemes.join(", "));
                println!("executable: {}", p.executable_path().display());
                if let Some(d) = &p.manifest.description {
                    println!("description: {d}");
                }
            }
            Ok(0)
        }
    }
}

fn providers_remove(ctx: &Ctx, a: ProvidersTargetArgs) -> CliResult<i32> {
    let registry = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
    let found = registry
        .installed()
        .iter()
        .find(|p| {
            p.manifest.name == a.name
                || p.manifest.name == format!("dotenv-cloud-provider-{}", a.name)
        })
        .cloned();
    match found {
        None => Err(CliError::Runtime(format!(
            "provider `{}` is not installed",
            a.name
        ))),
        Some(p) => {
            if !a.yes {
                ctx.reporter
                    .warn(&format!("re-run with --yes to remove {}", p.dir.display()));
                return Ok(crate::error::ExitCode::Runtime.code());
            }
            std::fs::remove_dir_all(&p.dir).map_err(|e| {
                CliError::Runtime(format!("cannot remove {}: {e}", p.dir.display()))
            })?;
            println!("removed {}", p.manifest.name);
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn filter_keys(
    map: &BTreeMap<String, String>,
    include: &[String],
    exclude: &[String],
) -> BTreeMap<String, String> {
    map.iter()
        .filter(|(k, _)| include.is_empty() || include.contains(k))
        .filter(|(k, _)| !exclude.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
