//! Helpers shared by the end-to-end tests: the `dirigent` binary run against a [`FakeHub`].
// Each test crate uses a different part of this module.
#![allow(dead_code)]

mod hub;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use tokio::process::Command;

pub use hub::*;

/// A home directory for dirigent and the hub it talks to.
pub struct Dirigent {
    pub home: tempfile::TempDir,
    pub hub: FakeHub,
}

impl Dirigent {
    pub async fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            hub: FakeHub::start().await,
        }
    }

    /// Paired already, as `dirigent pair` leaves it.
    pub async fn paired() -> Self {
        let dirigent = Self::new().await;
        dirigent.hub.write_pairing(dirigent.home.path());
        dirigent
    }

    pub fn file(&self, relative: &str) -> PathBuf {
        self.home.path().join(relative)
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dirigent"));
        command
            .args(args)
            .env("DIRIGENT_HOME", self.home.path())
            .env("DIRIGENT_HUB_URL", format!("http://{}", self.hub.address))
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    pub async fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().await.unwrap()
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}
