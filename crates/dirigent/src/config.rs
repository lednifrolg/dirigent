//! `config.toml`: optional settings.

use std::fs;
use std::io;

use serde::Deserialize;

use crate::error::AppError;
use crate::paths::Paths;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Motion in the TUI: rows that flash on a change from elsewhere, a blinking dot while a
    /// device identifies, bars that run to a new value. On by default.
    pub animations: Option<bool>,
}

impl Config {
    pub fn animations(&self) -> bool {
        self.animations.unwrap_or(true)
    }
}

pub fn load(paths: &Paths) -> Result<Config, AppError> {
    let path = paths.config_file();
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| AppError::Corrupt {
            path,
            message: e.message().to_owned(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(AppError::io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animations_are_on_unless_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        assert!(load(&paths).unwrap().animations());
        fs::create_dir_all(dir.path().join("config")).unwrap();
        fs::write(paths.config_file(), "animations = false\n").unwrap();
        assert!(!load(&paths).unwrap().animations());
        fs::write(paths.config_file(), "colour = true\n").unwrap();
        assert!(matches!(load(&paths), Err(AppError::Corrupt { .. })));
    }
}
