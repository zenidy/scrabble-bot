//! Command line and environment.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::game::rules::AuctionType;

#[derive(Parser, Debug)]
#[command(
    name = "scrabbler",
    version,
    about = "The Scrabbler: a ScrabbleBot bot"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Play live: join the lobby after every match (and tournaments if asked).
    Run(RunArgs),
    /// Offline matches against baselines and sims; prints stats with 95% CIs.
    Sim(SimArgs),
    /// Rack solver benchmark.
    Bench(BenchArgs),
    /// Parameter sweep on fixed seeds.
    Tune(TuneArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// SpacetimeDB host.
    #[arg(
        long,
        env = "STDB_HOST",
        default_value = "https://maincloud.spacetimedb.com"
    )]
    pub host: String,
    /// Database name.
    #[arg(long, env = "STDB_DB", default_value = "scrabblebot")]
    pub db: String,
    /// Bot token from the Team page (first run; saved to --token-file).
    #[arg(long, env = "BOT_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    /// Credential nonce from `mint_credential_nonce` (first run).
    #[arg(long, env = "BOT_NONCE", hide_env_values = true)]
    pub nonce: Option<String>,
    /// Where the token is kept (gitignored).
    #[arg(long, env = "TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    /// Opponent models and per-match JSONL logs.
    #[arg(long, env = "DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,
    /// Register for this tournament on connect.
    #[arg(long)]
    pub tournament: Option<u64>,
    /// Register for any tournament taking registrations.
    #[arg(long)]
    pub auto_register: bool,
    /// Don't join the lobby (tournament only, or testing).
    #[arg(long)]
    pub no_lobby: bool,
    /// Expected bot name; warns on mismatch.
    #[arg(long, env = "BOT_NAME", default_value = "The Scrabbler")]
    pub name: String,
    /// Stop after this many finished matches.
    #[arg(long)]
    pub max_matches: Option<u64>,
    /// Don't send a bid expected to land within this many ms of the close.
    #[arg(long, default_value_t = 120)]
    pub safety_ms: u64,
}

impl RunArgs {
    pub fn token_path(&self) -> PathBuf {
        self.token_file
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("secrets/{}.token", self.db)))
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Scenario {
    /// The Scrabbler + 5 random sims, paired against hoard-flush on the same seeds.
    Lobby,
    /// 6 bots: 2 Scrabblers, 2 hoard-flush, 2 starters.
    League,
    /// Every version and baseline in one 6-bot match.
    Versions,
    /// 4-bot Swiss table: Scrabbler, previous version, hoard-flush, starter.
    Table4,
    /// 8-bot elimination match.
    Elim8,
    /// 2-bot final vs hoard-flush and vs the previous version.
    Final2,
    /// All-strong lobby: 3 Scrabblers with the lambda cap vs 3 without.
    Strong6,
    /// 6 market bots (the live lobby's price level), to calibrate them.
    Market6,
    /// The Scrabbler + 5 market bots, paired against hoard-flush.
    Live,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AuctionArg {
    Vickrey,
    First,
}

impl From<AuctionArg> for AuctionType {
    fn from(a: AuctionArg) -> Self {
        match a {
            AuctionArg::Vickrey => AuctionType::Vickrey,
            AuctionArg::First => AuctionType::FirstPrice,
        }
    }
}

#[derive(Args, Debug)]
pub struct SimArgs {
    #[arg(long, value_enum, default_value = "lobby")]
    pub scenario: Scenario,
    #[arg(long, default_value_t = 500)]
    pub matches: usize,
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    #[arg(long, value_enum, default_value = "vickrey")]
    pub auction: AuctionArg,
    /// Starting rack size.
    #[arg(long, default_value_t = 5)]
    pub rack: u32,
    /// Chance an agent bid misses the window (latency model).
    #[arg(long, default_value_t = 0.0)]
    pub late: f64,
    /// JSON file of Scrabbler params to use instead of the defaults.
    #[arg(long)]
    pub params: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct BenchArgs {
    #[arg(long, default_value_t = 200)]
    pub racks: usize,
}

#[derive(Args, Debug)]
pub struct TuneArgs {
    /// Scenario to tune on.
    #[arg(long, value_enum, default_value = "league")]
    pub scenario: Scenario,
    #[arg(long, default_value_t = 300)]
    pub matches: usize,
    #[arg(long, default_value_t = 1000)]
    pub seed: u64,
    /// `name=v1,v2,...` (e.g. play_long_lambda=0.2,0.3,0.5)
    #[arg(long)]
    pub sweep: String,
    #[arg(long, value_enum, default_value = "vickrey")]
    pub auction: AuctionArg,
}
