use std::{
    collections::HashMap,
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, ReadBuf};

use crate::InfoHash;
use crate::{
    CacheCommand, CacheView, ClientCore, ClientFuture, Error, Playback, PlaybackRequest, Settings,
    SettingsCommand, TorrentCommand, TorrentReply, TorrentView, ViewedCommand, ViewedFile,
    WafCommand, WafLists,
};

#[derive(Clone)]
struct StoredTorrent {
    view: TorrentView,
    files: HashMap<u32, Arc<Vec<u8>>>,
}

#[derive(Default)]
struct MemoryState {
    torrents: HashMap<InfoHash, StoredTorrent>,
    settings: Settings,
    viewed: Vec<ViewedFile>,
    waf: WafLists,
}

/// Deterministic adapter for HTTP contract tests. Tests seed complete torrent
/// views and bytes directly, so no BitTorrent engine, disk cache, or SQLite is
/// involved.
#[derive(Default)]
pub struct InMemoryClientCore {
    state: Mutex<MemoryState>,
}

impl InMemoryClientCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, view: TorrentView, files: HashMap<u32, Vec<u8>>) -> Result<(), Error> {
        let hash = view
            .hash()
            .ok_or_else(|| Error::LocalSource(io::Error::other("view has no info hash")))?;
        self.state().torrents.insert(
            hash,
            StoredTorrent {
                view,
                files: files
                    .into_iter()
                    .map(|(index, bytes)| (index, Arc::new(bytes)))
                    .collect(),
            },
        );
        Ok(())
    }

    fn state(&self) -> MutexGuard<'_, MemoryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct MemoryReader {
    bytes: Arc<Vec<u8>>,
    position: usize,
}

impl AsyncRead for MemoryReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let available = self.bytes.len().saturating_sub(self.position);
        let count = available.min(buffer.remaining());
        if count != 0 {
            let end = self.position + count;
            buffer.put_slice(&self.bytes[self.position..end]);
            self.position = end;
        }
        Poll::Ready(Ok(()))
    }
}

impl ClientCore for InMemoryClientCore {
    fn torrents(&self, command: TorrentCommand) -> ClientFuture<'_, TorrentReply> {
        Box::pin(async move {
            let mut state = self.state();
            match command {
                TorrentCommand::Add(request) => {
                    let hash = request.link.parse::<InfoHash>().map_err(|_| {
                        Error::LocalSource(io::Error::other("unknown in-memory link"))
                    })?;
                    Ok(TorrentReply::Torrent(
                        state
                            .torrents
                            .get(&hash)
                            .map(|torrent| Box::new(torrent.view.clone())),
                    ))
                }
                TorrentCommand::AddMetainfo { .. } => Err(Error::LocalSource(io::Error::other(
                    "in-memory adapter does not decode metainfo",
                ))),
                TorrentCommand::Get(hash) => Ok(TorrentReply::Torrent(
                    state
                        .torrents
                        .get(&hash)
                        .map(|torrent| Box::new(torrent.view.clone())),
                )),
                TorrentCommand::Set(update) => {
                    if let Some(torrent) = state.torrents.get_mut(&update.hash) {
                        torrent.view.title = update.title;
                        torrent.view.poster = update.poster;
                        torrent.view.category = update.category;
                        if !update.data.is_empty() {
                            torrent.view.data = Some(update.data);
                        }
                    }
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::Remove(hash) | TorrentCommand::Drop(hash) => {
                    state.torrents.remove(&hash);
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::List => Ok(TorrentReply::List(
                    state
                        .torrents
                        .values()
                        .map(|torrent| torrent.view.clone())
                        .collect(),
                )),
                TorrentCommand::Wipe => {
                    state.torrents.clear();
                    Ok(TorrentReply::Empty)
                }
            }
        })
    }

    fn playback(self: Arc<Self>, request: PlaybackRequest) -> ClientFuture<'static, Playback> {
        Box::pin(async move {
            let state = self.state();
            let torrent = state
                .torrents
                .get(&request.hash)
                .ok_or(Error::NotFound(request.hash))?;
            let file = torrent
                .view
                .file_stats
                .iter()
                .find(|file| file.id == request.index)
                .ok_or(Error::UnknownFile {
                    hash: request.hash,
                    index: request.index,
                })?;
            let bytes = torrent
                .files
                .get(&request.index)
                .cloned()
                .ok_or(Error::UnknownFile {
                    hash: request.hash,
                    index: request.index,
                })?;
            let position = usize::try_from(request.offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            Ok(Playback {
                reader: Box::pin(MemoryReader { bytes, position }),
                hash: request.hash,
                index: request.index,
                path: file.path.clone(),
                length: file.length,
                timestamp: torrent.view.timestamp,
            })
        })
    }

    fn settings(&self, command: SettingsCommand) -> ClientFuture<'_, Settings> {
        Box::pin(async move {
            let mut state = self.state();
            match command {
                SettingsCommand::Get => {}
                SettingsCommand::Set(settings) => state.settings = (*settings).normalized(),
                SettingsCommand::Defaults => state.settings = Settings::default(),
            }
            Ok(state.settings.clone())
        })
    }

    fn viewed(&self, command: ViewedCommand) -> ClientFuture<'_, Vec<ViewedFile>> {
        Box::pin(async move {
            let mut state = self.state();
            match command {
                ViewedCommand::Set {
                    hash,
                    index,
                    timecode,
                } => {
                    let timecode = if state.settings.track_timecode {
                        timecode
                    } else {
                        0.0
                    };
                    if let Some(entry) = state
                        .viewed
                        .iter_mut()
                        .find(|entry| entry.hash == hash && entry.index == index)
                    {
                        entry.timecode = timecode;
                    } else {
                        state.viewed.push(ViewedFile {
                            hash,
                            index,
                            timecode,
                        });
                    }
                    Ok(Vec::new())
                }
                ViewedCommand::Remove { hash, index } => {
                    state.viewed.retain(|entry| {
                        entry.hash != hash || index.is_some_and(|index| entry.index != index)
                    });
                    Ok(Vec::new())
                }
                ViewedCommand::List { hash } => Ok(state
                    .viewed
                    .iter()
                    .filter(|entry| hash.is_none_or(|hash| hash == entry.hash))
                    .cloned()
                    .collect()),
            }
        })
    }

    fn cache(&self, _: CacheCommand) -> ClientFuture<'_, CacheView> {
        Box::pin(async move {
            Ok(CacheView {
                snapshots: Vec::new(),
                capacity: self.state().settings.cache_cap(),
                filled: 0,
            })
        })
    }

    fn waf(&self, command: WafCommand) -> ClientFuture<'_, WafLists> {
        Box::pin(async move {
            let mut state = self.state();
            if let WafCommand::Set(lists) = command {
                state.waf = lists;
            }
            Ok(state.waf.clone())
        })
    }
}
