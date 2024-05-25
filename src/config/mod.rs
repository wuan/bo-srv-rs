use std::fs;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Blitzortung {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Database {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub blitzortung: Blitzortung,
    pub database: Database,
}

pub fn read_config() -> Config {
    let config_string = fs::read_to_string("config.yml").expect("failed to read config file");
    serde_yaml::from_str(&config_string).expect("failed to parse config file")
}