//! MatriX.145's FUSE mount (`--fusepath`, `server/torrfs/fuse` over
//! `hanwen/go-fuse`): the torrent file system, read-only, with go-fuse's
//! answers for the operations it does not implement.

use std::{
    collections::HashMap,
    ffi::OsStr,
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fuser::{
    BackgroundSession, BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem,
    FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr,
    ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, Request, Session,
    SessionACL, TimeOrNow,
};
use nix::mount::{MntFlags, MsFlags};
use rustorr_lifecycle::{ClientCore, InfoHash, PlaybackRequest};
use rustorr_vfs::{FsError, Info, Node, TorrentFs};
use tokio::{io::AsyncRead, io::AsyncReadExt, runtime::Handle};
use tracing::info;

/// go-fuse's entry and attribute timeouts in TorrServer's mount.
const TTL: Duration = Duration::from_secs(1);
/// go-fuse numbers inodes it creates from here on.
const FIRST_INO: u64 = 1 << 63;

/// A mounted torrent file system.
pub struct FuseMount {
    session: BackgroundSession,
    path: PathBuf,
    /// Mounted by this process with `mount(2)`, so unmounted by it too.
    direct: bool,
}

impl FuseMount {
    /// `FuseFS.Mount`: creates the mount point and mounts with TorrServer's
    /// options. The runtime serves the file system's reads.
    pub fn mount(core: Arc<dyn ClientCore>, path: &Path, runtime: Handle) -> io::Result<Self> {
        std::fs::create_dir_all(path)?;
        let mut config = Config::default();
        config.mount_options = vec![
            MountOption::FSName("torrserver-fuse".into()),
            MountOption::Subtype("torrserver".into()),
            MountOption::CUSTOM("max_read=131072".into()),
        ];
        config.acl = SessionACL::All;
        let filesystem = TorrentFuse::new(TorrentFs::new(core), runtime);
        let (session, direct) = match direct_mount(path)? {
            Some(fd) => {
                let session = Session::from_fd(filesystem, fd, SessionACL::All, config)?;
                (session.spawn()?, true)
            }
            // Without the right to mount, fusermount3 mounts for us.
            None => (fuser::spawn_mount(filesystem, path, &config)?, false),
        };
        info!(path = %path.display(), "FUSE filesystem mounted");
        Ok(Self {
            session,
            path: path.to_owned(),
            direct,
        })
    }

    pub fn unmount(self) -> io::Result<()> {
        if self.direct {
            nix::mount::umount2(&self.path, MntFlags::MNT_DETACH).map_err(io::Error::from)?;
            self.session.join()?;
        } else {
            self.session.umount_and_join()?;
        }
        info!(path = %self.path.display(), "FUSE filesystem unmounted");
        Ok(())
    }
}

/// Mounts `/dev/fuse` at `path` as go-fuse's fusermount would: type
/// `fuse.torrserver`, source `torrserver-fuse`, `nosuid,nodev`, open to other
/// users. `None` when this process may not mount.
fn direct_mount(path: &Path) -> io::Result<Option<OwnedFd>> {
    let device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")?;
    let options = format!(
        "fd={},rootmode=40000,user_id={},group_id={},allow_other,max_read=131072",
        device.as_raw_fd(),
        nix::unistd::getuid().as_raw(),
        nix::unistd::getgid().as_raw()
    );
    match nix::mount::mount(
        Some("torrserver-fuse"),
        path,
        Some("fuse.torrserver"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some(options.as_str()),
    ) {
        Ok(()) => Ok(Some(OwnedFd::from(device))),
        Err(nix::errno::Errno::EPERM) => Ok(None),
        Err(error) => Err(io::Error::from(error)),
    }
}

type Reader = Pin<Box<dyn AsyncRead + Send>>;

/// An open file: its torrent file and a reader kept at its position.
struct OpenFile {
    hash: InfoHash,
    index: u32,
    size: u64,
    reader: Option<(Reader, u64)>,
}

struct TorrentFuse {
    fs: TorrentFs,
    runtime: Handle,
    uid: u32,
    gid: u32,
    /// Paths by inode number and back; the root is inode 1.
    inodes: Mutex<Inodes>,
    files: Mutex<HashMap<u64, Arc<Mutex<OpenFile>>>>,
    next_handle: AtomicU64,
}

#[derive(Default)]
struct Inodes {
    paths: HashMap<u64, String>,
    numbers: HashMap<String, u64>,
    next: u64,
}

impl Inodes {
    fn number(&mut self, path: &str) -> u64 {
        if let Some(&number) = self.numbers.get(path) {
            return number;
        }
        let number = FIRST_INO + self.next;
        self.next += 1;
        self.numbers.insert(path.into(), number);
        self.paths.insert(number, path.into());
        number
    }
}

fn errno(error: FsError) -> Errno {
    match error {
        FsError::NotFound => Errno::ENOENT,
        FsError::Invalid => Errno::EINVAL,
    }
}

impl TorrentFuse {
    fn new(fs: TorrentFs, runtime: Handle) -> Self {
        Self {
            fs,
            runtime,
            // go-fuse's UID and GID options: the process's own.
            uid: nix::unistd::getuid().as_raw(),
            gid: nix::unistd::getgid().as_raw(),
            inodes: Mutex::new(Inodes::default()),
            files: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    fn path(&self, ino: INodeNo) -> Option<String> {
        if ino == INodeNo::ROOT {
            return Some(".".into());
        }
        self.inodes.lock().ok()?.paths.get(&ino.0).cloned()
    }

    /// `fillAttr`.
    fn attr(&self, ino: u64, info: &Info) -> FileAttr {
        let time = UNIX_EPOCH + Duration::from_secs(u64::try_from(info.mtime).unwrap_or(0));
        let size = if info.is_dir { 4096 } else { info.size };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind: if info.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: u16::try_from(info.mode & 0o777).unwrap_or(0),
            nlink: 0,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn stat(&self, path: &str) -> Result<Info, FsError> {
        self.runtime.block_on(self.fs.stat(path))
    }
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "." {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

impl Filesystem for TorrentFuse {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent) = self.path(parent) else {
            return reply.error(Errno::ENOENT);
        };
        let path = child_path(&parent, &name.to_string_lossy());
        match self.stat(&path) {
            Ok(info) => {
                let ino = self
                    .inodes
                    .lock()
                    .map_or(0, |mut inodes| inodes.number(&path));
                reply.entry(&TTL, &self.attr(ino, &info), Generation(0));
            }
            Err(error) => reply.error(errno(error)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Some(path) = self.path(ino) else {
            return reply.error(Errno::ENOENT);
        };
        // go-fuse reports the root with inode number 0.
        let number = if ino == INodeNo::ROOT { 0 } else { ino.0 };
        match self.stat(&path) {
            Ok(info) => reply.attr(&TTL, &self.attr(number, &info)),
            Err(error) => reply.error(errno(error)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(path) = self.path(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let children = self.runtime.block_on(async {
            let node = self.fs.open(&path).await?;
            self.fs.read_dir(&node).await
        });
        let children = match children {
            Ok(children) => children,
            Err(error) => return reply.error(errno(error)),
        };
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (index, child) in children.iter().enumerate().skip(skip) {
            let info = child.info();
            let child_ino = self.inodes.lock().map_or(0, |mut inodes| {
                inodes.number(&child_path(&path, &info.name))
            });
            let kind = if info.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            if reply.add(INodeNo(child_ino), index as u64 + 1, kind, &info.name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.0
            & (libc::O_WRONLY | libc::O_RDWR | libc::O_APPEND | libc::O_TRUNC | libc::O_CREAT)
            != 0
        {
            return reply.error(Errno::EROFS);
        }
        let Some(path) = self.path(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let node = match self.runtime.block_on(self.fs.open(&path)) {
            Ok(node) => node,
            Err(error) => return reply.error(errno(error)),
        };
        let Node::File { torrent, file, .. } = node else {
            // Directories do not read and seek.
            return reply.error(Errno::ENOSYS);
        };
        let Some(hash) = torrent.hash.as_deref().and_then(|hash| hash.parse().ok()) else {
            return reply.error(Errno::EINVAL);
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let open = OpenFile {
            hash,
            index: file.id,
            size: file.length,
            reader: None,
        };
        if let Ok(mut files) = self.files.lock() {
            files.insert(handle, Arc::new(Mutex::new(open)));
        }
        reply.opened(FileHandle(handle), FopenFlags::FOPEN_DIRECT_IO);
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(open) = self
            .files
            .lock()
            .ok()
            .and_then(|files| files.get(&fh.0).cloned())
        else {
            return reply.error(Errno::EBADF);
        };
        let Ok(mut open) = open.lock() else {
            return reply.error(Errno::EIO);
        };
        let result = self
            .runtime
            .block_on(read_at(self.fs.core(), &mut open, offset, size));
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Ok(mut files) = self.files.lock() {
            files.remove(&fh.0);
        }
        reply.ok();
    }

    fn create(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        reply.error(Errno::EROFS);
    }

    // go-fuse's answers for operations TorrServer's nodes do not implement:
    // creation and attribute changes are refused as read-only, directories
    // and renames are unsupported, and removals report success without
    // removing anything.
    fn setattr(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        reply.error(Errno::EROFS);
    }

    fn mkdir(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::ENOTSUP);
    }

    fn unlink(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.ok();
    }

    fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        reply.ok();
    }

    fn rename(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _newparent: INodeNo,
        _newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        reply.error(Errno::ENOTSUP);
    }
}

/// Reads up to `size` bytes at `offset`, reusing the handle's reader when
/// the read continues where the last one ended.
async fn read_at(
    core: &Arc<dyn ClientCore>,
    open: &mut OpenFile,
    offset: u64,
    size: u32,
) -> io::Result<Vec<u8>> {
    if offset >= open.size {
        return Ok(Vec::new());
    }
    let wanted = u64::from(size).min(open.size - offset);
    let reuse = matches!(&open.reader, Some((_, position)) if *position == offset);
    if !reuse {
        let playback = Arc::clone(core)
            .playback(PlaybackRequest {
                hash: open.hash,
                index: open.index,
                offset,
                end: Some(open.size - 1),
                prefetch_offset: None,
            })
            .await
            .map_err(io::Error::other)?;
        open.reader = Some((playback.reader, offset));
    }
    let (reader, position) = open.reader.as_mut().expect("just set");
    let mut buffer = vec![0; usize::try_from(wanted).map_err(io::Error::other)?];
    let mut filled = 0;
    while filled < buffer.len() {
        let count = reader.read(&mut buffer[filled..]).await?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    buffer.truncate(filled);
    *position += filled as u64;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_numbers_follow_go_fuse() {
        let mut inodes = Inodes::default();
        assert_eq!(inodes.number("other"), FIRST_INO);
        assert_eq!(inodes.number("other/a"), FIRST_INO + 1);
        assert_eq!(inodes.number("other"), FIRST_INO);
        assert_eq!(child_path(".", "other"), "other");
        assert_eq!(child_path("other", "a"), "other/a");
    }
}
