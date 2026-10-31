//! Shared parsing and identity resolution for remote GM slash commands.
//! Chat forwards commands to `slash.SlashCmd`; there is no client-side parser mirror.
//! Identity data comes from active game owners and the startup SDE snapshot.
//! Quotes group text but shell escapes and single-quote syntax are not supported.
use super::{slash_error, CallError, Client};
use crate::{dao, messages, services::db_err};

pub(super) fn tokenize(line: &str) -> Result<Vec<String>, CallError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in line.chars() {
        match ch {
            '"' => { quoted = !quoted; started = true; }
            ch if ch.is_whitespace() && !quoted => {
                if started { words.push(std::mem::take(&mut word)); started = false; }
            }
            ch => { word.push(ch); started = true; }
        }
    }
    if quoted { return Err(slash_error(messages::gm::GM_ARGS_UNCLOSED_QUOTE.into())); }
    if started { words.push(word); }
    Ok(words)
}

pub(super) struct Args<'a> { values: &'a [String] }
impl<'a> Args<'a> {
    pub(super) fn new(values: &'a [String]) -> Self { Self { values } }
    pub(super) fn required(&self, index: usize) -> Result<&'a str, CallError> {
        self.values.get(index).map(String::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| slash_error(messages::gm::gm_args_missing(index + 1)))
    }
    pub(super) fn i64(&self, index: usize) -> Result<i64, CallError> {
        let text = self.required(index)?;
        text.parse().map_err(|_| slash_error(messages::gm::gm_args_integer(text)))
    }
    pub(super) async fn type_id(&self, c: &Client, index: usize) -> Result<(i32, String), CallError> {
        super::resolve_type(c, self.required(index)?).await
    }
    pub(super) async fn target(&self, c: &Client, index: usize) -> Result<i64, CallError> {
        target(c, self.required(index)?).await
    }
}

pub(super) async fn target(c: &Client, text: &str) -> Result<i64, CallError> {
    if text.eq_ignore_ascii_case("me") {
        return c.session.int("charid").ok_or_else(|| slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()));
    }
    dao::characters::lookup::find(&c.db, text, text.parse().ok()).await.map_err(db_err)?
        .ok_or_else(|| slash_error(messages::gm::gm_args_unknown_target(text)))
}



#[cfg(test)]
mod tests;
