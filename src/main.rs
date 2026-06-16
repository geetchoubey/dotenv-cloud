//! dotenv-cloud: load dotenv files, resolve remote secret references through
//! external provider plugins, and inject the merged environment into a child
//! process. See TECHNICAL_SPEC.md.

mod cli;
mod commands;
mod config;
mod dotenv;
mod error;
mod exec;
mod export;
mod merge;
mod pipeline;
mod provider;
mod redact;
mod report;
mod secret;
mod uri;

use clap::Parser;

use cli::{Cli, Command};
use commands::Ctx;
use error::CliError;

fn main() {
    let exit = real_main();
    std::process::exit(exit);
}

fn real_main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap prints its own message; map to usage exit code for errors.
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
                _ => error::ExitCode::Usage.code(),
            };
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start async runtime: {e}");
            return error::ExitCode::Runtime.code();
        }
    };

    let result = runtime.block_on(dispatch(cli));

    match result {
        Ok(code) => code,
        Err(err) => {
            // Build a reporter just for coloring; errors always print.
            let reporter = report::Reporter::default();
            reporter.error(&err.to_string());
            err.exit_code().code()
        }
    }
}

async fn dispatch(cli: Cli) -> Result<i32, CliError> {
    // These need no config or environment context.
    match cli.command {
        Command::Completions(args) => return commands::completions(args),
        Command::Keygen => return commands::keygen(),
        Command::Sign(args) => return commands::sign(args),
        _ => {}
    }

    let ctx = Ctx::from_global(&cli.global)?;

    match cli.command {
        Command::Run(args) => commands::run(&ctx, args).await,
        Command::Export(args) => commands::export(&ctx, args).await,
        Command::Build(args) => commands::build(&ctx, args).await,
        Command::Resolve(args) => commands::resolve_key(&ctx, args).await,
        Command::Validate(args) => commands::validate(&ctx, args).await,
        Command::Doctor => commands::doctor(&ctx).await,
        Command::Init(args) => commands::init(&ctx, args).await,
        Command::Providers(args) => commands::providers(&ctx, args).await,
        Command::Completions(_) | Command::Keygen | Command::Sign(_) => {
            unreachable!("handled before ctx")
        }
    }
}
