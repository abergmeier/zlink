//! Integration tests for the `user.varlink` tagging of Unix sockets.
//!
//! Kernels older than Linux 7.1 refuse extended attributes on sockets, so every test asserts the
//! tags when the kernel supports them and their absence (with tagging staying out of the way)
//! when it does not.

#![cfg(all(feature = "server", target_os = "linux"))]

use std::os::{
    fd::{AsFd, OwnedFd},
    unix::net::UnixListener as StdUnixListener,
};

use tempfile::TempDir;
use zlink::{
    Listener as _,
    unix_utils::{read_fxattr, read_lxattr, socket_xattr_supported},
};

#[cfg(all(feature = "smol", not(feature = "tokio")))]
use zlink::smol::unix;
#[cfg(feature = "tokio")]
use zlink::tokio::unix;

#[tokio::test]
async fn sockets_carry_varlink_role() {
    let temp_dir = TempDir::new().unwrap();
    let socket_path = temp_dir.path().join("roles.sock");

    let mut listener = unix::bind(&socket_path).unwrap();
    // A Unix socket connect completes as soon as it is queued in the listener's backlog.
    let client = unix::connect(&socket_path).await.unwrap();
    let accepted = listener.accept().await.unwrap().unwrap();

    let listener_value = read_fxattr(&listener, VARLINK_XATTR);
    let path_value = read_lxattr(&socket_path, VARLINK_XATTR);
    let (client_read, _) = client.split();
    let client_value = read_fxattr(client_read.read_half().as_fd(), VARLINK_XATTR);
    let (accepted_read, _) = accepted.split();
    let accepted_value = read_fxattr(accepted_read.read_half().as_fd(), VARLINK_XATTR);

    if socket_xattr_supported() {
        assert_eq!(listener_value.unwrap(), b"listen");
        assert_eq!(path_value.unwrap(), b"entrypoint");
        assert_eq!(client_value.unwrap(), b"client");
        assert_eq!(accepted_value.unwrap(), b"server");
    } else {
        assert!(listener_value.is_err());
        assert!(path_value.is_err());
        assert!(client_value.is_err());
        assert!(accepted_value.is_err());
    }
}

#[tokio::test]
async fn adopted_listener_tags_only_the_socket() {
    let temp_dir = TempDir::new().unwrap();
    let socket_path = temp_dir.path().join("adopted.sock");

    let fd: OwnedFd = StdUnixListener::bind(&socket_path).unwrap().into();
    let listener = unix::Listener::try_from(fd).unwrap();

    let listener_value = read_fxattr(&listener, VARLINK_XATTR);
    if socket_xattr_supported() {
        assert_eq!(listener_value.unwrap(), b"listen");
    } else {
        assert!(listener_value.is_err());
    }
    // The path the socket was bound to is not known, so the inode is left alone.
    assert!(read_lxattr(&socket_path, VARLINK_XATTR).is_err());
}

const VARLINK_XATTR: &str = "user.varlink";
