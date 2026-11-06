//! `dirigent`: terminal client for the IKEA DIRIGERA smart home hub over its local API.

mod commands;
mod config;
mod connect;
mod control;
mod error;
mod files;
mod names;
mod output;
mod pairing;
mod paths;
mod store;
mod theme;
mod tui;
mod wanted;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;

use crate::connect::Hub;
use crate::control::{Options, Wanted};
use crate::error::AppError;
use crate::names::{Kind, Target};
use crate::paths::Paths;
use crate::wanted::{Color, Level, White};

#[derive(Debug, Parser)]
#[command(
    name = "dirigent",
    version,
    propagate_version = true,
    about,
    after_help = "Without a command, dirigent opens the interactive TUI."
)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// A command with the global options.
struct Invocation {
    json: bool,
    command: Command,
}

/// Names of devices or rooms, and what they stand for.
#[derive(Debug, Args)]
struct Names {
    /// Device or room names; part of a name is enough.
    #[arg(required = true, value_name = "NAME")]
    names: Vec<String>,
    #[command(flatten)]
    only: Only,
}

/// One device or room.
#[derive(Debug, Args)]
struct Name {
    /// Device or room name; part of it is enough.
    name: String,
    #[command(flatten)]
    only: Only,
}

#[derive(Debug, Clone, Copy, Args)]
struct Only {
    /// The names are rooms.
    #[arg(long, conflicts_with = "device")]
    room: bool,
    /// The names are devices.
    #[arg(long)]
    device: bool,
}

impl Only {
    fn kinds(self) -> &'static [Kind] {
        match (self.room, self.device) {
            (true, _) => &[Kind::Room],
            (_, true) => &[Kind::Device],
            _ => &[Kind::Device, Kind::Room],
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Pair with the hub: press its action button when asked.
    Pair {
        /// The hub's IP address; found on the network when left out.
        address: Option<String>,
        /// How the hub lists dirigent among its users.
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove dirigent from the hub's users and forget the pairing.
    Unpair,
    /// Look for hubs on the network.
    Discover,
    /// List the rooms with their devices, the scenes and the remotes.
    #[command(visible_alias = "ls")]
    List,
    /// Show a device, room or scene.
    Status {
        /// Device, room or scene name; part of it is enough.
        name: String,
    },
    /// Switch devices or whole rooms on.
    On {
        #[command(flatten)]
        names: Names,
        #[command(flatten)]
        options: Options,
    },
    /// Switch devices or whole rooms off.
    Off {
        #[command(flatten)]
        names: Names,
        #[command(flatten)]
        options: Options,
    },
    /// Switch on what is off and off what is on; a room switches as a whole.
    Toggle {
        #[command(flatten)]
        names: Names,
        #[command(flatten)]
        options: Options,
    },
    /// Set the brightness in percent: 40, or relative to now: +10, -10.
    Dim {
        #[command(flatten)]
        name: Name,
        #[arg(allow_negative_numbers = true, value_name = "LEVEL")]
        level: Level,
        #[command(flatten)]
        options: Options,
    },
    /// Set white in kelvin, or warm, neutral or cool for the light's range.
    White {
        #[command(flatten)]
        name: Name,
        #[arg(value_name = "KELVIN|warm|neutral|cool")]
        white: White,
        #[command(flatten)]
        options: Options,
    },
    /// Set a color by name, hue or #rrggbb.
    ///
    /// Names: red, orange, yellow, green, cyan, blue, purple, pink. A hue in degrees takes an
    /// optional saturation from 0 to 1: 30 or 30,0.8. Brightness stays as it is.
    Color {
        #[command(flatten)]
        name: Name,
        #[arg(value_name = "COLOR")]
        color: Color,
        #[command(flatten)]
        options: Options,
    },
    /// Run a scene, or take back its latest run.
    Scene {
        /// Scene name; part of it is enough.
        name: String,
        /// Take back the latest run, while the scene allows it.
        #[arg(long)]
        undo: bool,
    },
    /// Make a device blink so you can tell which one it is.
    Identify {
        /// Device name; part of it is enough.
        name: String,
        #[arg(long, default_value_t = 5, value_name = "SECONDS")]
        seconds: u64,
    },
    /// Print what changes in the home as it happens, until stopped.
    Watch,
    /// Print a shell completion script.
    ///
    /// For example: dirigent completions bash > ~/.local/share/bash-completion/completions/dirigent
    Completions { shell: Shell },
    /// Write the man pages for dirigent and each of its commands into a directory.
    ///
    /// For example: dirigent man ~/.local/share/man/man1
    Man {
        /// Directory for the pages; created when missing.
        dir: PathBuf,
    },
}

/// Writes a man page for `command` and for each of its commands into `dir`, every one with
/// dirigent's name and version in its footer.
fn man_pages(command: clap::Command, dir: &Path) -> std::io::Result<()> {
    fn write(command: clap::Command, dir: &Path, source: &str) -> std::io::Result<()> {
        for sub in command.get_subcommands().filter(|s| !s.is_hide_set()) {
            write(sub.clone(), dir, source)?;
        }
        clap_mangen::Man::new(command)
            .source(source.to_owned())
            .generate_to(dir)
            .map(drop)
    }
    let mut command = command.disable_help_subcommand(true);
    command.build();
    write(
        command,
        dir,
        &format!("dirigent {}", env!("CARGO_PKG_VERSION")),
    )
}

/// The names and what to do with them, for commands that change devices.
fn wanted(command: &Command) -> Option<(Vec<String>, Only, Wanted, Options)> {
    Some(match command {
        Command::On { names, options } => (names.names.clone(), names.only, Wanted::On, *options),
        Command::Off { names, options } => (names.names.clone(), names.only, Wanted::Off, *options),
        Command::Toggle { names, options } => {
            (names.names.clone(), names.only, Wanted::Toggle, *options)
        }
        Command::Dim {
            name,
            level,
            options,
        } => (
            vec![name.name.clone()],
            name.only,
            Wanted::Brightness(*level),
            *options,
        ),
        Command::White {
            name,
            white,
            options,
        } => (
            vec![name.name.clone()],
            name.only,
            Wanted::White(*white),
            *options,
        ),
        Command::Color {
            name,
            color,
            options,
        } => (
            vec![name.name.clone()],
            name.only,
            Wanted::Color(*color),
            *options,
        ),
        _ => return None,
    })
}

async fn run(cli: Cli) -> Result<(), AppError> {
    // These need no hub and no files, so they work before anything is set up.
    match &cli.command {
        Some(Command::Completions { shell }) => {
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "dirigent", &mut script);
            return output::raw(&script);
        }
        Some(Command::Man { dir }) => {
            if dir.exists() && !dir.is_dir() {
                return Err(AppError::Usage(format!(
                    "{} is not a directory",
                    dir.display()
                )));
            }
            std::fs::create_dir_all(dir)
                .and_then(|()| man_pages(Cli::command(), dir))
                .map_err(|e| AppError::io(dir, e))?;
            output::success(format_args!(
                "man pages written to {}; read one with `man -l {}`",
                dir.display(),
                dir.join("dirigent.1").display()
            ));
            return Ok(());
        }
        _ => {}
    }
    let paths = Paths::discover()?;
    let Some(command) = cli.command else {
        return tui::run(paths).await;
    };
    let cli = Invocation {
        json: cli.json,
        command,
    };
    match &cli.command {
        Command::Pair { address, name } => {
            return commands::pair(&paths, address.as_deref(), name.clone()).await;
        }
        Command::Unpair => return commands::unpair(&paths).await,
        Command::Discover => return commands::discover(cli.json).await,
        Command::Watch => return commands::watch(&paths, cli.json).await,
        _ => {}
    }
    let hub = Hub::connect(&paths).await?;
    if let Some((names, only, wanted, options)) = wanted(&cli.command) {
        let targets = commands::targets(&hub.home, &names, only.kinds())?;
        return control::run(&hub, &targets, wanted, options, cli.json).await;
    }
    let home = &hub.home;
    match &cli.command {
        Command::List => commands::list(&hub, cli.json),
        Command::Status { name } => {
            let target = names::resolve(home, name, &[Kind::Device, Kind::Room, Kind::Scene])?;
            commands::status(&hub, target, cli.json)
        }
        Command::Scene { name, undo } => {
            let Target::Scene(scene) = names::resolve(home, name, &[Kind::Scene])? else {
                unreachable!("only scenes were asked for");
            };
            commands::scene(&hub, scene, *undo).await
        }
        Command::Identify { name, seconds } => {
            let Target::Device(device) = names::resolve(home, name, &[Kind::Device])? else {
                unreachable!("only devices were asked for");
            };
            commands::identify(&hub, device, *seconds).await
        }
        _ => unreachable!("every other command is handled above"),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a single-threaded runtime always starts");
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !matches!(error, AppError::Shown(_)) {
                output::error(&error);
            }
            error.exit_code()
        }
    }
}
