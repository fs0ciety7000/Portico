//! Minimal SFTP v3 server over a temporary directory (test fixture).
//! Paths are absolute and rooted at `root`; `..` is refused.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Component, Path, PathBuf};

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};

enum Open {
    File(fs::File),
    Dir { entries: Vec<File>, sent: bool },
}

pub struct SftpServer {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
    /// Names a hostile server adds to every listing.
    extra_names: Vec<String>,
}

impl SftpServer {
    pub fn new(root: PathBuf, extra_names: Vec<String>) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            next: 0,
            extra_names,
        }
    }

    fn local(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let mut out = self.root.clone();
        for c in Path::new(path).components() {
            match c {
                Component::Normal(n) => out.push(n),
                Component::RootDir | Component::CurDir => {}
                _ => return Err(StatusCode::PermissionDenied),
            }
        }
        Ok(out)
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let h = format!("h{}", self.next);
        self.handles.insert(h.clone(), open);
        h
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

fn io(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl russh_sftp::server::Handler for SftpServer {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.local(&filename)?;
        let mut options = fs::OpenOptions::new();
        options
            .read(pflags.contains(OpenFlags::READ))
            .write(pflags.contains(OpenFlags::WRITE))
            .truncate(pflags.contains(OpenFlags::TRUNCATE));
        if pflags.contains(OpenFlags::EXCLUDE) {
            options.create_new(true);
        } else {
            options.create(pflags.contains(OpenFlags::CREATE));
        }
        if let Some(mode) = attrs.permissions {
            options.mode(mode & 0o7777);
        }
        let file = options.open(path).map_err(|e| io(&e))?;
        Ok(Handle {
            id,
            handle: self.handle(Open::File(file)),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(Open::File(f)) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let mut buf = vec![0; len as usize];
        let n = f.read_at(&mut buf, offset).map_err(|e| io(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let Some(Open::File(f)) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        f.write_all_at(&data, offset).map_err(|e| io(&e))?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = fs::symlink_metadata(self.local(&path)?).map_err(|e| io(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = fs::metadata(self.local(&path)?).map_err(|e| io(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(Open::File(f)) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let meta = f.metadata().map_err(|e| io(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = attrs.permissions {
            fs::set_permissions(
                self.local(&path)?,
                fs::Permissions::from_mode(mode & 0o7777),
            )
            .map_err(|e| io(&e))?;
        }
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let dir = self.local(&path)?;
        let mut entries = Vec::new();
        for e in fs::read_dir(dir).map_err(|e| io(&e))? {
            let e = e.map_err(|e| io(&e))?;
            let meta = fs::symlink_metadata(e.path()).map_err(|e| io(&e))?;
            entries.push(File::new(
                e.file_name().to_string_lossy().into_owned(),
                FileAttributes::from(&meta),
            ));
        }
        for name in &self.extra_names {
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(1);
            attrs.permissions = Some(0o100_644);
            entries.push(File::new(name.clone(), attrs));
        }
        Ok(Handle {
            id,
            handle: self.handle(Open::Dir {
                entries,
                sent: false,
            }),
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(Open::Dir { entries, sent }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *sent {
            return Err(StatusCode::Eof);
        }
        *sent = true;
        Ok(Name {
            id,
            files: std::mem::take(entries),
        })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        fs::remove_file(self.local(&filename)?).map_err(|e| io(&e))?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        fs::create_dir(self.local(&path)?).map_err(|e| io(&e))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        fs::remove_dir(self.local(&path)?).map_err(|e| io(&e))?;
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let normalized = if path == "." || path.is_empty() {
            "/".to_owned()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![File::dummy(normalized)],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let to = self.local(&newpath)?;
        if to.exists() {
            // SFTP v3 semantics (like OpenSSH): no implicit replace.
            return Err(StatusCode::Failure);
        }
        fs::rename(self.local(&oldpath)?, to).map_err(|e| io(&e))?;
        Ok(ok(id))
    }
}
