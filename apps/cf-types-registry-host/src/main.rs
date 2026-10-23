//! `cf-types-registry-host` — the types-registry gear as a Profile 3 deployable
//! (SPEC §8.4, P30).
//!
//! A `clap` CLI over `toolkit::bootstrap::oop::run_oop_with_options`, and nothing else:
//! the listener, probes, directory self-registration, platform-plane authentication from
//! `oop_http.internal_auth` and the drain come from the bootstrap.
//!
//! The binary owns the registry's database (`gears.types-registry.database`) and links the
//! production tenant-plane authentication — authn-resolver with the OIDC plugin — so tenant reads
//! are served to a validated bearer. `AuthNResolverClient` has no remote contract, so the
//! resolver runs in this process. The `dev-authn` feature adds the static plugin for local
//! runs and tests.
//!
//! Configuration is resolved by the bootstrap: `--config` if given, else
//! `MODULE_CONFIG_PATH`. `config/types-registry-host.yaml` is a local example.

mod registered_gears;

// Help text is given as `help = "..."` strings rather than doc comments: clap re-emits doc
// comments as literals, which then have to satisfy `clippy::doc_markdown`.
#[derive(clap::Parser)]
#[command(
    name = "cf-types-registry-host",
    about = "The types-registry gear as an out-of-process service"
)]
struct Cli {
    #[arg(
        short,
        long,
        help = "Path to the configuration file. Falls back to MODULE_CONFIG_PATH"
    )]
    config: Option<std::path::PathBuf>,

    #[arg(
        short,
        long,
        action = clap::ArgAction::Count,
        help = "Log verbosity (-v debug, -vv trace)"
    )]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser as _;
    use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

    let cli = Cli::parse();

    run_oop_with_options(OopRunOptions {
        // Must equal `#[toolkit::gear(name = ...)]` of the registry gear.
        gear_name: "types-registry".to_owned(),
        verbose: cli.verbose,
        config_path: cli.config,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    })
    .await
}
