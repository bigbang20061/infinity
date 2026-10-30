//! Ordered post-commit service effects and client notifications.
//! Notifications correspond to client scatter events; no database tables are accessed here.
//! Application-specific effects must be dispatched by their owning service.

use std::convert::Infallible;
use marshal::Value;
use crate::{client::Client, services::CallError, space::{NotifyTx, World}};

pub struct Outbox<E = Infallible> {
    entries: Vec<Effect<E>>,
}

impl<E> Default for Outbox<E> {
    fn default() -> Self { Self { entries: Vec::new() } }
}

pub enum Effect<E> {
    Notification(Notification),
    Operation(E),
}

pub enum Notification {
    DirectClient { event: String, args: Vec<Value> },
    Pilot { char_id: i64, event: String, args: Vec<Value> },
    Sender { sender: NotifyTx, event: String, args: Vec<Value> },
}

impl<E> Outbox<E> {
    pub fn notify_client(&mut self, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::DirectClient { event: event.into(), args }));
    }
    pub fn notify_pilot(&mut self, char_id: i64, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::Pilot { char_id, event: event.into(), args }));
    }
    pub fn notify_sender(&mut self, sender: NotifyTx, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::Sender { sender, event: event.into(), args }));
    }
    pub fn operation(&mut self, operation: E) { self.entries.push(Effect::Operation(operation)); }
    pub fn into_entries(self) -> Vec<Effect<E>> { self.entries }
}

impl Notification {
    pub fn dispatch(self, world: &World, client: Option<&mut Client>) -> Result<(), CallError> {
        match self {
            Self::DirectClient { event, args } => {
                let client = client.ok_or_else(|| CallError::Internal(anyhow::anyhow!("direct notification requires a client")))?;
                client.notify(&event, args);
            }
            Self::Pilot { char_id, event, args } => world.notify_pilot(char_id, &event, args),
            Self::Sender { sender, event, args } => { let _ = sender.send((event, args)); }
        }
        Ok(())
    }
}

pub fn dispatch_world<T>(value: T, outbox: Outbox, world: &World) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => notification.dispatch(world, None)?,
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}

pub fn dispatch_client<T>(value: T, outbox: Outbox, client: &mut Client) -> Result<T, CallError> {
    let world = client.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *client))?,
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}

#[cfg(test)] mod tests;
