//! Simple configuration store (replacement for Configuration.cpp/h).

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Configuration {
    map: HashMap<String, String>,
}

impl Configuration {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.map.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
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
}
