//! The host side of semihosting.
//!
//! The firmware and the server are not trusted. A call opens only what the mappings of the user
//! allow, and the checks here do not depend on the checks of the server.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    net::TcpStream,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use postcard_rpc::host_client::Subscription;
use probe_rs_rpc::HostReplyTopic;
use probe_rs_rpc::host_calls::{
    FileKind, HostCall, HostError, HostReply, HostRequest, HostValue, MAX_CHUNK, MAX_OPEN_FILES,
    MAX_PATH, OpenMode,
};
use tokio::sync::{Mutex, Notify, oneshot};

use crate::RpcClient;
use crate::semihosting_mappings::{HostTarget, MappingError, Mappings};
use crate::semihosting_options::SemihostingOptions;

/// How long a call waits for the file operation of the previous call to finish.
const PREVIOUS_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Gives the firmware of a target access to the files and sockets that the user mapped.
#[derive(Clone)]
pub struct HostFiles {
    mappings: Arc<Mappings>,
    allow_unverified_server: bool,
    /// Finishes when the file thread of the previous call ends.
    previous_call: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
}

impl HostFiles {
    /// Parses and checks the mappings.
    pub fn new(options: &SemihostingOptions) -> Result<Self, MappingError> {
        Ok(Self {
            mappings: Arc::new(Mappings::new(options)?),
            allow_unverified_server: false,
            previous_call: Arc::new(Mutex::new(None)),
        })
    }

    /// Fails if the directory of a prefix or regex mapping does not exist.
    pub fn check_roots(&self) -> Result<(), MappingError> {
        self.mappings.check_roots()
    }

    /// Allows host calls from a server that the client cannot verify.
    pub fn allow_unverified_server(mut self, allow: bool) -> Self {
        self.allow_unverified_server = allow;
        self
    }

    pub(crate) fn allows_unverified_server(&self) -> bool {
        self.allow_unverified_server
    }

    /// Answers host calls until `stop` fires. A new thread does the file operations, so that a stuck
    /// operation cannot stop the client or its exit.
    pub(crate) async fn serve(
        &self,
        client: &RpcClient,
        calls: &mut Subscription<HostCall>,
        stop: Arc<Notify>,
    ) {
        let (requests, done) = start_file_thread(self.mappings.clone());

        let mut previous_call = self.previous_call.lock().await;
        // A late write of the previous call must not mix with the calls of this one.
        if let Some(previous) = previous_call.take()
            && tokio::time::timeout(PREVIOUS_CALL_TIMEOUT, previous)
                .await
                .is_err()
        {
            tracing::warn!("A semihosting file operation of the previous call is stuck.");
        }
        *previous_call = Some(done);
        drop(previous_call);

        let answer_calls = async {
            // One call at a time, so that a server that ignores the window cannot start more work.
            while let Some(call) = calls.recv().await {
                let reply = HostReply {
                    id: call.id,
                    result: run_on_thread(&requests, call.request).await,
                };
                if client.publish::<HostReplyTopic>(&reply).await.is_err() {
                    break;
                }
            }
        };

        tokio::select! {
            _ = answer_calls => {}
            _ = stop.notified() => {}
        }
    }
}

type FileRequest = (HostRequest, oneshot::Sender<Result<HostValue, HostError>>);

fn start_file_thread(
    mappings: Arc<Mappings>,
) -> (std::sync::mpsc::Sender<FileRequest>, oneshot::Receiver<()>) {
    let (requests, receiver) = std::sync::mpsc::channel::<FileRequest>();
    let (done_sender, done) = oneshot::channel();

    let thread = std::thread::Builder::new()
        .name("semihosting-files".to_owned())
        .spawn(move || {
            let mut table = HandleTable::new(mappings);
            while let Ok((request, reply)) = receiver.recv() {
                _ = reply.send(table.run(request));
            }
            drop(table);
            _ = done_sender.send(());
        });
    if let Err(error) = thread {
        tracing::error!("Failed to start the semihosting file thread: {error}");
    }

    (requests, done)
}

async fn run_on_thread(
    requests: &std::sync::mpsc::Sender<FileRequest>,
    request: HostRequest,
) -> Result<HostValue, HostError> {
    let (reply, result) = oneshot::channel();
    if requests.send((request, reply)).is_err() {
        return Err(HostError::Other);
    }
    result.await.unwrap_or(Err(HostError::Other))
}

enum Handle {
    File(File),
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Handle {
    fn stream(&mut self) -> &mut dyn ReadWrite {
        match self {
            Self::File(file) => file,
            Self::Tcp(stream) => stream,
            #[cfg(unix)]
            Self::Unix(stream) => stream,
        }
    }
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// The files and sockets of one monitor or test call.
struct HandleTable {
    mappings: Arc<Mappings>,
    handles: HashMap<u32, Handle>,
    next_handle: u32,
}

impl HandleTable {
    fn new(mappings: Arc<Mappings>) -> Self {
        Self {
            mappings,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    fn run(&mut self, request: HostRequest) -> Result<HostValue, HostError> {
        match request {
            HostRequest::Open { path, mode } => self.open(&path, mode),
            HostRequest::Close { handle } => {
                self.handles.remove(&handle).ok_or(HostError::BadHandle)?;
                Ok(HostValue::Done)
            }
            HostRequest::Read { handle, len } => {
                if len > MAX_CHUNK {
                    return Err(HostError::InvalidInput);
                }
                let mut buf = vec![0; len as usize];
                let read = self.handle(handle)?.stream().read(&mut buf)?;
                buf.truncate(read);
                Ok(HostValue::Read(buf))
            }
            HostRequest::Write { handle, bytes } => {
                if bytes.len() > MAX_CHUNK as usize {
                    return Err(HostError::InvalidInput);
                }
                let written = self.handle(handle)?.stream().write(&bytes)?;
                Ok(HostValue::Written(written as u32))
            }
            HostRequest::Seek { handle, pos } => {
                let Handle::File(file) = self.handle(handle)? else {
                    return Err(HostError::InvalidInput);
                };
                file.seek(SeekFrom::Start(pos as u64))?;
                Ok(HostValue::Done)
            }
            HostRequest::FileLength { handle } => {
                let Handle::File(file) = self.handle(handle)? else {
                    return Err(HostError::InvalidInput);
                };
                Ok(HostValue::Length(file.metadata()?.len()))
            }
        }
    }

    fn handle(&mut self, handle: u32) -> Result<&mut Handle, HostError> {
        self.handles.get_mut(&handle).ok_or(HostError::BadHandle)
    }

    fn open(&mut self, path: &str, mode: OpenMode) -> Result<HostValue, HostError> {
        if path.len() > MAX_PATH as usize {
            return Err(HostError::InvalidInput);
        }
        if self.handles.len() >= MAX_OPEN_FILES {
            return Err(HostError::TooManyFiles);
        }

        let Some(target) = self.mappings.resolve(path) else {
            tracing::warn!("The target wanted to open {path:?}, which no mapping allows.");
            return Err(HostError::NotMapped);
        };

        let handle = open_target(target, mode).map_err(|error| {
            tracing::warn!("The target wanted to open {path:?}, but it failed: {error}");
            HostError::from(error)
        })?;
        let kind = match handle {
            Handle::File(_) => FileKind::File,
            _ => FileKind::Stream,
        };

        let id = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(id, handle);
        Ok(HostValue::Opened { handle: id, kind })
    }
}

fn open_target(target: HostTarget, mode: OpenMode) -> io::Result<Handle> {
    Ok(match target {
        HostTarget::File(path) => Handle::File(open_options(mode).open(path)?),
        HostTarget::Beneath { root, path } => Handle::File(open_beneath(&root, &path, mode)?),
        HostTarget::Tcp(address) => Handle::Tcp(TcpStream::connect(address)?),
        #[cfg(unix)]
        HostTarget::Unix(path) => Handle::Unix(std::os::unix::net::UnixStream::connect(path)?),
        #[cfg(not(unix))]
        HostTarget::Unix(_) => return Err(io::Error::from(io::ErrorKind::Unsupported)),
    })
}

fn open_options(mode: OpenMode) -> std::fs::OpenOptions {
    let mut options = File::options();
    match mode {
        OpenMode::Read => options.read(true),
        OpenMode::ReadUpdate => options.read(true).write(true),
        OpenMode::Write => options.write(true).truncate(true).create(true),
        OpenMode::WriteUpdate => options.read(true).write(true).truncate(true).create(true),
        OpenMode::Append => options.append(true).create(true),
        OpenMode::AppendUpdate => options.read(true).append(true).create(true),
    };
    options
}

/// Opens a regular file beneath `root`.
fn open_beneath(root: &Path, path: &str, mode: OpenMode) -> io::Result<File> {
    let path = resolve_beneath(root, path)?;

    let file = open_options(mode).open(path)?;
    // `resolve_beneath` checked the path, but another process can replace the file before the open.
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }

    Ok(file)
}

/// Returns the path of `path` beneath `root`. A symbolic link cannot lead out of `root`.
///
/// The open comes after this check, and `w` truncates at the open, so the check cannot use the open
/// file. Semihosting has no call that creates a link or renames a file, so the target and the server
/// cannot change a link between the check and the open.
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

    // Opening a device can have an effect before the open file can be checked. For example, a
    // serial port can reset a board.
    match std::fs::metadata(&resolved) {
        Ok(metadata) if !metadata.is_file() => Err(io::Error::other("not a regular file")),
        Ok(_) => Ok(resolved),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const CONTENTS: &[u8] = b"0123456789";

    fn table_for(root: &Path) -> HandleTable {
        let mut options = SemihostingOptions::new();
        options
            .add_file_prefix("in/".to_owned(), format!("{}/", root.display()))
            .unwrap();
        HandleTable::new(Arc::new(Mappings::new(&options).unwrap()))
    }

    fn open(table: &mut HandleTable, path: &str, mode: OpenMode) -> Result<u32, HostError> {
        match table.run(HostRequest::Open {
            path: path.to_owned(),
            mode,
        })? {
            HostValue::Opened { handle, .. } => Ok(handle),
            _ => panic!("open returned another value"),
        }
    }

    #[test]
    fn a_file_works_through_requests() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut table = table_for(dir.path());

        let handle = open(&mut table, "in/file.txt", OpenMode::WriteUpdate).unwrap();
        assert_eq!(
            table.run(HostRequest::Write {
                handle,
                bytes: CONTENTS.to_vec()
            }),
            Ok(HostValue::Written(10))
        );
        assert_eq!(
            table.run(HostRequest::FileLength { handle }),
            Ok(HostValue::Length(10))
        );
        assert_eq!(
            table.run(HostRequest::Seek { handle, pos: 8 }),
            Ok(HostValue::Done)
        );
        assert_eq!(
            table.run(HostRequest::Read { handle, len: 100 }),
            Ok(HostValue::Read(b"89".to_vec()))
        );
        assert_eq!(
            table.run(HostRequest::Close { handle }),
            Ok(HostValue::Done)
        );
        assert_eq!(
            table.run(HostRequest::Close { handle }),
            Err(HostError::BadHandle)
        );
    }

    #[test]
    fn an_unmapped_path_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut table = table_for(dir.path());

        for path in ["other.txt", "in/../other.txt", "tcp:localhost:1"] {
            assert_eq!(
                open(&mut table, path, OpenMode::Read),
                Err(HostError::NotMapped),
                "{path}"
            );
        }
    }

    #[test]
    fn a_hostile_server_gets_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut table = table_for(dir.path());

        assert_eq!(
            table.run(HostRequest::Read {
                handle: 1,
                len: MAX_CHUNK + 1
            }),
            Err(HostError::InvalidInput)
        );
        assert_eq!(
            table.run(HostRequest::Read { handle: 1, len: 1 }),
            Err(HostError::BadHandle)
        );
        assert_eq!(
            table.run(HostRequest::Write {
                handle: 1,
                bytes: vec![0; MAX_CHUNK as usize + 1]
            }),
            Err(HostError::InvalidInput)
        );
        assert_eq!(
            open(
                &mut table,
                &format!("in/{}", "a".repeat(MAX_PATH as usize)),
                OpenMode::Read
            ),
            Err(HostError::InvalidInput)
        );
    }

    #[test]
    fn the_open_files_are_limited() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut table = table_for(dir.path());

        for _ in 0..MAX_OPEN_FILES {
            open(&mut table, "in/file.txt", OpenMode::Append).unwrap();
        }
        assert_eq!(
            open(&mut table, "in/file.txt", OpenMode::Append),
            Err(HostError::TooManyFiles)
        );
    }

    #[test]
    fn a_closed_handle_is_not_reused() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut table = table_for(dir.path());

        let first = open(&mut table, "in/file.txt", OpenMode::Append).unwrap();
        table.run(HostRequest::Close { handle: first }).unwrap();
        let second = open(&mut table, "in/file.txt", OpenMode::Append).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn modes_follow_the_c_library() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        let write = |mode| {
            let mut file = open_beneath(dir.path(), "file.txt", mode).unwrap();
            file.write_all(b"ab").unwrap();
        };

        std::fs::write(&path, CONTENTS).unwrap();
        assert!(open_beneath(dir.path(), "file.txt", OpenMode::Read).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), CONTENTS);

        write(OpenMode::ReadUpdate);
        assert_eq!(std::fs::read(&path).unwrap(), b"ab23456789");

        write(OpenMode::Append);
        assert_eq!(std::fs::read(&path).unwrap(), b"ab23456789ab");

        write(OpenMode::Write);
        assert_eq!(std::fs::read(&path).unwrap(), b"ab");
    }

    #[test]
    fn open_beneath_refuses_a_path_that_leaves_the_root() {
        let dir = tempfile::TempDir::new().unwrap();

        assert!(open_beneath(dir.path(), "../outside.txt", OpenMode::Write).is_err());
        assert!(!dir.path().parent().unwrap().join("outside.txt").exists());
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_last_component_cannot_leave_the_root() {
        let (_dir, root, outside) = root_with_outside_file();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", OpenMode::Write).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), CONTENTS);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_cannot_create_a_file_outside_the_root() {
        let (dir, root, _outside) = root_with_outside_file();
        let created = dir.path().join("created.txt");
        std::os::unix::fs::symlink(&created, root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", OpenMode::Write).is_err());
        assert!(!created.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_a_parent_component_cannot_leave_the_root() {
        let (dir, root, _outside) = root_with_outside_file();
        std::os::unix::fs::symlink(dir.path(), root.join("link")).unwrap();

        assert!(open_beneath(&root, "link/outside.txt", OpenMode::Read).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_root_works() {
        let (_dir, root, _outside) = root_with_outside_file();
        std::fs::write(root.join("file.txt"), CONTENTS).unwrap();
        std::os::unix::fs::symlink("file.txt", root.join("link")).unwrap();

        assert!(open_beneath(&root, "link", OpenMode::Read).is_ok());
    }

    #[test]
    fn open_beneath_refuses_a_directory_before_the_open() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        assert!(resolve_beneath(dir.path(), "sub").is_err());
    }
}
