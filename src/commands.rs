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
use crate::provider::registry::{self, Installer, Scope};
use crate::provider::{self, ProviderRegistry};
use crate::redact::{RedactionPolicy, REDACTED};
use crate::report::Reporter;
use crate::uri;

/// Default provider registry when none is configured (spec §9, §19).
const DEFAULT_REGISTRY_URL: &str = "https://geetchoubey.github.io/dotenv-cloud/index.json";

/// Resolve the registry URL: explicit flag > config > built-in default.
fn registry_url(ctx: &Ctx, flag: Option<&str>) -> String {
    flag.map(String::from)
        .or_else(|| {
            ctx.config
                .provider_registry
                .as_ref()
                .and_then(|r| r.url.clone())
        })
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string())
}

/// Resolve the install scope from flags (default: project).
fn install_scope(project: bool, user: bool) -> Scope {
    if user && !project {
        Scope::User
    } else {
        Scope::Project
    }
}

/// Whether unsigned installs are allowed (flag OR config).
fn allow_unsigned(ctx: &Ctx, flag: bool) -> bool {
    flag || ctx
        .config
        .provider_registry
        .as_ref()
        .map(|r| r.allow_unsigned)
        .unwrap_or(false)
}

fn registry_public_key(ctx: &Ctx) -> Option<String> {
    ctx.config
        .provider_registry
        .as_ref()
        .and_then(|r| r.public_key.clone())
}

/// Build an installer by loading the registry index.
fn make_installer(ctx: &Ctx, url: &str, unsigned: bool) -> CliResult<Installer> {
    Installer::load(url, unsigned, registry_public_key(ctx)).map_err(CliError::Runtime)
}

/// Install one provider and record it in the lockfile at `lock`.
fn install_one(
    ctx: &Ctx,
    installer: &Installer,
    name_spec: &str,
    scope: Scope,
    lock: &std::path::Path,
) -> CliResult<()> {
    let record = installer
        .install(name_spec, scope)
        .map_err(CliError::Runtime)?;
    registry::upsert_lockfile(lock, &record).map_err(CliError::Runtime)?;
    ctx.reporter.info(&format!("updated {}", lock.display()));
    println!(
        "installed {} v{} (schemes: {})",
        record.package,
        record.version,
        record.schemes.join(", ")
    );
    Ok(())
}

/// Detect remote schemes whose provider is not installed and install them from
/// the registry (used by `run --install-missing-providers`). Signature policy
/// follows config; unsigned installs require `allow_unsigned`.
fn install_missing_providers(ctx: &Ctx) -> CliResult<()> {
    let merged = ctx.merge()?;
    let installed = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;

    let mut packages: Vec<String> = Vec::new();
    for w in &merged.winners {
        if w.winning_source == Source::Remote {
            if let Ok(r) = uri::parse(&w.value) {
                if !installed.has_scheme(&r.scheme) {
                    let pkg = provider::suggest_package(&r.scheme).to_string();
                    if !packages.contains(&pkg) {
                        packages.push(pkg);
                    }
                }
            }
        }
    }
    if packages.is_empty() {
        return Ok(());
    }

    let url = registry_url(ctx, None);
    let installer = make_installer(ctx, &url, allow_unsigned(ctx, false))?;
    let lock = PathBuf::from("dotenv-cloud.lock");
    for pkg in &packages {
        install_one(ctx, &installer, pkg, Scope::Project, &lock)?;
    }
    Ok(())
}

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

    if args.install_missing_providers {
        install_missing_providers(ctx)?;
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

    // Build the overlay applied on top of the child's environment. The child
    // already inherits the process (`system`) environment (unless --clear-env),
    // so we only set a project value when it should win over an inherited
    // system variable: always if its source outranks `system` in precedence (or
    // the env is being cleared), otherwise only when no system value exists to
    // defer to.
    let sys_rank = resolved.precedence.rank(Source::System);
    let mut child_env = BTreeMap::new();
    for (key, value) in &resolved.map {
        let source = resolved
            .info
            .get(key)
            .map(|i| i.winning_source)
            .unwrap_or(Source::Env);
        let outranks_system = resolved.precedence.rank(source) < sys_rank;
        if args.clear_env || outranks_system || std::env::var_os(key).is_none() {
            child_env.insert(key.clone(), value.clone());
        }
    }

    let opts = ExecOptions {
        clear_env: args.clear_env,
        preserve: args.preserve,
    };
    exec::run(&args.command, &child_env, &opts)
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

    let lockfile = args
        .lockfile
        .unwrap_or_else(|| PathBuf::from("dotenv-cloud.lock"));

    // Packages to install: missing schemes, or all (when --upgrade), de-duped.
    let target_schemes: Vec<&String> = if args.upgrade {
        schemes.iter().collect()
    } else {
        missing.iter().collect()
    };
    let mut packages: Vec<String> = Vec::new();
    for s in &target_schemes {
        let pkg = provider::suggest_package(s).to_string();
        if !packages.contains(&pkg) {
            packages.push(pkg);
        }
    }

    if packages.is_empty() {
        write_lockfile(&lockfile, &registry)?;
        println!("wrote {}", lockfile.display());
        return Ok(0);
    }

    if args.offline {
        ctx.reporter
            .warn("--offline: skipping installation of missing providers");
        return Ok(crate::error::ExitCode::Runtime.code());
    }

    if !args.yes {
        println!("would install: {}", packages.join(", "));
        ctx.reporter
            .warn("re-run `dotenv-cloud init --yes` to install the providers above");
        return Ok(crate::error::ExitCode::Runtime.code());
    }

    let url = registry_url(ctx, args.registry.as_deref());
    let installer = make_installer(ctx, &url, allow_unsigned(ctx, false))?;
    let scope = install_scope(args.project, args.user);
    for pkg in &packages {
        install_one(ctx, &installer, pkg, scope, &lockfile)?;
    }
    println!("init complete; lockfile at {}", lockfile.display());
    Ok(0)
}

/// Regenerate the lockfile from currently-installed providers, reusing the same
/// record schema as the install path so entries are consistent (`source` and
/// `sha256` included). The integrity comes from each provider's installed
/// manifest, which the install path records.
fn write_lockfile(path: &PathBuf, registry: &ProviderRegistry) -> CliResult<()> {
    let _ = std::fs::remove_file(path);
    if registry.installed().is_empty() {
        return std::fs::write(path, "version = 1\n")
            .map_err(|e| CliError::Runtime(format!("cannot write {}: {e}", path.display())));
    }
    for p in registry.installed() {
        let sha256 = p
            .manifest
            .integrity
            .as_ref()
            .and_then(|i| i.sha256.clone())
            .unwrap_or_default();
        let record = registry::InstalledRecord {
            name: p.manifest.name.clone(),
            package: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            schemes: p.manifest.schemes.clone(),
            source: "installed".to_string(),
            sha256,
        };
        registry::upsert_lockfile(path, &record).map_err(CliError::Runtime)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

pub async fn providers(ctx: &Ctx, args: ProvidersArgs) -> CliResult<i32> {
    match args.command {
        ProvidersCommand::List(a) => providers_list(ctx, a),
        ProvidersCommand::Info(a) => providers_info(ctx, a),
        ProvidersCommand::Remove(a) => providers_remove(ctx, a),
        ProvidersCommand::Search { query, registry } => {
            providers_search(ctx, &query, registry.as_deref())
        }
        ProvidersCommand::Install(a) => providers_install(ctx, a),
        ProvidersCommand::Update(a) => providers_update(ctx, a),
    }
}

fn providers_search(ctx: &Ctx, query: &str, registry: Option<&str>) -> CliResult<i32> {
    let url = registry_url(ctx, registry);
    let installer = make_installer(ctx, &url, true)?;
    let q = query.to_ascii_lowercase();
    let mut hits = 0;
    for (name, p) in &installer.index.providers {
        let hay = format!(
            "{name} {} {} {}",
            p.package,
            p.description.clone().unwrap_or_default(),
            p.schemes.join(" ")
        )
        .to_ascii_lowercase();
        if q.is_empty() || hay.contains(&q) {
            hits += 1;
            let latest = p
                .select_version(None)
                .map(|(v, _)| v)
                .unwrap_or_else(|_| "?".into());
            println!(
                "{name:<16} {} v{latest}  schemes={}",
                p.package,
                p.schemes.join(",")
            );
        }
    }
    if hits == 0 {
        println!("no providers matched `{query}`");
    }
    Ok(0)
}

fn providers_install(ctx: &Ctx, a: ProvidersTargetArgs) -> CliResult<i32> {
    let url = registry_url(ctx, a.registry.as_deref());
    let installer = make_installer(ctx, &url, allow_unsigned(ctx, a.allow_unsigned))?;
    let lock = PathBuf::from("dotenv-cloud.lock");
    install_one(
        ctx,
        &installer,
        &a.name,
        install_scope(a.project, a.user),
        &lock,
    )?;
    Ok(0)
}

fn providers_update(ctx: &Ctx, a: ProvidersOptionalTargetArgs) -> CliResult<i32> {
    let url = registry_url(ctx, a.registry.as_deref());
    let installer = make_installer(ctx, &url, allow_unsigned(ctx, a.allow_unsigned))?;
    let scope = install_scope(a.project, a.user);
    let lock = PathBuf::from("dotenv-cloud.lock");

    match a.name {
        Some(name) => install_one(ctx, &installer, &name, scope, &lock)?,
        None => {
            // Update every installed provider that the registry knows about.
            let installed = ProviderRegistry::discover(&ctx.config).map_err(CliError::Config)?;
            let mut updated = 0;
            for p in installed.installed() {
                // Match by package name against the index.
                if let Some((short, _)) = installer
                    .index
                    .providers
                    .iter()
                    .find(|(_, ip)| ip.package == p.manifest.name)
                {
                    install_one(ctx, &installer, short, scope, &lock)?;
                    updated += 1;
                }
            }
            if updated == 0 {
                println!("no installed providers found in the registry");
            }
        }
    }
    Ok(0)
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
// completions
// ---------------------------------------------------------------------------

/// Print a shell completion script to stdout. Users eval it (e.g.
/// `eval "$(dotenv-cloud completions zsh)"`), or it's installed by the package.
pub fn completions(args: CompletionsArgs) -> CliResult<i32> {
    use clap::CommandFactory;
    let mut cmd = crate::cli::Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(args.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(0)
}

// ---------------------------------------------------------------------------
// keygen / sign (maintainer + CI signing tools)
// ---------------------------------------------------------------------------

/// Generate an ed25519 release signing keypair and print it. The private key is
/// stored as a CI secret (`DOTENV_CLOUD_SIGNING_KEY`); the public key is baked
/// into the CLI's trusted-keys list.
pub fn keygen() -> CliResult<i32> {
    let (private_b64, public_b64) = registry::generate_keypair().map_err(CliError::Runtime)?;
    println!("# dotenv-cloud release signing keypair (ed25519)");
    println!("# PRIVATE KEY — store as the DOTENV_CLOUD_SIGNING_KEY CI secret; never commit it.");
    println!("private_key = {private_b64}");
    println!("# PUBLIC KEY — add to TRUSTED_PUBLIC_KEYS in src/provider/registry.rs.");
    println!("public_key = {public_b64}");
    Ok(0)
}

/// Sign a file's bytes with an ed25519 private key, emitting a base64 signature.
pub fn sign(args: SignArgs) -> CliResult<i32> {
    let key = args.key.ok_or_else(|| {
        CliError::Usage("no signing key; pass --key or set DOTENV_CLOUD_SIGNING_KEY".to_string())
    })?;
    let bytes = std::fs::read(&args.file)
        .map_err(|e| CliError::Runtime(format!("cannot read {}: {e}", args.file.display())))?;
    let sig = registry::sign_bytes(&key, &bytes).map_err(CliError::Runtime)?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, format!("{sig}\n"))
                .map_err(|e| CliError::Runtime(format!("cannot write {}: {e}", path.display())))?;
        }
        None => println!("{sig}"),
    }
    Ok(0)
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
