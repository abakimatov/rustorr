use rusqlite::{ErrorCode, OptionalExtension};

use crate::{Error, State, error::database};

impl State {
    /// The settings document exactly as it was saved, or `None` if none was.
    pub fn settings(&self) -> Result<Option<String>, Error> {
        self.connection()
            .query_row("SELECT document FROM settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(database("load settings"))
    }

    /// Replaces the whole document. The text is stored verbatim, so key order
    /// and spacing come back as written; it only has to be valid JSON.
    pub fn set_settings(&self, document: &str) -> Result<(), Error> {
        self.connection()
            .execute(
                "INSERT INTO settings (id, document) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET document = excluded.document",
                [document],
            )
            .map(drop)
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(failure, _)
                    if failure.code == ErrorCode::ConstraintViolation =>
                {
                    Error::InvalidValue {
                        field: "settings document",
                        reason: "is not valid JSON",
                    }
                }
                other => database("save settings")(other),
            })
    }

    /// Forgets the saved document, so the caller falls back to its defaults.
    pub fn reset_settings(&self) -> Result<(), Error> {
        self.connection()
            .execute("DELETE FROM settings", [])
            .map(drop)
            .map_err(database("reset settings"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = "{\n  \"CacheSize\": 67108864,\n  \"TMDBSettings\": {\"APIURL\": \"https://api.themoviedb.org\"},\n  \"TorznabUrls\": null,\n  \"FriendlyName\": \"Гостиная\"\n}";

    #[test]
    fn there_are_no_settings_until_some_are_saved() {
        assert_eq!(State::open_in_memory().unwrap().settings().unwrap(), None);
    }

    #[test]
    fn the_document_comes_back_byte_for_byte() {
        let state = State::open_in_memory().unwrap();

        state.set_settings(DOCUMENT).unwrap();

        assert_eq!(state.settings().unwrap().as_deref(), Some(DOCUMENT));
    }

    #[test]
    fn saving_replaces_the_previous_document() {
        let state = State::open_in_memory().unwrap();
        state.set_settings(DOCUMENT).unwrap();

        state.set_settings("{\"CacheSize\": 1}").unwrap();

        assert_eq!(
            state.settings().unwrap().as_deref(),
            Some("{\"CacheSize\": 1}")
        );
    }

    #[test]
    fn text_that_is_not_json_is_rejected_and_the_old_document_kept() {
        let state = State::open_in_memory().unwrap();
        state.set_settings(DOCUMENT).unwrap();

        for bad in ["", "{", "not json", "{\"a\": }"] {
            assert!(
                matches!(state.set_settings(bad), Err(Error::InvalidValue { .. })),
                "{bad:?}"
            );
        }

        assert_eq!(state.settings().unwrap().as_deref(), Some(DOCUMENT));
    }

    #[test]
    fn reset_forgets_the_document() {
        let state = State::open_in_memory().unwrap();
        state.set_settings(DOCUMENT).unwrap();

        state.reset_settings().unwrap();
        state.reset_settings().unwrap();

        assert_eq!(state.settings().unwrap(), None);
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustorr.db");
        State::open(&path).unwrap().set_settings(DOCUMENT).unwrap();

        let state = State::open(&path).unwrap();

        assert_eq!(state.settings().unwrap().as_deref(), Some(DOCUMENT));
    }
}
