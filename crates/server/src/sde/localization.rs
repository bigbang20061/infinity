//! Immutable English and Chinese text used by dungeon and store data.
//! This server-startup dataset has no direct client mirror.
//! Sources: native localization, dungeons, types and market groups. Other languages and unreferenced text are unsupported.

use crate::{dao, db::Db};

pub(crate) struct LocalizedText {
    pub id: i64,
    pub text: Option<String>,
}

pub(crate) struct Localization {
    english: Vec<LocalizedText>,
    chinese: Vec<LocalizedText>,
}

impl Localization {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (english, chinese) = tokio::try_join!(dao::sde::localization::english(db), dao::sde::localization::chinese(db))?;
        Ok(Self { english, chinese })
    }

    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub(crate) fn english(&self, id: i64) -> Option<&str> {
        self.english.binary_search_by_key(&id, |row| row.id).ok().and_then(|index| self.english[index].text.as_deref())
    }

    pub(crate) fn chinese(&self, id: i64) -> Option<&str> {
        self.chinese.binary_search_by_key(&id, |row| row.id).ok().and_then(|index| self.chinese[index].text.as_deref())
    }

    pub(crate) fn len(&self) -> usize { self.english.len() + self.chinese.len() }
}
