use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use tokio::{sync::Mutex, task::JoinHandle};

use crate::{Indexer, RutorDatabase, TorrentDetails, torznab};

pub type SearchFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the reference downloads its Rutor database from.
pub const RUTOR_URL: &str = "http://releases.yourok.ru/torr/rutor.ls";
const RUTOR_UPDATE_INTERVAL: Duration = Duration::from_secs(3 * 60 * 60);
/// Bounds each search request as a whole. The reference uses Go's default
/// client, which never times out.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The search integrations as the HTTP layer sees them.
pub trait Search: Send + Sync {
    /// Follows `EnableRutorSearch`: loading or downloading the database and
    /// refreshing it every three hours, or forgetting it.
    fn set_rutor_enabled(&self, enabled: bool) -> SearchFuture<'_, ()>;
    fn rutor(&self, query: &str) -> Vec<TorrentDetails>;
    /// One indexer when `index` names it, otherwise every configured one.
    fn torznab<'a>(
        &'a self,
        indexers: &'a [Indexer],
        query: &'a str,
        index: i64,
    ) -> SearchFuture<'a, Vec<TorrentDetails>>;
    fn torznab_test<'a>(
        &'a self,
        host: &'a str,
        key: &'a str,
    ) -> SearchFuture<'a, Result<(), String>>;
}

pub struct SearchService {
    rutor: Arc<RutorDatabase>,
    client: reqwest::Client,
    updater: Mutex<Option<JoinHandle<()>>>,
}

impl SearchService {
    /// `client` is the server's shared outbound client; every search request
    /// adds its own overall timeout.
    pub fn new(rutor: RutorDatabase, client: reqwest::Client) -> Self {
        Self {
            rutor: Arc::new(rutor),
            client,
            updater: Mutex::new(None),
        }
    }
}

impl Search for SearchService {
    fn set_rutor_enabled(&self, enabled: bool) -> SearchFuture<'_, ()> {
        Box::pin(async move {
            let mut updater = self.updater.lock().await;
            if let Some(task) = updater.take() {
                task.abort();
            }
            self.rutor.unload();
            if !enabled {
                return;
            }
            let rutor = Arc::clone(&self.rutor);
            let client = self.client.clone();
            *updater = Some(tokio::spawn(async move {
                if !rutor.update(&client).await {
                    rutor.load();
                }
                loop {
                    tokio::time::sleep(RUTOR_UPDATE_INTERVAL).await;
                    rutor.update(&client).await;
                }
            }));
        })
    }

    fn rutor(&self, query: &str) -> Vec<TorrentDetails> {
        self.rutor.search(query)
    }

    fn torznab<'a>(
        &'a self,
        indexers: &'a [Indexer],
        query: &'a str,
        index: i64,
    ) -> SearchFuture<'a, Vec<TorrentDetails>> {
        Box::pin(async move {
            let usable = |indexer: &Indexer| !indexer.host.is_empty() && !indexer.key.is_empty();
            if let Some(indexer) = usize::try_from(index)
                .ok()
                .and_then(|index| indexers.get(index))
            {
                if !usable(indexer) {
                    return Vec::new();
                }
                return torznab::search_one(&self.client, indexer, query)
                    .await
                    .unwrap_or_default();
            }
            let mut all = Vec::new();
            for indexer in indexers.iter().filter(|indexer| usable(indexer)) {
                all.extend(
                    torznab::search_one(&self.client, indexer, query)
                        .await
                        .unwrap_or_default(),
                );
            }
            all
        })
    }

    fn torznab_test<'a>(
        &'a self,
        host: &'a str,
        key: &'a str,
    ) -> SearchFuture<'a, Result<(), String>> {
        Box::pin(torznab::test(&self.client, host, key))
    }
}
