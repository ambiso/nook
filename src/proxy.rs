use crate::config::{Result, error};
use std::{
    fs,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    thread,
};

// Connecting from this separate owner-UID process ensures that SO_PEERCRED at
// the host Nix daemon identifies the real owner, rather than a shifted guest
// UID. The two directions close independently so protocol EOF is preserved.
pub fn nix_proxy(socket: &str, target: &str) -> Result<i32> {
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    for connection in listener.incoming() {
        let mut incoming = connection?;
        let mut outgoing = UnixStream::connect(target)?;
        let mut input = incoming.try_clone()?;
        let mut output = outgoing.try_clone()?;
        thread::spawn(move || {
            let first = thread::spawn(move || {
                let _ = std::io::copy(&mut input, &mut outgoing);
                let _ = outgoing.shutdown(std::net::Shutdown::Write);
            });
            let _ = std::io::copy(&mut output, &mut incoming);
            let _ = incoming.shutdown(std::net::Shutdown::Write);
            let _ = first.join();
        });
    }
    Err(error("Nix proxy listener closed").into())
}
