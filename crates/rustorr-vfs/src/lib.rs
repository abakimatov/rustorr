//! The torrent file system MatriX.145 exposes over WebDAV and FUSE
//! (`server/torrfs`): categories at the top, torrents inside them, and each
//! torrent's files under their display paths. Read-only; file contents are
//! read through the client core's playback.

use std::{sync::Arc, time::Duration};

use rustorr_lifecycle::{
    AddTorrent, ClientCore, SettingsCommand, TorrentCommand, TorrentFileView, TorrentReply,
    TorrentView,
};

/// `torrfs` fixes these modification times for the synthetic directories.
const ROOT_MTIME: i64 = 477_033_600;
const CATEGORY_MTIME: i64 = 477_033_666;
const DIR_SIZE: u64 = 4096;

/// Why a path cannot be opened, in the classes WebDAV and FUSE tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// `fs.ErrNotExist`.
    NotFound,
    /// `fs.ErrInvalid`: not a valid `io/fs` path, or an operation the node
    /// does not support.
    Invalid,
}

/// A node's `fs.FileInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub name: String,
    pub size: u64,
    /// Permission bits: `0o555` for directories, `0o444` for files.
    pub mode: u32,
    /// Unix seconds.
    pub mtime: i64,
    pub is_dir: bool,
}

/// An opened node.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Root,
    Category(String),
    /// A torrent's directory; `path` is the display path inside the torrent,
    /// empty for the torrent itself.
    Dir {
        torrent: Box<TorrentView>,
        name: String,
        path: String,
    },
    File {
        torrent: Box<TorrentView>,
        name: String,
        file: TorrentFileView,
    },
}

impl Node {
    pub fn info(&self) -> Info {
        match self {
            Self::Root => dir_info("/", ROOT_MTIME),
            Self::Category(name) => dir_info(name, CATEGORY_MTIME),
            Self::Dir { torrent, name, .. } => dir_info(name, torrent.timestamp),
            Self::File {
                torrent,
                name,
                file,
            } => Info {
                name: name.clone(),
                size: file.length,
                mode: 0o444,
                mtime: torrent.timestamp,
                is_dir: false,
            },
        }
    }
}

fn dir_info(name: &str, mtime: i64) -> Info {
    Info {
        name: name.into(),
        size: DIR_SIZE,
        mode: 0o555,
        mtime,
        is_dir: true,
    }
}

/// `SanitizeName`: slashes become underscores, control characters vanish,
/// and names `path.Clean` would collapse become empty.
pub fn sanitize_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .filter_map(|character| match character {
            '/' | '\\' => Some('_'),
            '\u{0}'..='\u{1f}' | '\u{7f}' => None,
            other => Some(other),
        })
        .collect();
    let trimmed = mapped.trim();
    if matches!(trimmed, "" | "." | "..") {
        String::new()
    } else {
        trimmed.into()
    }
}

fn category_name(category: &str) -> String {
    let name = sanitize_name(category);
    if name.is_empty() {
        "other".into()
    } else {
        name
    }
}

fn torrent_name(torrent: &TorrentView) -> String {
    let name = sanitize_name(&torrent.title);
    if name.is_empty() {
        torrent
            .hash
            .clone()
            .unwrap_or_default()
            .to_ascii_lowercase()
    } else {
        name
    }
}

/// `GotInfo`: a loaded torrent with its file list.
fn got_info(torrent: &TorrentView) -> bool {
    torrent.stat < 4 && !torrent.file_stats.is_empty()
}

/// A file's path as anacrolix `File.DisplayPath` gives it: without the
/// torrent's name for a multi-file torrent.
fn display_path<'a>(torrent: &TorrentView, file: &'a TorrentFileView) -> &'a str {
    let name = torrent.name.as_deref().unwrap_or_default();
    file.path
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(&file.path)
}

/// `fs.ValidPath`.
fn valid_path(name: &str) -> bool {
    if name == "." {
        return true;
    }
    !name.is_empty()
        && name
            .split('/')
            .all(|element| !matches!(element, "" | "." | ".."))
}

/// Go's `path.Clean`.
pub fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// The file system over one client core.
#[derive(Clone)]
pub struct TorrentFs {
    core: Arc<dyn ClientCore>,
    /// How long to wait between polls for a torrent's metadata.
    poll: Duration,
}

impl TorrentFs {
    pub fn new(core: Arc<dyn ClientCore>) -> Self {
        Self {
            core,
            poll: Duration::from_millis(500),
        }
    }

    pub fn core(&self) -> &Arc<dyn ClientCore> {
        &self.core
    }

    async fn torrents(&self) -> Vec<TorrentView> {
        match self.core.torrents(TorrentCommand::List).await {
            Ok(TorrentReply::List(torrents)) => torrents,
            _ => Vec::new(),
        }
    }

    /// `ioFSAdapter.Open`: a slash-separated path, `.`, `/` or empty for the
    /// root.
    pub async fn open(&self, name: &str) -> Result<Node, FsError> {
        let name = clean(name);
        if matches!(name.as_str(), "." | "/" | "") {
            return Ok(Node::Root);
        }
        let name = name.trim_start_matches('/');
        if !valid_path(name) {
            return Err(FsError::Invalid);
        }
        let mut node = Node::Root;
        let mut elements = name.split('/');
        while let Some(element) = elements.next() {
            if matches!(node, Node::File { .. }) {
                // A file opens whatever path continues below it.
                break;
            }
            let children = self.read_dir(&node).await?;
            node = children
                .into_iter()
                .find(|child| child.info().name == element)
                .ok_or(FsError::NotFound)?;
            if elements.clone().next().is_none() {
                break;
            }
        }
        Ok(node)
    }

    /// `fs.Stat`.
    pub async fn stat(&self, name: &str) -> Result<Info, FsError> {
        self.open(name).await.map(|node| node.info())
    }

    /// The children of a directory node, in name order (the reference lists
    /// them in Go map order).
    pub async fn read_dir(&self, node: &Node) -> Result<Vec<Node>, FsError> {
        let mut children = match node {
            Node::Root => {
                let mut categories: Vec<String> = self
                    .torrents()
                    .await
                    .iter()
                    .map(|torrent| category_name(&torrent.category))
                    .collect();
                categories.sort();
                categories.dedup();
                categories.into_iter().map(Node::Category).collect()
            }
            Node::Category(category) => {
                let only_loaded = self
                    .core
                    .settings(SettingsCommand::Get)
                    .await
                    .map_or(true, |settings| settings.show_fs_active_torr);
                return Ok(self
                    .torrents()
                    .await
                    .into_iter()
                    .filter(|torrent| category_name(&torrent.category) == *category)
                    .filter(|torrent| !only_loaded || got_info(torrent))
                    .map(|torrent| Node::Dir {
                        name: torrent_name(&torrent),
                        torrent: Box::new(torrent),
                        path: String::new(),
                    })
                    .collect());
            }
            Node::Dir {
                torrent,
                name: _,
                path,
            } => {
                let torrent = self.loaded(torrent).await?;
                let mut children: Vec<Node> = Vec::new();
                for file in &torrent.file_stats {
                    let display = display_path(&torrent, file);
                    let relative = if path.is_empty() {
                        display
                    } else {
                        match display
                            .strip_prefix(path.as_str())
                            .and_then(|rest| rest.strip_prefix('/'))
                        {
                            Some(rest) => rest,
                            None => continue,
                        }
                    };
                    let Some(first) = relative.split('/').next().filter(|first| !first.is_empty())
                    else {
                        continue;
                    };
                    let is_file = !relative.contains('/');
                    if is_file {
                        children.retain(|child| child.info().name != first);
                        children.push(Node::File {
                            torrent: Box::new(torrent.clone()),
                            name: first.into(),
                            file: file.clone(),
                        });
                    } else if !children.iter().any(|child| child.info().name == first) {
                        children.push(Node::Dir {
                            torrent: Box::new(torrent.clone()),
                            name: first.into(),
                            path: if path.is_empty() {
                                first.into()
                            } else {
                                format!("{path}/{first}")
                            },
                        });
                    }
                }
                children
            }
            Node::File { .. } => return Err(FsError::Invalid),
        };
        children.sort_by(|left, right| left.info().name.cmp(&right.info().name));
        Ok(children)
    }

    /// A torrent's view with its file list: the reference loads a torrent
    /// without metadata when its directory is read, for up to twice the
    /// disconnect timeout in half-second steps.
    async fn loaded(&self, torrent: &TorrentView) -> Result<TorrentView, FsError> {
        if got_info(torrent) {
            return Ok(torrent.clone());
        }
        let hash = torrent
            .hash
            .clone()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let attempts = self
            .core
            .settings(SettingsCommand::Get)
            .await
            .map_or(60, |settings| {
                settings.torrent_disconnect_timeout.max(0) * 2
            });
        for _ in 0..attempts {
            let reply = self
                .core
                .torrents(TorrentCommand::Add(AddTorrent {
                    link: hash.clone(),
                    ..AddTorrent::default()
                }))
                .await;
            if let Ok(TorrentReply::Torrent(Some(view))) = reply
                && got_info(&view)
            {
                return Ok(*view);
            }
            tokio::time::sleep(self.poll).await;
        }
        Err(FsError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rustorr_lifecycle::InMemoryClientCore;

    use super::*;

    fn view(
        hash: &str,
        title: &str,
        category: &str,
        name: &str,
        files: &[(&str, u64)],
    ) -> TorrentView {
        TorrentView {
            title: title.into(),
            category: category.into(),
            poster: String::new(),
            data: None,
            timestamp: 1_700_000_000,
            name: Some(name.into()),
            hash: Some(hash.into()),
            torrs_hash: None,
            stat: 3,
            stat_string: "Torrent working".into(),
            loaded_size: None,
            torrent_size: None,
            download_speed: None,
            upload_speed: None,
            total_peers: None,
            active_peers: None,
            connected_seeders: None,
            bytes_written: None,
            bytes_read: None,
            file_stats: files
                .iter()
                .enumerate()
                .map(|(index, (path, length))| TorrentFileView {
                    id: u32::try_from(index).unwrap() + 1,
                    path: (*path).into(),
                    length: *length,
                    engine_index: u32::try_from(index).unwrap(),
                })
                .collect(),
        }
    }

    fn fs() -> TorrentFs {
        let core = Arc::new(InMemoryClientCore::new());
        core.insert(
            view(
                "0101010101010101010101010101010101010101",
                "Show/Season",
                "",
                "Show",
                &[
                    ("Show/01 A/one.mkv", 10),
                    ("Show/02 B/two.mkv", 20),
                    ("Show/notes.txt", 3),
                ],
            ),
            HashMap::new(),
        )
        .unwrap();
        core.insert(
            view(
                "0202020202020202020202020202020202020202",
                "",
                "movie",
                "film.mp4",
                &[("film.mp4", 7)],
            ),
            HashMap::new(),
        )
        .unwrap();
        TorrentFs::new(core)
    }

    #[tokio::test]
    async fn the_tree_is_categories_torrents_and_display_paths() {
        let fs = fs();
        let names = |nodes: Vec<Node>| {
            nodes
                .iter()
                .map(|node| node.info().name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(fs.read_dir(&Node::Root).await.unwrap()),
            ["movie", "other"]
        );
        let other = fs.open("other").await.unwrap();
        assert_eq!(names(fs.read_dir(&other).await.unwrap()), ["Show_Season"]);
        let show = fs.open("/other/Show_Season").await.unwrap();
        assert_eq!(
            names(fs.read_dir(&show).await.unwrap()),
            ["01 A", "02 B", "notes.txt"]
        );
        let file = fs.open("other/Show_Season/01 A/one.mkv").await.unwrap();
        assert_eq!(file.info().size, 10);
        assert_eq!(file.info().mode, 0o444);
        // An untitled torrent is named by its hash; a single file keeps its name.
        let movie = fs
            .open("movie/0202020202020202020202020202020202020202")
            .await
            .unwrap();
        assert_eq!(names(fs.read_dir(&movie).await.unwrap()), ["film.mp4"]);
    }

    #[tokio::test]
    async fn opening_follows_the_reference_rules() {
        let fs = fs();
        assert_eq!(fs.open(".").await.unwrap(), Node::Root);
        assert_eq!(fs.stat("").await.unwrap().mtime, ROOT_MTIME);
        assert_eq!(fs.stat("movie").await.unwrap().mtime, CATEGORY_MTIME);
        assert_eq!(fs.open("nope").await.unwrap_err(), FsError::NotFound);
        assert_eq!(fs.open("../x").await.unwrap_err(), FsError::Invalid);
        // A path below a file opens the file.
        let below = fs.open("other/Show_Season/notes.txt/extra").await.unwrap();
        assert_eq!(below.info().name, "notes.txt");
    }

    #[test]
    fn names_are_sanitized_like_torrfs() {
        assert_eq!(sanitize_name(" a/b\\c\u{1} "), "a_b_c");
        assert_eq!(sanitize_name(".."), "");
        assert_eq!(category_name(""), "other");
    }
}
