//! The interactive TUI: `dirigent` without a command.
//!
//! [`model`] holds the state and decides what to do, [`view`] draws it. This module does the
//! I/O: terminal input, the clock, and the background work the model asks for: connecting,
//! following the hub's event stream, and pairing.

mod keymap;
mod model;
mod view;

use std::io::{self, IsTerminal};
use std::time::Duration;

use dirigera::{Client, EventStream};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Position;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::connect::Hub;
use crate::error::AppError;
use crate::output;
use crate::pairing;
use crate::paths::Paths;
use model::{Connected, Effect, Failure, FailureKind, Model, Msg, Pairing, Start};
use view::{Hotspot, Target};

/// How often the clock and the countdowns are updated.
const TICK: Duration = Duration::from_secs(1);
/// Redraw interval while a spinner runs.
const FRAME: Duration = Duration::from_millis(80);

/// What reaches the event loop: messages for the model, and mouse events still to be placed.
enum Incoming {
    Msg(Box<Msg>),
    Mouse(MouseEvent),
}

pub async fn run(paths: Paths) -> Result<(), AppError> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(AppError::Usage(
            "`dirigent` without a command opens the TUI, which needs a terminal; see `dirigent --help`".into(),
        ));
    }
    let (mut model, effects) = Model::start(Start {
        now: Timestamp::now(),
        tz: TimeZone::system(),
        // https://no-color.org: set and not empty.
        colors: std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
    });
    let mut terminal = ratatui::init();
    let result = match execute!(io::stdout(), EnableMouseCapture) {
        Ok(()) => {
            // Leave the terminal's mouse alone even after a panic.
            let restore = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = execute!(io::stdout(), DisableMouseCapture);
                restore(info);
            }));
            let result = event_loop(&paths, &mut terminal, &mut model, effects).await;
            let _ = execute!(io::stdout(), DisableMouseCapture);
            result
        }
        Err(e) => Err(AppError::io("terminal", e)),
    };
    ratatui::restore();
    result
}

/// Background work that a newer request of the same kind replaces.
#[derive(Default)]
struct Tasks {
    listening: Option<JoinHandle<()>>,
    pairing: Option<JoinHandle<()>>,
}

impl Tasks {
    fn replace(slot: &mut Option<JoinHandle<()>>, task: JoinHandle<()>) {
        if let Some(old) = slot.replace(task) {
            old.abort();
        }
    }
}

async fn event_loop(
    paths: &Paths,
    terminal: &mut DefaultTerminal,
    model: &mut Model,
    effects: Vec<Effect>,
) -> Result<(), AppError> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    read_input(tx.clone());
    let mut tasks = Tasks::default();
    for effect in effects {
        spawn(paths, &tx, &mut tasks, effect);
    }
    let mut hotspots: Vec<Hotspot> = Vec::new();
    while !model.quit {
        terminal
            .draw(|frame| hotspots = view::render(model, frame))
            .map_err(|e| AppError::io("terminal", e))?;
        let wait = if model.spinning() { FRAME } else { TICK };
        let first = tokio::select! {
            Some(incoming) = rx.recv() => incoming,
            () = tokio::time::sleep(wait) => Incoming::Msg(Box::new(Msg::Tick(Timestamp::now()))),
        };
        let mut incoming = vec![first];
        while let Ok(more) = rx.try_recv() {
            incoming.push(more);
        }
        let mut messages: Vec<Msg> = incoming
            .into_iter()
            .filter_map(|incoming| match incoming {
                Incoming::Msg(msg) => Some(*msg),
                Incoming::Mouse(event) => place(event, &hotspots),
            })
            .collect();
        messages.push(Msg::Tick(Timestamp::now()));
        for msg in messages {
            for effect in model.update(msg) {
                spawn(paths, &tx, &mut tasks, effect);
            }
        }
    }
    for task in [tasks.listening, tasks.pairing].into_iter().flatten() {
        task.abort();
    }
    Ok(())
}

/// The message a click stands for, by what is under the pointer.
fn place(event: MouseEvent, hotspots: &[Hotspot]) -> Option<Msg> {
    if event.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let pointer = Position::new(event.column, event.row);
    let spot = hotspots
        .iter()
        .rev()
        .find(|spot| spot.area.contains(pointer))?;
    Some(match &spot.target {
        Target::Row(row) => Msg::Select(row.clone()),
        Target::Tab(tab) => Msg::Tab(*tab),
    })
}

/// Reads terminal events on their own thread, since reading blocks.
fn read_input(tx: UnboundedSender<Incoming>) {
    std::thread::spawn(move || {
        loop {
            let incoming = match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    Incoming::Msg(Box::new(Msg::Key(key)))
                }
                // A tick redraws at the new size.
                Ok(Event::Resize(..)) => Incoming::Msg(Box::new(Msg::Tick(Timestamp::now()))),
                Ok(Event::Mouse(mouse))
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
                {
                    Incoming::Mouse(mouse)
                }
                Ok(_) => continue,
                Err(_) => return,
            };
            if tx.send(incoming).is_err() {
                return;
            }
        }
    });
}

fn spawn(paths: &Paths, tx: &UnboundedSender<Incoming>, tasks: &mut Tasks, effect: Effect) {
    let paths = paths.clone();
    let tx = tx.clone();
    // The receiver is gone only when the TUI has quit.
    let report = move |msg| {
        let _ = tx.send(Incoming::Msg(Box::new(msg)));
    };
    match effect {
        Effect::Connect => {
            tokio::spawn(async move {
                report(Msg::Connected(
                    Hub::connect(&paths)
                        .await
                        .map(connected)
                        .map_err(|e| failure(&e)),
                ));
            });
        }
        Effect::Listen {
            client,
            after,
            resync,
        } => {
            let task = tokio::spawn(listen(client, after, resync, report));
            Tasks::replace(&mut tasks.listening, task);
        }
        Effect::Resync { client } => {
            tokio::spawn(async move {
                report(Msg::Resynced(
                    client.home().await.map_err(|e| failure(&e.into())),
                ));
            });
        }
        Effect::Pair => {
            let task = tokio::spawn(async move {
                let result = pair(&paths, &report).await;
                report(Msg::Pairing(match result {
                    Ok(hub) => Pairing::Done(Box::new(connected(hub))),
                    Err(error) => Pairing::Failed(output::message(&error)),
                }));
            });
            Tasks::replace(&mut tasks.pairing, task);
        }
        Effect::CancelPairing => {
            if let Some(task) = tasks.pairing.take() {
                task.abort();
            }
        }
    }
}

fn connected(hub: Hub) -> Connected {
    Connected {
        client: hub.client,
        home: hub.home,
        name: hub.paired.name,
    }
}

/// Opens the event stream and forwards its events until it ends.
async fn listen(client: Client, after: Duration, resync: bool, report: impl Fn(Msg)) {
    tokio::time::sleep(after).await;
    if resync {
        match client.home().await {
            Ok(home) => report(Msg::Resynced(Ok(home))),
            Err(dirigera::Error::Unauthorized) => {
                return report(Msg::Resynced(Err(failure(&AppError::TokenRevoked))));
            }
            Err(error) => return report(Msg::Lost(error.to_string())),
        }
    }
    let mut stream = match EventStream::connect(&client).await {
        Ok(stream) => stream,
        Err(
            error @ (dirigera::Error::Unauthorized | dirigera::Error::CertificateChanged { .. }),
        ) => {
            return report(Msg::Resynced(Err(failure(&error.into()))));
        }
        Err(error) => return report(Msg::Lost(error.to_string())),
    };
    report(Msg::Live);
    loop {
        match stream.next().await {
            Ok(Some(event)) => report(Msg::Event(Box::new(event))),
            Ok(None) => return report(Msg::Lost("the hub closed the event stream".into())),
            Err(error) => return report(Msg::Lost(error.to_string())),
        }
    }
}

/// Pairs the way `dirigent pair` does, telling the dialog when to press the button.
async fn pair(paths: &Paths, report: &impl Fn(Msg)) -> Result<Hub, AppError> {
    let (address, found) = pairing::find_hub(None).await?;
    let started = pairing::start(address).await?;
    let hub = found
        .as_ref()
        .map_or_else(|| address.ip().to_string(), |hub| hub.hostname.clone());
    report(Msg::Pairing(Pairing::Pressing { hub, address }));
    let token = pairing::wait_for_press(&started, &pairing::client_name(), |_| {}).await?;
    pairing::remember(paths, address, found, &started, token).await
}

fn failure(error: &AppError) -> Failure {
    let (kind, message) = match error {
        AppError::NotPaired => (FailureKind::NotPaired, output::message(error)),
        AppError::TokenRevoked => (
            FailureKind::PairAgain,
            "the hub no longer accepts dirigent; pair again".to_owned(),
        ),
        AppError::CertificateChanged => (
            FailureKind::PairAgain,
            "the hub presented another certificate than at pairing; pair again if it was reset"
                .to_owned(),
        ),
        _ => (FailureKind::Other, output::message(error)),
    };
    Failure { kind, message }
}
