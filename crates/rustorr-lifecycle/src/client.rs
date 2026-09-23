use std::{future::Future, pin::Pin, sync::Arc};

use rustorr_cache::TorrentSnapshot;
use rustorr_domain::{FileIndex, InfoHash};
use rustorr_state::ViewedEntry;
use tokio::io::AsyncRead;

use crate::{
    AddTorrent, Error, MagnetView, Settings, TorrentCoordinator, TorrentView, UpdateTorrent,
};

pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub enum TorrentCommand {
    Add(AddTorrent),
    AddMetainfo {
        bytes: Vec<u8>,
        request: AddTorrent,
    },
    Get(InfoHash),
    Set(UpdateTorrent),
    Remove(InfoHash),
    List,
    Drop(InfoHash),
    Wipe,
    /// Saved torrents as `/magnets` lists them.
    Magnets,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TorrentReply {
    Torrent(Option<Box<TorrentView>>),
    List(Vec<TorrentView>),
    Magnets(Vec<MagnetView>),
    Empty,
}

#[derive(Debug, Clone)]
pub struct PlaybackRequest {
    pub hash: InfoHash,
    pub index: u32,
    pub offset: u64,
    pub end: Option<u64>,
    pub prefetch_offset: Option<u64>,
}

pub struct Playback {
    pub reader: Pin<Box<dyn AsyncRead + Send>>,
    pub hash: InfoHash,
    pub index: u32,
    pub path: String,
    pub length: u64,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub enum SettingsCommand {
    Get,
    Set(Box<Settings>),
    Defaults,
}

#[derive(Debug, Clone)]
pub enum ViewedCommand {
    Set {
        hash: InfoHash,
        index: u32,
        timecode: f64,
    },
    Remove {
        hash: InfoHash,
        index: Option<u32>,
    },
    List {
        hash: Option<InfoHash>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewedFile {
    pub hash: InfoHash,
    pub index: u32,
    pub timecode: f64,
}

#[derive(Debug, Clone)]
pub enum CacheCommand {
    Get(InfoHash),
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheView {
    pub snapshots: Vec<TorrentSnapshot>,
    pub capacity: u64,
    pub filled: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WafLists {
    pub whitelist: String,
    pub blacklist: String,
    pub referers: String,
}

#[derive(Debug, Clone)]
pub enum WafCommand {
    Get,
    Set(WafLists),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessPolicy {
    pub basic_auth: bool,
}

/// Small HTTP-facing seam. It carries client-domain commands, never engine,
/// cache or SQLite implementation types. The production adapter is
/// [`TorrentCoordinator`]; HTTP tests can use the in-memory adapter.
pub trait ClientCore: Send + Sync {
    fn torrents(&self, command: TorrentCommand) -> ClientFuture<'_, TorrentReply>;
    fn playback(self: Arc<Self>, request: PlaybackRequest) -> ClientFuture<'static, Playback>;
    fn settings(&self, command: SettingsCommand) -> ClientFuture<'_, Settings>;
    fn viewed(&self, command: ViewedCommand) -> ClientFuture<'_, Vec<ViewedFile>>;
    fn cache(&self, command: CacheCommand) -> ClientFuture<'_, CacheView>;
    fn waf(&self, command: WafCommand) -> ClientFuture<'_, WafLists>;
}

impl ClientCore for TorrentCoordinator {
    fn torrents(&self, command: TorrentCommand) -> ClientFuture<'_, TorrentReply> {
        Box::pin(async move {
            match command {
                TorrentCommand::Add(request) => self
                    .add_torrent(request)
                    .await
                    .map(|view| TorrentReply::Torrent(Some(Box::new(view)))),
                TorrentCommand::AddMetainfo { bytes, request } => self
                    .add_metainfo(bytes, request)
                    .await
                    .map(|view| TorrentReply::Torrent(Some(Box::new(view)))),
                TorrentCommand::Get(hash) => self
                    .get_view(hash)
                    .await
                    .map(|view| TorrentReply::Torrent(view.map(Box::new))),
                TorrentCommand::Set(update) => {
                    self.update_torrent(update).await?;
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::Remove(hash) => {
                    self.remove_torrent(hash).await?;
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::List => self.list_views().await.map(TorrentReply::List),
                TorrentCommand::Drop(hash) => {
                    self.drop_live(hash).await?;
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::Wipe => {
                    self.wipe().await?;
                    Ok(TorrentReply::Empty)
                }
                TorrentCommand::Magnets => self.magnets().await.map(TorrentReply::Magnets),
            }
        })
    }

    fn playback(self: Arc<Self>, request: PlaybackRequest) -> ClientFuture<'static, Playback> {
        Box::pin(async move {
            let view = self
                .get_view(request.hash)
                .await?
                .ok_or(Error::NotFound(request.hash))?;
            let file = view
                .file_stats
                .iter()
                .find(|file| file.id == request.index)
                .cloned()
                .ok_or(Error::UnknownFile {
                    hash: request.hash,
                    index: request.index,
                })?;
            let reader = self
                .play_with_prefetch(
                    request.hash,
                    request.index,
                    request.offset,
                    request.end,
                    request.prefetch_offset,
                )
                .await?;
            let length = reader.file_length();
            Ok(Playback {
                reader: Box::pin(reader),
                hash: request.hash,
                index: request.index,
                path: file.path,
                length,
                timestamp: view.timestamp,
            })
        })
    }

    fn settings(&self, command: SettingsCommand) -> ClientFuture<'_, Settings> {
        Box::pin(async move {
            match command {
                SettingsCommand::Get => Ok(TorrentCoordinator::settings(self)),
                SettingsCommand::Set(settings) => {
                    self.set_settings(*settings).await?;
                    Ok(TorrentCoordinator::settings(self))
                }
                SettingsCommand::Defaults => {
                    self.reset_settings().await?;
                    Ok(TorrentCoordinator::settings(self))
                }
            }
        })
    }

    fn viewed(&self, command: ViewedCommand) -> ClientFuture<'_, Vec<ViewedFile>> {
        Box::pin(async move {
            match command {
                ViewedCommand::Set {
                    hash,
                    index,
                    timecode,
                } => {
                    let index = index.checked_sub(1).ok_or(Error::InvalidIndex(index))?;
                    let timecode = if self.settings().track_timecode {
                        timecode
                    } else {
                        0.0
                    };
                    self.state.set_viewed(&ViewedEntry {
                        torrent: hash,
                        file: FileIndex::from_zero_based(index),
                        timecode,
                    })?;
                    Ok(Vec::new())
                }
                ViewedCommand::Remove { hash, index } => {
                    let file = index
                        .map(|index| {
                            index
                                .checked_sub(1)
                                .map(FileIndex::from_zero_based)
                                .ok_or(Error::InvalidIndex(index))
                        })
                        .transpose()?;
                    self.state.remove_viewed(hash, file)?;
                    Ok(Vec::new())
                }
                ViewedCommand::List { hash } => Ok(self
                    .state
                    .list_viewed()?
                    .into_iter()
                    .filter(|entry| hash.is_none_or(|hash| entry.torrent == hash))
                    .map(|entry| ViewedFile {
                        hash: entry.torrent,
                        index: entry.file.zero_based() + 1,
                        timecode: entry.timecode,
                    })
                    .collect()),
            }
        })
    }

    fn cache(&self, command: CacheCommand) -> ClientFuture<'_, CacheView> {
        Box::pin(async move {
            let stats = self.cache.stats();
            let snapshots = match command {
                CacheCommand::Get(hash) => vec![self.cache.snapshot(hash)?],
                CacheCommand::List => self.cache.snapshots(),
            };
            Ok(CacheView {
                snapshots,
                capacity: stats.cap_bytes,
                filled: stats.stored_bytes,
            })
        })
    }

    fn waf(&self, command: WafCommand) -> ClientFuture<'_, WafLists> {
        Box::pin(async move {
            match command {
                WafCommand::Get => {
                    let lists = self.state.waf_lists()?;
                    Ok(WafLists {
                        whitelist: lists.whitelist,
                        blacklist: lists.blacklist,
                        referers: lists.referers,
                    })
                }
                WafCommand::Set(lists) => {
                    self.state.set_waf_lists(&rustorr_state::WafLists {
                        whitelist: lists.whitelist.clone(),
                        blacklist: lists.blacklist.clone(),
                        referers: lists.referers.clone(),
                    })?;
                    Ok(lists)
                }
            }
        })
    }
}
