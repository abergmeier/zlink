use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::Path,
};

#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::unix_utils::{self, SocketRole};
use crate::{Connection, Result};

/// Create a new unix domain socket listener and bind it to `path`.
///
/// On Linux the listener socket is tagged with a `user.varlink` extended attribute set to `listen`
/// and, if `path` is absolute, the socket inode created there with one set to `entrypoint`, so that
/// tools like `varlinkctl list-sockets` recognise them as Varlink sockets. A relative `path` is not
/// used for that, since it would be resolved against whatever the working directory is at the
/// time. Tagging is best effort; kernels without support (older than Linux 7.1) are silently
/// tolerated.
pub fn bind<P>(path: P) -> Result<Listener>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let listener = tokio::net::UnixListener::bind(path)?;

    Ok(Listener::new(
        listener,
        #[cfg(target_os = "linux")]
        Some(path.to_owned()),
    ))
}

/// A unix domain socket listener.
///
/// On Linux, listeners are tagged `listen`, the socket inode [`bind`] creates at an absolute path
/// `entrypoint` and sockets returned by [`crate::Listener::accept`] `server`. Listeners adopted
/// from a file descriptor or bound to a relative path only get the `listen` tag, since the inode
/// they were bound to cannot be located safely. Tagging is best effort; kernels without support
/// (older than Linux 7.1) are silently tolerated.
#[derive(Debug)]
pub struct Listener {
    listener: tokio::net::UnixListener,
    /// The path [`bind`] was given, if this listener came from it.
    #[cfg(target_os = "linux")]
    path: Option<PathBuf>,
}

impl Listener {
    /// Wrap a bound listener, tagging it and, when `path` is known, its entrypoint inode.
    fn new(
        listener: tokio::net::UnixListener,
        #[cfg(target_os = "linux")] path: Option<PathBuf>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        unix_utils::tag_listener(&listener, path.as_deref());

        Self {
            listener,
            #[cfg(target_os = "linux")]
            path,
        }
    }
}

impl crate::Listener for Listener {
    type Socket = super::Stream;

    async fn accept(&mut self) -> Result<Option<Connection<Self::Socket>>> {
        let (stream, _) = self.listener.accept().await?;
        #[cfg(target_os = "linux")]
        unix_utils::tag_socket(&stream, SocketRole::Server);

        Ok(Some(super::Stream::try_from(stream)?.into()))
    }

    #[cfg(target_os = "linux")]
    fn set_xattr(&self, name: &str, value: impl AsRef<[u8]>) -> Result<()> {
        let Some(path) = &self.path else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the entrypoint inode of a listener adopted from a file descriptor is not known",
            )
            .into());
        };

        unix_utils::set_entrypoint_xattr(path, name, value.as_ref()).map_err(Into::into)
    }
}

impl From<tokio::net::UnixListener> for Listener {
    fn from(listener: tokio::net::UnixListener) -> Self {
        Listener::new(
            listener,
            #[cfg(target_os = "linux")]
            None,
        )
    }
}

impl TryFrom<OwnedFd> for Listener {
    type Error = crate::Error;

    fn try_from(fd: OwnedFd) -> Result<Self> {
        let std_listener = std::os::unix::net::UnixListener::from(fd);
        std_listener.set_nonblocking(true)?;

        let listener = tokio::net::UnixListener::from_std(std_listener)?;

        Ok(Listener::new(
            listener,
            #[cfg(target_os = "linux")]
            None,
        ))
    }
}

impl AsFd for Listener {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Listener as _;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use tempfile::TempDir;

    #[tokio::test]
    async fn from_fd_with_multiple_connections() {
        // Create a temporary directory for the socket
        let temp_dir = TempDir::new().unwrap();
        let socket_path = temp_dir.path().join("test3.sock");

        // Create a standard Unix listener and convert to OwnedFd
        let std_listener = StdUnixListener::bind(&socket_path).unwrap();
        let fd: OwnedFd = std_listener.into();

        // Create our listener from the fd
        let mut listener = Listener::try_from(fd).unwrap();

        // Connect and accept multiple times to verify the listener remains functional
        let mut previous_id = None;
        for _i in 0..3 {
            let socket_path_clone = socket_path.clone();
            let connect_task = tokio::spawn(async move {
                tokio::net::UnixStream::connect(&socket_path_clone)
                    .await
                    .unwrap()
            });

            let connection = listener.accept().await.unwrap().unwrap();
            let id = connection.id();

            // Each connection should have a unique ID
            if let Some(prev) = previous_id {
                assert_ne!(id, prev, "Connection IDs should be unique");
            }
            previous_id = Some(id);

            let _stream = connect_task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn from_fd_preserves_nonblocking() {
        // Create a temporary directory for the socket
        let temp_dir = TempDir::new().unwrap();
        let socket_path = temp_dir.path().join("test4.sock");

        // Create a standard Unix listener in blocking mode
        let std_listener = StdUnixListener::bind(&socket_path).unwrap();
        // Explicitly set to blocking (though it's the default)
        std_listener.set_nonblocking(false).unwrap();

        let fd: OwnedFd = std_listener.into();

        // The from_fd should set it to non-blocking
        let mut listener = Listener::try_from(fd).unwrap();

        // Should still work with tokio's async runtime
        let socket_path_clone = socket_path.clone();
        let connect_task = tokio::spawn(async move {
            tokio::net::UnixStream::connect(&socket_path_clone)
                .await
                .unwrap()
        });

        let connection = listener.accept().await.unwrap().unwrap();
        // Just verify we got a valid connection with an ID
        let id = connection.id();
        assert!(id < usize::MAX);

        let _stream = connect_task.await.unwrap();
    }
}
