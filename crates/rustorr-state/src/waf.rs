use rusqlite::{OptionalExtension, params};

use crate::{Error, State, error::database};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WafLists {
    pub whitelist: String,
    pub blacklist: String,
    pub referers: String,
}

impl State {
    pub fn waf_lists(&self) -> Result<WafLists, Error> {
        self.connection()
            .query_row(
                "SELECT whitelist, blacklist, referers FROM waf WHERE id = 1",
                [],
                |row| {
                    Ok(WafLists {
                        whitelist: row.get(0)?,
                        blacklist: row.get(1)?,
                        referers: row.get(2)?,
                    })
                },
            )
            .optional()
            .map(|value| value.unwrap_or_default())
            .map_err(database("load WAF lists"))
    }

    pub fn set_waf_lists(&self, lists: &WafLists) -> Result<(), Error> {
        self.connection()
            .execute(
                "INSERT INTO waf (id, whitelist, blacklist, referers)
                 VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET
                    whitelist = excluded.whitelist,
                    blacklist = excluded.blacklist,
                    referers = excluded.referers",
                params![lists.whitelist, lists.blacklist, lists.referers],
            )
            .map(drop)
            .map_err(database("save WAF lists"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_default_empty_and_round_trip() {
        let state = State::open_in_memory().unwrap();
        assert_eq!(state.waf_lists().unwrap(), WafLists::default());
        let lists = WafLists {
            whitelist: "127.0.0.1\n".into(),
            blacklist: "10.0.0.0/8".into(),
            referers: "example.invalid".into(),
        };
        state.set_waf_lists(&lists).unwrap();
        assert_eq!(state.waf_lists().unwrap(), lists);
    }
}
