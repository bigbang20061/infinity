//! Shared parsing for remote GM slash commands.
//! Chat forwards commands to `slash.SlashCmd`; there is no client-side parser mirror.
//! Quotes group text but shell escapes and single-quote syntax are not supported.
use super::{slash_error, CallError};
use crate::messages;

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

#[cfg(test)]
mod tests;
