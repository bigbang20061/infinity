//! Server-side mirror of the client's distributed session attributes.

use marshal::Value;
use std::collections::BTreeMap;

pub struct Session {
    pub sid: i64,
    attrs: BTreeMap<String, Value>,
}

impl Session {
    pub fn new(sid: i64) -> Self {
        Session { sid, attrs: BTreeMap::new() }
    }

    pub fn get(&self, key: &str) -> &Value {
        self.attrs.get(key).unwrap_or(&Value::None)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).as_int()
    }

    /// Inspect session attributes without modifying the distributed session.
    pub(crate) fn attributes(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.attrs.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// Apply updates; returns the `{attr: (old, new)}` pairs that actually changed.
    pub fn apply(&mut self, updates: Vec<(&str, Value)>) -> Vec<(Value, Value)> {
        let mut change = Vec::new();
        for (k, new) in updates {
            let old = self.get(k).clone();
            if old != new {
                change.push((Value::str(k), Value::tuple([old, new.clone()])));
                self.attrs.insert(k.to_string(), new);
            }
        }
        change
    }
}

#[cfg(test)]
mod tests;
