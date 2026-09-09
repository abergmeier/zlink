use core::future::Future;

use crate::{Connection, Result, connection::Socket};

/// A listener is a server that listens for incoming connections.
pub trait Listener: core::fmt::Debug {
    /// The type of the socket the connections this listener creates will use.
    type Socket: Socket;

    /// Accept a new connection.
    ///
    /// Returns `Ok(None)` to signal that no more connections will be produced. Once `Ok(None)`
    /// has been returned, subsequent calls must pend forever — the listener is considered closed.
    fn accept(&mut self) -> impl Future<Output = Result<Option<Connection<Self::Socket>>>>;

    /// Set an extended attribute on the socket inode this listener is bound to.
    ///
    /// Varlink tooling can read attributes off a service's entrypoint socket without connecting
    /// to it. Listeners bound to a Unix socket in the file system already carry `user.varlink`
    /// set to `entrypoint`; this lets a service publish additional attributes of its own, for
    /// example the `user.userdb.*` attributes systemd's user database consults to skip providers
    /// that cannot answer a query. Attribute names normally live in the `user.` namespace.
    /// Extended attributes on sockets are a Linux feature, so this method only exists there.
    ///
    /// # Errors
    ///
    /// Fails with [`std::io::ErrorKind::Unsupported`] if the listener is not backed by a socket
    /// inode in the file system (the default implementation) or if the kernel does not support
    /// extended attributes on socket inodes (Linux 7.1 or newer is required), with
    /// [`std::io::ErrorKind::InvalidInput`] if the entrypoint inode is not known because the
    /// listener was adopted from a file descriptor or bound to a relative path, and otherwise with
    /// whatever error setting the attribute produced.
    #[cfg(all(feature = "std", target_os = "linux"))]
    fn set_xattr(&self, _name: &str, _value: impl AsRef<[u8]>) -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "listener is not backed by a socket inode in the file system",
        )
        .into())
    }
}

/// A listener that already has a socket.
///
/// This is useful for services spawned by their supervisor with a connected socket already in
/// hand (systemd `Accept=yes`, `varlinkctl exec:`, etc.). [`Server::run`] exits cleanly once the
/// handed-out connection closes.
///
/// [`Server::run`]: crate::Server::run
#[derive(Debug)]
pub struct ReadyListener<Sock: Socket> {
    state: ReadyListenerState<Sock>,
}

#[derive(Debug)]
enum ReadyListenerState<Sock: Socket> {
    Ready(Sock),
    Handed,
    Closed,
}

impl<Sock> ReadyListener<Sock>
where
    Sock: Socket,
{
    /// Create a new listener from a socket.
    pub fn new(socket: Sock) -> Self {
        Self {
            state: ReadyListenerState::Ready(socket),
        }
    }
}

impl<Sock> Listener for ReadyListener<Sock>
where
    Sock: Socket,
{
    type Socket = Sock;

    /// Returns the contained socket on the first call, then reports closure.
    async fn accept(&mut self) -> Result<Option<Connection<Self::Socket>>> {
        match core::mem::replace(&mut self.state, ReadyListenerState::Handed) {
            ReadyListenerState::Ready(socket) => Ok(Some(Connection::new(socket))),
            ReadyListenerState::Handed => {
                self.state = ReadyListenerState::Closed;

                Ok(None)
            }
            ReadyListenerState::Closed => {
                self.state = ReadyListenerState::Closed;

                core::future::pending().await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{future::poll_fn, task::Poll};

    use super::*;
    use crate::test_utils::mock_socket::MockSocket;

    #[tokio::test]
    async fn ready_listener() {
        let socket = MockSocket::with_responses(&["test"]);
        let mut listener = ReadyListener::new(socket);

        // First call returns a connection with properly split read/write halves.
        let conn = listener.accept().await.unwrap().unwrap();
        let (read, write) = conn.split();
        assert_eq!(read.id(), write.id());

        // Second call reports closure.
        assert!(matches!(listener.accept().await, Ok(None)));

        // Subsequent calls pend forever.
        let accept_fut = listener.accept();
        futures_util::pin_mut!(accept_fut);
        let is_pending = poll_fn(|cx| Poll::Ready(accept_fut.as_mut().poll(cx).is_pending())).await;
        assert!(is_pending);
    }
}
