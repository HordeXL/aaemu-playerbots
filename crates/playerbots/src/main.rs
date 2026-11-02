//! `playerbots`: headless ArcheAge 1.2 player bots for the AAEmu Rust server.
//!
//! ```text
//! playerbots example-config > playerbots.toml
//! playerbots bake --pak <client>/game_pak --out <dir> --compact <compact.sqlite3>
//! playerbots run --config playerbots.toml [--count N] [--duration-secs S]
//! playerbots grant-gm-sql --config playerbots.toml [--game-db aaemu_game] [--login-db aaemu_login]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use pb_web::BotHub;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use aa_world::{World, WorldPaths};
use playerbots::archetype::Archetypes;
use playerbots::bot::coordinator::{self, Dispatch};
use playerbots::bot::{Shared, LOGIN_PERMITS};
use playerbots::config::Config;
use playerbots::hub::{Hub, HubParams, SupCmd};
use playerbots::roster::{Roster, SharedRoster};
use playerbots::supervisor::{ensure_roster, register_slots, Supervisor};

/// Headless ArcheAge 1.2 player bots for the AAEmu Rust server.
#[derive(Parser)]
#[command(name = "playerbots", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the bots (and the web UI).
    Run {
        /// The configuration file.
        #[arg(long, default_value = "playerbots.toml")]
        config: PathBuf,
        /// Number of bots to start (overrides `bots.count`).
        #[arg(long)]
        count: Option<u32>,
        /// Stop after this many seconds (default: run until Ctrl+C).
        #[arg(long)]
        duration_secs: Option<u64>,
    },
    /// Extract terrain, zones, housing areas, quest spheres and the world map from the client's
    /// `game_pak` (needed once; the bots work without it on flat terrain).
    Bake {
        /// The client's `game_pak`.
        #[arg(long)]
        pak: PathBuf,
        /// Output directory (`data.baked` in the config).
        #[arg(long)]
        out: PathBuf,
        /// World to bake.
        #[arg(long, default_value = "main_world")]
        world: String,
        /// Metres between height samples (a multiple of 2 that divides 1024).
        #[arg(long, default_value_t = 4.0)]
        step: f32,
        /// The server's `compact.sqlite3`: validates housing areas and gives the map rectangle.
        #[arg(long)]
        compact: Option<PathBuf>,
    },
    /// Print the SQL that gives the bot accounts GM access (for `bots.gm_bootstrap`).
    GrantGmSql {
        /// The configuration file.
        #[arg(long, default_value = "playerbots.toml")]
        config: PathBuf,
        /// The game server's MySQL database (accounts, characters).
        #[arg(long, default_value = playerbots::gm::DEFAULT_GAME_DB)]
        game_db: String,
        /// The login server's MySQL database (users).
        #[arg(long, default_value = playerbots::gm::DEFAULT_LOGIN_DB)]
        login_db: String,
    },
    /// Print the commented example configuration.
    ExampleConfig,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,playerbots=info")),
        )
        .init();
    match Cli::parse().command {
        Command::ExampleConfig => {
            print!("{}", Config::example());
            Ok(())
        }
        Command::GrantGmSql {
            config,
            game_db,
            login_db,
        } => {
            let cfg = Config::load(&config)?;
            let sql = playerbots::gm::grant_sql(&cfg.server.account_prefix, &game_db, &login_db)
                .map_err(anyhow::Error::msg)?;
            print!("{sql}");
            Ok(())
        }
        Command::Bake {
            pak,
            out,
            world,
            step,
            compact,
        } => bake(pak, out, world, step, compact),
        Command::Run {
            config,
            count,
            duration_secs,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("playerbots")
                .build()
                .context("starting the async runtime")?;
            rt.block_on(run(config, count, duration_secs))
        }
    }
}

fn bake(
    pak: PathBuf,
    out: PathBuf,
    world: String,
    step: f32,
    compact: Option<PathBuf>,
) -> anyhow::Result<()> {
    let mut opts = aa_bake::BakeOptions::new(pak, out, world);
    opts.step = step;
    opts.compact = compact;
    let report = aa_bake::bake(&opts)?;
    println!("{report}");
    if report.checks.iter().any(|c| !c.ok) {
        bail!("some bake checks failed (see above)");
    }
    Ok(())
}

async fn run(
    config: PathBuf,
    count: Option<u32>,
    duration_secs: Option<u64>,
) -> anyhow::Result<()> {
    let mut cfg = Config::load(&config)?;
    if let Some(n) = count {
        cfg.bots.count = n;
        cfg.validate()?;
    }
    let cfg = Arc::new(cfg);
    let archetypes = Arc::new(Archetypes::builtin()?);

    let paths = WorldPaths {
        compact: cfg.data.compact.clone(),
        server_data: cfg.data.server_data.clone(),
        baked: cfg.data.baked.clone(),
        world: cfg.data.world.clone(),
    };
    let world = tokio::task::spawn_blocking(move || World::load(&paths))
        .await
        .context("the world loader panicked")?
        .with_context(|| {
            format!(
                "loading the world data (data.compact = {})",
                cfg.data.compact.display()
            )
        })?;
    let world = Arc::new(world);

    let roster_path = cfg.bots.roster.clone();
    let roster = Roster::load(&roster_path)?;
    let roster = Arc::new(SharedRoster::new(roster, roster_path));
    let wanted = cfg.bots.count as usize;
    let creatable = {
        let w = world.clone();
        move |race: u8, gender: u8| w.creatable(race, gender)
    };
    let added = ensure_roster(&roster, &cfg, &archetypes, &creatable, wanted)?;
    if added > 0 {
        tracing::info!(added, "new bots in the roster");
    }

    let (sup_tx, sup_rx) = tokio::sync::mpsc::channel::<SupCmd>(16);
    let llm = pb_llm::Llm::from_config(&cfg.llm);
    let hub = Arc::new(Hub::new(HubParams {
        world: &world,
        baked_dir: cfg.data.baked.as_deref(),
        supervisor: sup_tx,
        llm,
    }));

    let cancel = CancellationToken::new();
    let (coord_handle, coordinator) = coordinator::channel(hub.clone() as Arc<dyn Dispatch>);
    let coord_task = tokio::spawn(coordinator.run(cancel.clone()));

    let shared = Arc::new(Shared {
        cfg: cfg.clone(),
        world,
        hub: hub.clone(),
        roster,
        archetypes,
        coord: coord_handle,
        login_gate: tokio::sync::Semaphore::new(LOGIN_PERMITS),
        password: cfg.password(),
    });
    let registered = register_slots(&shared);
    tracing::info!(
        bots = registered,
        start = wanted.min(registered),
        login = %cfg.server.login,
        "ready"
    );

    // The web UI binds first so a busy port is an error, not a silent loss.
    let listener = tokio::net::TcpListener::bind(cfg.web.bind)
        .await
        .with_context(|| format!("binding the web UI to {}", cfg.web.bind))?;
    let web = tokio::spawn(pb_web::serve_listener(
        listener,
        hub.clone() as Arc<dyn BotHub>,
        cfg.web.clone(),
        {
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        },
    ));

    let ticker = {
        let (hub, cancel) = (hub.clone(), cancel.clone());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = every.tick() => hub.tick(),
                }
            }
        })
    };

    let supervisor = tokio::spawn(Supervisor::new(shared).run(wanted, sup_rx, cancel.clone()));

    let timer = async {
        match duration_secs {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        r = tokio::signal::ctrl_c() => {
            if let Err(e) = r {
                tracing::warn!("waiting for Ctrl+C: {e}");
            }
            tracing::info!("Ctrl+C: shutting down");
        }
        () = timer => tracing::info!("duration over: shutting down"),
    }
    cancel.cancel();

    if let Err(e) = supervisor.await {
        tracing::warn!("supervisor: {e}");
    }
    match web.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("web UI: {e:#}"),
        Err(e) => tracing::warn!("web UI task: {e}"),
    }
    let _ = coord_task.await;
    let _ = ticker.await;
    tracing::info!("bye");
    Ok(())
}
