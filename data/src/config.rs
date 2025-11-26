//! Simple configuration store (replacement for Configuration.cpp/h).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Configuration {
    map: HashMap<String, String>,
}

impl Configuration {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.map.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }

    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    pub fn get_int(&self, key: &str, default: i64) -> i64 {
        self.get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(default)
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key)
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(default)
    }

    pub fn set_int(&mut self, key: impl Into<String>, value: i64) {
        self.set(key, value.to_string());
    }

    /// Persist configuration to a JSON file.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.map)?;
        fs::write(path, data)?;
        Ok(())
    }

    /// Load configuration from a JSON file, merging into the current map.
    pub fn load_from_file<P: AsRef<Path>>(&mut self, path: P) -> anyhow::Result<()> {
        let path_ref = path.as_ref();
        if !path_ref.exists() {
            return Ok(());
        }
        let data = fs::read_to_string(path_ref)?;
        let parsed: HashMap<String, String> = serde_json::from_str(&data)?;
        self.map.extend(parsed);
        Ok(())
    }
}
