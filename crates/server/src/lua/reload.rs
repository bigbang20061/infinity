//! Atomic generation replacement. The old VM remains reachable by every call
//! that obtained its registry before the swap; failed builds leave it untouched.

use std::sync::{Arc, RwLock};
use crate::config::lua::LuaConfig;
use super::{host::Resources, registry::Registry, vm};

pub struct Host {
    pub config: LuaConfig,
    pub resources: Resources,
    pub current: RwLock<Arc<Registry>>,
    reload: tokio::sync::Mutex<()>,
}

impl Host {
    pub async fn new(config: LuaConfig, resources: Resources) -> anyhow::Result<Self> {
        let registry = vm::build(&config, &resources)?;
        Ok(Self { config, resources, current: RwLock::new(Arc::new(registry)), reload: tokio::sync::Mutex::new(()) })
    }

    pub fn snapshot(&self) -> Arc<Registry> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let _guard = self.reload.lock().await;
        replace(&self.current, vm::build(&self.config, &self.resources)?);
        Ok(())
    }
}

fn replace(current: &RwLock<Arc<Registry>>, replacement: Registry) {
    *current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(replacement);
}

#[cfg(test)]
mod tests;
