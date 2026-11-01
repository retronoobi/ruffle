//! Network access is permanently unavailable. This is not a configurable policy.
//! Local-file reads also reject SMB/UNC paths, mapped network drives and reparse
//! points so that file:// cannot silently turn into a network request.
use async_channel::{Receiver, Sender};
use ruffle_core::{
    backend::navigator::*,
    indexmap::IndexMap,
    loader::Error,
    socket::{ConnectionState, SocketAction, SocketHandle},
};
use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};
use url::{ParseError, Url};

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
}

pub fn local_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        if matches!(path.components().next(), Some(Component::Prefix(_))) {
            return Err("drive-relative paths are blocked".into());
        }
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    #[cfg(windows)]
    {
        use std::path::Prefix;
        let drive = match absolute.components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
                _ => return Err("network/device paths are blocked".into()),
            },
            _ => return Err("a local drive is required".into()),
        };
        let root = [drive as u16, 58, 92, 0];
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if !matches!(kind, 2 | 3 | 5 | 6) {
            return Err("network or unavailable drives are blocked".into());
        }
    }
    #[cfg(not(windows))]
    if absolute.to_string_lossy().starts_with("//") {
        return Err("network paths are blocked".into());
    }
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("invalid parent path".into());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut walked = PathBuf::new();
    let mut missing = false;
    for component in normalized.components() {
        walked.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        if missing {
            continue;
        }
        match std::fs::symlink_metadata(&walked) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("symbolic links are blocked".into());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(
                            "reparse points, including network redirects, are blocked".into()
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing = true,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(normalized)
}

pub struct OfflineNavigator {
    local: NullNavigatorBackend,
}
impl OfflineNavigator {
    pub fn new(path: &Path, executor: &NullExecutor) -> Result<Self, String> {
        let path = local_path(path)?;
        Ok(Self {
            local: NullNavigatorBackend::with_base_path(&path, executor)
                .map_err(|e| e.to_string())?,
        })
    }
    fn checked_url(&self, value: &str) -> Result<Url, String> {
        let raw = value.trim().replace(char::from(92), "/");
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("//")
            || lower
                .strip_prefix("file://")
                .is_some_and(|tail| !tail.starts_with('/'))
        {
            return Err("file URL authorities are blocked".into());
        }
        let url = self.local.resolve_url(value).map_err(|e| e.to_string())?;
        if url.scheme() != "file" || url.host_str().is_some() {
            return Err("all network access is blocked".into());
        }
        let path = url.to_file_path().map_err(|_| "invalid local file URL")?;
        let path = local_path(&path)?;
        Url::from_file_path(path).map_err(|_| "invalid local file path".into())
    }
}
impl NavigatorBackend for OfflineNavigator {
    fn navigate_to_url(
        &self,
        _: &str,
        _: &str,
        _: Option<(NavigationMethod, IndexMap<String, String>)>,
    ) {
        crate::log("Blocked external URL navigation (offline core)");
    }
    fn fetch(&self, mut request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        match self.checked_url(request.url()) {
            Ok(url) => {
                request.set_url(url.into());
                self.local.fetch(request)
            }
            Err(reason) => async_return(Err(create_specific_fetch_error(
                "Offline core",
                request.url(),
                reason,
            ))),
        }
    }
    fn resolve_url(&self, url: &str) -> Result<Url, ParseError> {
        self.checked_url(url)
            .map_err(|_| ParseError::RelativeUrlWithoutBase)
    }
    fn spawn_future(&mut self, future: OwnedFuture<(), Error>) {
        self.local.spawn_future(future);
    }
    fn pre_process_url(&self, url: Url) -> Url {
        url
    }
    fn connect_socket(
        &mut self,
        _: String,
        _: u16,
        _: Duration,
        handle: SocketHandle,
        _: Receiver<Vec<u8>>,
        sender: Sender<SocketAction>,
    ) {
        let _ = sender.try_send(SocketAction::Connect(handle, ConnectionState::Failed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_network_schemes_and_unc_without_contacting_hosts() {
        let executor = NullExecutor::new();
        let nav = OfflineNavigator::new(Path::new(env!("CARGO_MANIFEST_DIR")), &executor).unwrap();
        for url in [
            "https://example.invalid/a.swf",
            "http://127.0.0.1:1/a",
            "ftp://example.invalid/a",
            "file://example.invalid/share/a.swf",
            "//example.invalid/share/a.swf",
            "javascript:alert(1)",
            "file://localhost/C:/a.swf",
        ] {
            assert!(nav.checked_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn permits_regular_local_assets_and_rejects_missing_fetch() {
        let executor = NullExecutor::new();
        let nav = OfflineNavigator::new(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("Cargo.toml")
                .as_path(),
            &executor,
        )
        .unwrap();
        let url = nav.checked_url("Cargo.toml").unwrap();
        assert_eq!(url.scheme(), "file");
        assert!(url.host_str().is_none());
    }
    #[test]
    #[cfg(windows)]
    fn rejects_windows_remote_namespaces() {
        for path in [
            r"\\example.invalid\share\a.swf",
            r"\\?\UNC\example.invalid\share\a.swf",
            r"\\.\pipe\anything",
        ] {
            assert!(local_path(Path::new(path)).is_err());
        }
    }
    #[test]
    fn sockets_always_report_failure_and_closed_channels_do_not_panic() {
        let executor = NullExecutor::new();
        let mut nav =
            OfflineNavigator::new(Path::new(env!("CARGO_MANIFEST_DIR")), &executor).unwrap();
        let (_, receiver) = async_channel::unbounded();
        let (sender, result) = async_channel::unbounded();
        let handle = SocketHandle::default();
        nav.connect_socket(
            "example.invalid".into(),
            80,
            Duration::from_secs(1),
            handle,
            receiver,
            sender,
        );
        assert!(matches!(
            result.try_recv(),
            Ok(SocketAction::Connect(_, ConnectionState::Failed))
        ));
        let (_, receiver) = async_channel::unbounded();
        let (sender, result) = async_channel::unbounded();
        drop(result);
        nav.connect_socket(
            "127.0.0.1".into(),
            1,
            Duration::from_secs(1),
            handle,
            receiver,
            sender,
        );
    }
}
