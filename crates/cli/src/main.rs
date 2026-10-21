use std::num::NonZeroUsize;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use odoo_changelog::{extract, walk};
use odoo_changelog_core::{Config, Version};
use odoo_changelog_extract::Registry;
use odoo_changelog_store::Store;

#[derive(Parser)]
#[command(version, about = "Symbol-level changelog of Odoo")]
struct Cli {
    #[arg(long, short, default_value = "config/odoo-changelog.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve version forks and record every new commit of every configured repo.
    Walk {
        /// Only these repos (by config name).
        #[arg(long)]
        repo: Vec<String>,
    },
    /// Diff each recorded commit against its parent and record the symbols it changed.
    Extract {
        /// Only these repos (by config name).
        #[arg(long)]
        repo: Vec<String>,
        /// First version to extract, such as 17.0.
        #[arg(long)]
        from: Option<String>,
        /// Last version to extract.
        #[arg(long)]
        to: Option<String>,
        /// Worker threads (default: one per core).
        #[arg(long)]
        threads: Option<NonZeroUsize>,
        /// Print the time spent in setup, diffing, parsing, in the cache and writing.
        #[arg(long)]
        timings: bool,
    },
    /// Commit counts per repo and version.
    Stats,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    let store = Store::open(&cfg.db)?;
    match cli.command {
        Command::Walk { repo } => walk::run(&cfg, &store, &repo),
        Command::Extract {
            repo,
            from,
            to,
            threads,
            timings,
        } => {
            let opts = extract::Options {
                repos: repo,
                from: from.as_deref().map(version).transpose()?,
                to: to.as_deref().map(version).transpose()?,
                threads,
            };
            let spent = extract::run(&cfg, &store, &Registry::builtin(), &opts)?;
            if timings {
                print_timings(&spent);
            }
            Ok(())
        }
        Command::Stats => stats(&store),
    }
}

fn version(name: &str) -> Result<Version> {
    Version::parse_branch(name).ok_or_else(|| anyhow!("{name} is not a version such as 17.0 or saas-17.2"))
}

fn print_timings(t: &extract::Timings) {
    let total = t.setup + t.diff + t.parse + t.cache + t.write;
    println!("{:<8} {:>9}", "phase", "seconds");
    for (phase, spent) in [
        ("setup", t.setup),
        ("diff", t.diff),
        ("parse", t.parse),
        ("cache", t.cache),
        ("write", t.write),
        ("total", total),
    ] {
        println!("{:<8} {:>9.2}", phase, spent.as_secs_f64());
    }
}

fn stats(store: &Store) -> Result<()> {
    println!("{:<14} {:<11} {:>8} {:>7}", "repo", "version", "commits", "merges");
    for s in store.stats()? {
        println!("{:<14} {:<11} {:>8} {:>7}", s.repo, s.version, s.commits, s.merges);
    }
    Ok(())
}
