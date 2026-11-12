use probe_rs::{
    Core,
    semihosting::{
        CloseRequest, FileLengthRequest, MAX_TRANSFER_LEN, OpenRequest, ReadRequest, RemoveRequest,
        RenameRequest, SeekRequest, SemihostingCommand, WriteRequest,
    },
};
use probe_rs_rpc::monitor::SemihostingEvent;
#[cfg(target_family = "unix")]
use std::os::unix::net::UnixStream;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    net::TcpStream,
    num::NonZeroU32,
    path::{Component, Path, PathBuf},
};

use crate::rpc::utils::semihosting_mappings::{HostTarget, Mappings};

enum FileHandle {
    Stdout,
    Stderr,
    File(File),
    TcpStream(TcpStream),
    #[cfg(target_family = "unix")]
    UnixStream(UnixStream),
}

pub struct SemihostingFileManager {
    file_handles: Vec<Option<FileHandle>>,
    mappings: Mappings,
}

impl SemihostingFileManager {
    pub fn new(mappings: Mappings) -> Self {
        Self {
            file_handles: vec![],
            mappings,
        }
    }

    pub fn can_handle(other: SemihostingCommand) -> bool {
        matches!(
            other,
            SemihostingCommand::Open(_)
                | SemihostingCommand::Close(_)
                | SemihostingCommand::WriteConsole(_)
                | SemihostingCommand::Write(_)
                | SemihostingCommand::Read(_)
                | SemihostingCommand::Seek(_)
                | SemihostingCommand::FileLength(_)
                | SemihostingCommand::Remove(_)
                | SemihostingCommand::Rename(_)
        )
    }

    pub fn handle(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
        send: &mut impl FnMut(SemihostingEvent),
    ) -> anyhow::Result<()> {
        let mut send_output = |stream: &str, bytes: Vec<u8>| {
            send(SemihostingEvent::Output {
                stream: stream.into(),
                bytes,
            });
        };

        match command {
            SemihostingCommand::Open(request) => self.handle_open(core, request),
            SemihostingCommand::Close(request) => self.handle_close(core, request),
            SemihostingCommand::Write(request) => self.handle_write(core, request, send_output),
            SemihostingCommand::WriteConsole(request) => {
                send_output("stdout", request.read(core)?.into_bytes());
                Ok(())
            }
            SemihostingCommand::Read(request) => self.handle_read(core, request),
            SemihostingCommand::Seek(request) => self.handle_seek(core, request),
            SemihostingCommand::FileLength(request) => self.handle_file_length(core, request),
            SemihostingCommand::Remove(request) => self.handle_remove(core, request),
            SemihostingCommand::Rename(request) => self.handle_rename(core, request),

            _ => Ok(()),
        }
    }

    fn open_tt(&self, mode: &str) -> Option<FileHandle> {
        match mode.as_bytes()[0] {
            b'w' => Some(FileHandle::Stdout),
            b'a' => Some(FileHandle::Stderr),
            mode => {
                tracing::warn!(
                    "Target wanted to open file :tt with mode {mode}, \
                    but probe-rs does not support this operation yet. Continuing..."
                );
                None
            }
        }
    }

    fn open_tcp_stream(&self, addr: &str) -> Option<FileHandle> {
        TcpStream::connect(addr)
            .inspect_err(|err| {
                tracing::warn!(
                    "Target wanted to open a TCP stream to {addr}, \
                    but it failed with {err:?}. Continuing..."
                )
            })
            .map(FileHandle::TcpStream)
            .ok()
    }

    #[cfg(target_family = "unix")]
    fn open_unix_stream(&self, path: &str) -> Option<FileHandle> {
        UnixStream::connect(path)
            .inspect_err(|err| {
                tracing::warn!(
                    "Target wanted to open an UNIX stream to {path}, \
                    but it failed with {err:?}. Continuing..."
                )
            })
            .map(FileHandle::UnixStream)
            .ok()
    }

    #[cfg(not(target_family = "unix"))]
    fn open_unix_stream(&self, path: &str) -> Option<FileHandle> {
        tracing::error!(
            "Target wanted to open an UNIX stream to {path}, \
            but probe-rs does not support this on the current platform. Continuing..."
        );
        None
    }

    fn open_file(&self, path: &str, mode: &str) -> Option<FileHandle> {
        let mut options = File::options();
        let flags = OpenFlags::from_mode(mode)?;
        options
            .read(flags.read)
            .write(flags.write)
            .append(flags.append)
            .truncate(flags.truncate)
            .create(flags.create)
            .open(path)
            .inspect_err(|err| {
                tracing::warn!(
                    "Target wanted to open file {path} with mode {mode}, \
                    but it failed with {err:?}. Continuing..."
                )
            })
            .map(FileHandle::File)
            .ok()
    }

    fn open_beneath(&self, root: &Path, path: &str, mode: &str) -> Option<FileHandle> {
        let flags = OpenFlags::from_mode(mode)?;
        open_beneath(root, path, flags)
            .inspect_err(|err| {
                tracing::warn!(
                    "Target wanted to open file {path:?} in {} with mode {mode}, \
                    but it failed with {err:?}. Continuing...",
                    root.display(),
                )
            })
            .map(FileHandle::File)
            .ok()
    }

    fn handle_open(&mut self, core: &mut Core<'_>, request: OpenRequest) -> anyhow::Result<()> {
        if request.path_is_too_long() {
            tracing::warn!("Target wanted to open a file with a too long path. Continuing...");
            return Ok(());
        }

        let path = request.path(core)?;

        let f = if path == ":tt" {
            self.open_tt(request.mode())
        } else if let Some(target) = self.mappings.resolve(&path) {
            match target {
                HostTarget::File(path) => self.open_file(&path, request.mode()),
                HostTarget::Beneath { root, path } => {
                    self.open_beneath(&root, &path, request.mode())
                }
                HostTarget::Tcp(addr) => self.open_tcp_stream(&addr),
                HostTarget::Unix(path) => self.open_unix_stream(&path),
            }
        } else {
            tracing::warn!(
                "Target wanted to open file {path:?} which is not configured \
                for access via semihosting. Continuing..."
            );
            None
        };

        if let Some(f) = f {
            self.file_handles.push(Some(f));
            request.respond_with_handle(
                core,
                NonZeroU32::new(self.file_handles.len() as u32).unwrap(),
            )?;
        }

        Ok(())
    }

    fn handle_close(&mut self, core: &mut Core<'_>, request: CloseRequest) -> anyhow::Result<()> {
        let handle = request.file_handle();
        if self.take_file_handle(handle).is_some() {
            self.trim_file_handles();
            request.success(core)?;
        } else {
            tracing::warn!("Target wanted to close invalid file handle {handle}. Continuing...");
        }

        Ok(())
    }

    fn handle_write(
        &mut self,
        core: &mut Core<'_>,
        request: WriteRequest,
        mut send_output: impl FnMut(&str, Vec<u8>),
    ) -> anyhow::Result<()> {
        let Some((f, log)) = self.get_file_handle("write to", request.file_handle()) else {
            return Ok(());
        };

        let buf = request.read(core)?;
        let len = match f {
            FileHandle::Stdout => {
                send_output("stdout", buf.clone());
                Some(buf.len())
            }
            FileHandle::Stderr => {
                send_output("stderr", buf.clone());
                Some(buf.len())
            }
            FileHandle::File(file) => log.inspect(file.write(&buf)),
            FileHandle::TcpStream(stream) => log.inspect(stream.write(&buf)),
            #[cfg(target_family = "unix")]
            FileHandle::UnixStream(stream) => log.inspect(stream.write(&buf)),
        };
        if let Some(len) = len {
            request.write_status(core, (request.bytes_to_write() - len as u32) as i32)?;
        }

        Ok(())
    }

    fn handle_read(&mut self, core: &mut Core<'_>, request: ReadRequest) -> anyhow::Result<()> {
        let Some((f, log)) = self.get_file_handle("read from", request.file_handle()) else {
            return Ok(());
        };

        let mut buf = vec![0; request.bytes_to_read().min(MAX_TRANSFER_LEN) as usize];
        let len = match f {
            FileHandle::File(file) => log.inspect(file.read(&mut buf)),
            FileHandle::TcpStream(stream) => log.inspect(stream.read(&mut buf)),
            #[cfg(target_family = "unix")]
            FileHandle::UnixStream(stream) => log.inspect(stream.read(&mut buf)),
            _ => {
                log.not_supported();
                None
            }
        };
        if let Some(len) = len {
            request.write_buffer_to_target(core, &buf[..len])?;
        }

        Ok(())
    }

    fn handle_seek(&mut self, core: &mut Core<'_>, request: SeekRequest) -> anyhow::Result<()> {
        let Some((f, log)) = self.get_file_handle("seek in", request.file_handle()) else {
            return Ok(());
        };

        if let FileHandle::File(file) = f {
            let pos = SeekFrom::Start(request.position() as u64);
            if log.inspect(file.seek(pos)).is_some() {
                request.success(core)?;
            }
        } else {
            log.not_supported();
        }

        Ok(())
    }

    fn handle_file_length(
        &mut self,
        core: &mut Core<'_>,
        request: FileLengthRequest,
    ) -> anyhow::Result<()> {
        let action = "read the file length of";
        let Some((f, log)) = self.get_file_handle(action, request.file_handle()) else {
            return Ok(());
        };

        if let FileHandle::File(file) = f {
            if let Some(meta) = log.inspect(file.metadata()) {
                request.write_length(core, meta.len() as i32)?;
            }
        } else {
            log.not_supported();
        }

        Ok(())
    }

    fn handle_remove(&mut self, core: &mut Core<'_>, request: RemoveRequest) -> anyhow::Result<()> {
        let path = request.path(core)?;

        tracing::warn!(
            "Target wanted to remove file {path}, \
            but probe-rs does not support this operation yet. Continuing..."
        );

        Ok(())
    }

    fn handle_rename(&mut self, core: &mut Core<'_>, request: RenameRequest) -> anyhow::Result<()> {
        let from_path = request.from_path(core)?;
        let to_path = request.to_path(core)?;

        tracing::warn!(
            "Target wanted to rename file {from_path} to {to_path}, \
            but probe-rs does not support this operation yet. Continuing..."
        );

        Ok(())
    }

    fn get_file_handle_entry(&mut self, handle: u32) -> Option<&mut Option<FileHandle>> {
        self.file_handles.get_mut((handle as usize).checked_sub(1)?)
    }

    fn take_file_handle(&mut self, handle: u32) -> Option<FileHandle> {
        self.get_file_handle_entry(handle)
            .and_then(|inner| inner.take())
    }

    fn get_file_handle(
        &mut self,
        action: &'static str,
        handle: u32,
    ) -> Option<(&mut FileHandle, FileHandleLog)> {
        let Some(Some(file_handle)) = self.get_file_handle_entry(handle) else {
            tracing::warn!("Target wanted to {action} invalid file handle {handle}. Continuing...");
            return None;
        };

        let variant = match file_handle {
            FileHandle::Stdout => "stdout",
            FileHandle::Stderr => "stderr",
            FileHandle::File(_) => "file",
            FileHandle::TcpStream(_) => "TCP stream",
            #[cfg(target_family = "unix")]
            FileHandle::UnixStream(_) => "UNIX stream",
        };

        Some((
            file_handle,
            FileHandleLog {
                action,
                handle,
                variant,
            },
        ))
    }

    fn trim_file_handles(&mut self) {
        while let Some(None) = self.file_handles.last() {
            self.file_handles.pop();
        }
    }
}

/// How to open a file for a semihosting mode.
struct OpenFlags {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
}

impl OpenFlags {
    fn from_mode(mode: &str) -> Option<Self> {
        let (read, write, append, truncate, create) = match mode {
            "r" | "rb" => (true, false, false, false, false),
            "r+" | "r+b" => (true, true, false, false, false),
            "w" | "wb" => (false, true, false, true, true),
            "w+" | "w+b" => (true, true, false, true, true),
            "a" | "ab" => (false, false, true, false, true),
            "a+" | "a+b" => (true, false, true, false, true),
            mode => {
                tracing::error!("Target wanted to open a file with invalid mode {mode}.");
                return None;
            }
        };

        Some(Self {
            read,
            write,
            append,
            truncate,
            create,
        })
    }
}

/// Opens a regular file beneath `root`.
fn open_beneath(root: &Path, path: &str, flags: OpenFlags) -> io::Result<File> {
    let path = resolve_beneath(root, path)?;

    let mut options = File::options();
    options
        .read(flags.read)
        .write(flags.write)
        .append(flags.append)
        .truncate(flags.truncate)
        .create(flags.create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // The open must not wait for the other end of a FIFO.
        options.custom_flags(libc::O_NONBLOCK);
    }

    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }

    Ok(file)
}

/// Returns the path of `path` beneath `root`. A symbolic link cannot lead out of `root`.
///
/// The open comes after this check, and `w` truncates at the open, so the check cannot use the open
/// file. Semihosting has no call that creates a link or renames a file, so the target cannot change
/// a link between the check and the open.
fn resolve_beneath(root: &Path, path: &str) -> io::Result<PathBuf> {
    let root = root.canonicalize()?;

    let mut resolved = root.clone();
    for component in Path::new(path).components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the path leaves the root",
            ));
        };
        resolved.push(name);

        match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                // This also refuses a dangling link, which an open could use to create a file
                // outside of the root.
                resolved = resolved.canonicalize()?;
                if !resolved.starts_with(&root) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "a symbolic link leaves the root",
                    ));
                }
            }
            Ok(_) => {}
            // A new file. The open fails if a parent directory is missing.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    Ok(resolved)
}

struct FileHandleLog {
    action: &'static str,
    handle: u32,
    variant: &'static str,
}

impl FileHandleLog {
    fn not_supported(&self) {
        self.warn("probe-rs does not support this operation")
    }

    fn inspect<T>(&self, result: std::io::Result<T>) -> Option<T> {
        result
            .inspect_err(|err| {
                self.warn(&format!("it failed with {err:?}"));
            })
            .ok()
    }

    fn warn(&self, reason: &str) {
        tracing::warn!(
            "Target wanted to {action} file handle {handle} ({variant}), \
            but {reason}. Continuing...",
            action = self.action,
            handle = self.handle,
            variant = self.variant,
        );
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const CONTENTS: &[u8] = b"0123456789";
    const APPENDED: &[u8] = b"0123456789ab";

    struct TestFile {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestFile {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("semihosting.txt");
            std::fs::write(&path, CONTENTS).unwrap();

            TestFile { _dir: dir, path }
        }

        fn open(&self, mode: &str) -> Option<File> {
            let manager = SemihostingFileManager::new(Mappings::default());
            match manager.open_file(self.path.to_str().unwrap(), mode)? {
                FileHandle::File(file) => Some(file),
                _ => panic!("open_file did not return a file"),
            }
        }

        fn contents(&self) -> Vec<u8> {
            std::fs::read(&self.path).unwrap()
        }
    }

    fn flags(mode: &str) -> OpenFlags {
        OpenFlags::from_mode(mode).unwrap()
    }

    /// Creates a directory with `root/` and `outside.txt` next to each other.
    #[cfg(unix)]
    fn root_with_outside_file() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, CONTENTS).unwrap();
        (dir, root, outside)
    }

    #[test]
    fn open_beneath_refuses_a_path_that_leaves_the_root() {
        let dir = tempfile::TempDir::new().unwrap();

        assert!(open_beneath(dir.path(), "../outside.txt", flags("w")).is_err());
        assert!(!dir.path().parent().unwrap().join("outside.txt").exists());
    }

    #[test]
    fn open_beneath_creates_and_reads_files() {
        let dir = tempfile::TempDir::new().unwrap();

        let mut file = open_beneath(dir.path(), "new.txt", flags("w")).unwrap();
        file.write_all(CONTENTS).unwrap();
        drop(file);

        let mut read_back = Vec::new();
        open_beneath(dir.path(), "new.txt", flags("r"))
            .unwrap()
            .read_to_end(&mut read_back)
            .unwrap();
        assert_eq!(read_back, CONTENTS);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_last_component_cannot_leave_the_root() {
        let (_dir, root, outside) = root_with_outside_file();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", flags("w")).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), CONTENTS);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_cannot_create_a_file_outside_the_root() {
        let (dir, root, _outside) = root_with_outside_file();
        let created = dir.path().join("created.txt");
        std::os::unix::fs::symlink(&created, root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", flags("w")).is_err());
        assert!(!created.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_a_parent_component_cannot_leave_the_root() {
        let (dir, root, _outside) = root_with_outside_file();
        std::os::unix::fs::symlink(dir.path(), root.join("link")).unwrap();

        assert!(open_beneath(&root, "link/outside.txt", flags("r")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_works() {
        let (_dir, root, _outside) = root_with_outside_file();
        std::fs::write(root.join("file.txt"), CONTENTS).unwrap();
        std::os::unix::fs::symlink("file.txt", root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", flags("r")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_does_not_block_the_open() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fifo");
        let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: `path` is a valid C string.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);

        assert!(open_beneath(dir.path(), "fifo", flags("r")).is_err());
    }

    #[test]
    fn handle_zero_is_invalid() {
        let mut manager = SemihostingFileManager::new(Mappings::default());

        assert!(manager.get_file_handle("read from", 0).is_none());
        assert!(manager.take_file_handle(0).is_none());
    }

    #[test]
    fn read_does_not_truncate() {
        let file = TestFile::new();
        let handle = file.open("r").expect("mode r failed to open the file");
        drop(handle);

        assert_eq!(file.contents(), CONTENTS);
    }

    #[test]
    fn read_update_does_not_truncate() {
        let file = TestFile::new();
        let mut handle = file.open("r+").expect("mode r+ failed to open the file");

        let mut read_back = Vec::new();
        handle.read_to_end(&mut read_back).unwrap();

        assert_eq!(read_back, CONTENTS);
        assert_eq!(file.contents(), CONTENTS);
    }

    #[test]
    fn write_truncates() {
        let file = TestFile::new();
        let handle = file.open("w").expect("mode w failed to open the file");
        drop(handle);

        assert_eq!(file.contents(), b"".as_slice());
    }

    #[test]
    fn append_writes_to_the_end() {
        let file = TestFile::new();
        let mut handle = file.open("a").expect("mode a failed to open the file");
        handle.write_all(b"ab").unwrap();
        drop(handle);

        assert_eq!(file.contents(), APPENDED);
    }

    #[test]
    fn append_update_writes_to_the_end() {
        let file = TestFile::new();
        let mut handle = file.open("a+").expect("mode a+ failed to open the file");
        handle.write_all(b"ab").unwrap();
        drop(handle);

        assert_eq!(file.contents(), APPENDED);
    }
}
