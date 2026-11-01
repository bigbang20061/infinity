//! Runtime MUC occupant lifecycle for `xmppchatsvc.py` and room settings clients.
//! Authority and durable ACLs live in chat services/DAO; cached occupant roles are
//! refreshed after commits. Password rooms, corporate ACLs and proxy ownership
//! are not supported by the domain.
use super::*;
use crate::services::{chat::authority, CallError};

fn unavailable(room: &str, nick: &str, destroy: bool) -> String {
    format!("<presence from='{}' type='unavailable'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='none' role='none'/>{}</x></presence>", esc(&format!("{room}/{nick}")), if destroy { "<destroy/>" } else { "" })
}

impl Hub {
    fn remove_matching(&self, predicate: impl Fn(&str, &str, &Occupant) -> bool, destroy: bool) {
        let mut rooms = self.rooms.lock().unwrap();
        rooms.epoch = rooms.epoch.wrapping_add(1);
        for (room, occupants) in &mut rooms.entries {
            let gone: Vec<_> = occupants.iter().filter(|(nick, o)| predicate(room, nick, o)).map(|(nick, _)| nick.clone()).collect();
            for nick in gone {
                if let Some(removed) = occupants.remove(&nick) {
                    let stanza = unavailable(room, &nick, destroy);
                    let _ = removed.tx.send(stanza.clone());
                    for occupant in occupants.values() { let _ = occupant.tx.send(stanza.clone()); }
                }
            }
        }
        rooms.entries.retain(|_, occupants| !occupants.is_empty());
    }

    pub(super) fn leave_all(&self, full_jid: &str, tx: &Tx) {
        self.remove_matching(|_, _, o| o.full_jid == full_jid && o.tx.same_channel(tx), false);
    }

    pub(crate) fn disconnect_character(&self, id: i64) {
        self.tokens.lock().unwrap().remove(&id.to_string());
        self.remove_matching(|_, nick, _| nick == id.to_string(), false);
    }

    pub(crate) fn session_changed(&self, id: i64) {
        let allowed = self.world.chat_session(id).map(|s| authority::system_channels(self.world.sde(), &s)).unwrap_or_default();
        self.remove_matching(|room, nick, _| {
            let channel = room.split('@').next().unwrap_or("");
            nick == id.to_string() && ["local_", "wormhole_", "nolocal_", "nullsec_", "triglavian_", "corp_", "alliance_", "faction_", "fleet_"].iter().any(|prefix| channel.starts_with(prefix)) && !allowed.iter().any(|name| name == channel)
        }, false);
    }

    pub(crate) fn evict_room(&self, channel: &str) {
        let room = format!("{channel}@{}", self.conference());
        self.remove_matching(|r, _, _| r == room, true);
    }

    pub(crate) fn evict_member(&self, channel: &str, id: i64) {
        let room = format!("{channel}@{}", self.conference());
        self.remove_matching(|r, nick, _| r == room && nick == id.to_string(), false);
    }

    pub(crate) fn broadcast_subject(&self, channel: &str, subject: &str) {
        let room = format!("{channel}@{}", self.conference());
        let rooms = self.rooms.lock().unwrap();
        if let Some(occupants) = rooms.entries.get(&room) {
            for o in occupants.values() { let _ = o.tx.send(format!("<message from='{}' to='{}' type='groupchat'><subject>{}</subject></message>", esc(&room), esc(&o.full_jid), esc(subject))); }
        }
    }

    pub(crate) async fn refresh_permissions(&self, channel: &str) -> Result<(), CallError> {
        let room = format!("{channel}@{}", self.conference());
        let occupants = {
            let mut rooms = self.rooms.lock().unwrap();
            rooms.epoch = rooms.epoch.wrapping_add(1);
            rooms.entries.get(&room).cloned().unwrap_or_default()
        };
        for (nick, old) in occupants {
            let id = match nick.parse::<i64>() { Ok(id) => id, Err(_) => continue };
          loop {
            let epoch = self.rooms.lock().unwrap().epoch;
            let session = self.world.chat_session(id);
            let admission = match &session {
                Some(s) => match authority::admit(&self.db, self.world.sde(), s, channel).await {
                    Ok(admission) => admission,
                    Err(error) => { self.remove_matching(|r, _, _| r == room, false); return Err(error); }
                },
                None => None,
            };
            let mut rooms = self.rooms.lock().unwrap();
            if rooms.epoch != epoch || self.world.chat_session(id) != session { continue; }
            let Some(occupants) = rooms.entries.get_mut(&room) else { break };
            if !occupants.get(&nick).is_some_and(|o| o.full_jid == old.full_jid && o.tx.same_channel(&old.tx)) { break; }
            if let Some(admission) = admission {
                let o = occupants.get_mut(&nick).unwrap();
                if o.role != admission.role || o.affiliation != admission.affiliation {
                    o.role = admission.role; o.affiliation = admission.affiliation;
                    let p = format!("<presence from='{}'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='{}' role='{}' jid='{}'/></x></presence>", esc(&format!("{room}/{nick}")), esc(&o.affiliation), esc(&o.role), esc(&o.full_jid));
                    for target in occupants.values() { let _ = target.tx.send(p.clone()); }
                }
            } else {
                let removed = occupants.remove(&nick).unwrap();
                let p = unavailable(&room, &nick, false);
                let _ = removed.tx.send(p.clone());
                for target in occupants.values() { let _ = target.tx.send(p.clone()); }
            }
            break;
          }
        }
        Ok(())
    }
}
