use crate::config::{Mount, Profile, Result, error, safe_mount};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixDatagram, UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    fcntl::{AtFlags, FcntlArg, FdFlag, OFlag, fcntl, openat},
    sys::{
        signal::{Signal, kill},
        stat::{Mode, SFlag, mkdirat, mknod},
    },
    unistd::{
        AccessFlags, Gid, Pid, Uid, chown, faccessat, geteuid, initgroups, setegid, seteuid,
        setgroups,
    },
};
use std::{os::fd::AsFd, sync::Arc};

// Credential changes are confined to the single-threaded supervisor. Socket
// forwarding runs in a separate unprivileged process, so another thread can
// never perform filesystem operations while these credentials are changed.
fn as_user<T>(profile: &Profile, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let name = CString::new(profile.get("username")?)?;
    initgroups(&name, Gid::from_raw(profile.gid()?))?;
    setegid(Gid::from_raw(profile.gid()?))?;
    seteuid(Uid::from_raw(profile.uid()?))?;
    let result = operation();
    seteuid(Uid::from_raw(0))?;
    setegid(Gid::from_raw(0))?;
    setgroups(&[])?;
    result
}
fn pin(profile: &Profile, path: &Path, writable: bool) -> Result<File> {
    as_user(profile, || {
        // Pin the inode with the owner's credentials before any privileged
        // mount. No later mount follows a user-controlled source symlink.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(OFlag::O_PATH.bits())
            .open(path)?;
        let access = if writable {
            AccessFlags::R_OK | AccessFlags::W_OK
        } else {
            AccessFlags::R_OK
        };
        faccessat(
            &file,
            "",
            access,
            AtFlags::AT_EMPTY_PATH | AtFlags::AT_EACCESS,
        )?;
        Ok(file)
    })
}
fn own(path: &Path, uid: u32, gid: u32) -> Result<()> {
    chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
    Ok(())
}
// A child must receive the pinned descriptor, rather than resolving the
// original path after the owner has had an opportunity to replace it.
fn inherit(fd: impl AsFd) -> Result<()> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::empty()))?;
    Ok(())
}
fn command_ok(c: &mut Command) -> Result<()> {
    if c.status()?.success() {
        Ok(())
    } else {
        Err(format!("command failed: {c:?}").into())
    }
}
struct Helpers {
    children: Vec<Child>,
    latches: Vec<UnixStream>,
    cleanup: Vec<(u32, u32, PathBuf)>,
}
impl Drop for Helpers {
    fn drop(&mut self) {
        self.latches.clear();
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        for (uid, gid, path) in &self.cleanup {
            // These paths live in the owner's runtime directory. Clean them
            // with that owner's credentials, never with host root privileges.
            let _ = Command::new("setpriv")
                .args([
                    "--reuid",
                    &uid.to_string(),
                    "--regid",
                    &gid.to_string(),
                    "--clear-groups",
                    "--",
                    "rm",
                    "-rf",
                    "--",
                ])
                .arg(path)
                .status();
        }
    }
}
fn user_command(profile: &Profile, exe: &str) -> Result<Command> {
    let mut c = Command::new("setpriv");
    c.args([
        "--reuid",
        profile.get("uid")?,
        "--regid",
        profile.get("gid")?,
        "--init-groups",
        "--",
        exe,
    ]);
    Ok(c)
}
fn proxy(
    profile: &Profile,
    directory: &Path,
    system: bool,
    helpers: &mut Helpers,
    portal_metadata: Option<(&Path, &Path)>,
    rules_override: Option<&[String]>,
) -> Result<PathBuf> {
    let socket = directory.join(if system { "system-bus" } else { "session-bus" });
    // A previous lifetime can only leave a stale socket in this user-owned
    // runtime directory. The service never follows it as a bind source.
    as_user(profile, || {
        if socket.symlink_metadata().is_ok() {
            fs::remove_file(&socket)?;
        }
        Ok(())
    })?;
    let (mut reader, writer) = UnixStream::pair()?;
    inherit(&writer)?;
    let mut cmd = user_command(
        profile,
        if portal_metadata.is_some() {
            "bwrap"
        } else {
            "xdg-dbus-proxy"
        },
    )?;
    let info_file = if let Some((metadata, info)) = portal_metadata {
        let file = as_user(profile, || Ok(File::create(info)?))?;
        inherit(&file)?;
        // Only the portal proxy advertises sandbox metadata. Actual GUI
        // processes keep their upstream Wayland/desktop identity for GNOME.
        // Only /nix/store has the same contents in host and guest. Mount the
        // proxy's sockets under private names: exposing host /home, /tmp or
        // /run at their usual paths makes the portal wrongly skip document
        // export for files that the guest cannot actually reach.
        cmd.args(["--ro-bind", "/nix/store", "/nix/store", "--ro-bind"])
            .arg(format!("/run/user/{}/bus", profile.uid()?))
            .arg("/host-bus")
            .arg("--bind")
            .arg(directory)
            .arg("/proxy")
            .arg("--ro-bind")
            .arg(metadata)
            .arg("/.flatpak-info")
            .arg("--info-fd")
            .arg(file.as_raw_fd().to_string())
            .arg("--")
            .arg("xdg-dbus-proxy");
        Some(file)
    } else {
        None
    };
    let upstream = if portal_metadata.is_some() {
        "unix:path=/host-bus".into()
    } else if system {
        "unix:path=/run/dbus/system_bus_socket".into()
    } else {
        format!("unix:path=/run/user/{}/bus", profile.uid()?)
    };
    cmd.arg(upstream)
        .arg(if portal_metadata.is_some() {
            Path::new("/proxy/session-bus")
        } else {
            &socket
        })
        .arg("--filter")
        .arg(format!("--fd={}", writer.as_raw_fd()));
    cmd.args(rules_override.unwrap_or(if system {
        &profile.system_rules
    } else {
        &profile.session_rules
    }));
    helpers.children.push(cmd.spawn()?);
    drop(info_file);
    drop(writer);
    reader.set_read_timeout(Some(Duration::from_secs(10)))?;
    reader.read_exact(&mut [0])?;
    helpers.latches.push(reader);
    Ok(socket)
}

fn prepare_portals(
    profile: &Profile,
    instance: &str,
    runtime: &Path,
    uid_offset: u32,
    helpers: &mut Helpers,
) -> Result<(PathBuf, PathBuf)> {
    let machine = profile.machine(instance)?;
    let app_id = format!(
        "{}.{}",
        profile.get("portalAppId")?,
        instance.replace('-', "_")
    );
    let metadata_directory =
        PathBuf::from(format!("/run/user/{}/.flatpak/{machine}", profile.uid()?));
    let metadata = metadata_directory.join("info");
    as_user(profile, || {
        fs::create_dir_all(&metadata_directory)?;
        fs::write(
            &metadata,
            format!(
                "[Application]\nname={app_id}\n[Instance]\ninstance-id={machine}\n[Context]\nshared={}\n",
                if profile.flag("network") {
                    "network;"
                } else {
                    ""
                }
            ),
        )?;
        Ok(())
    })?;
    helpers
        .cleanup
        .push((profile.uid()?, profile.gid()?, metadata_directory.clone()));

    let user_runtime = profile.runtime(instance)?;
    let socket = proxy(
        profile,
        &user_runtime,
        false,
        helpers,
        Some((&metadata, &metadata_directory.join("bwrapinfo.json"))),
        None,
    )?;

    let mut query = user_command(
        profile,
        std::env::current_exe()?
            .to_str()
            .ok_or("invalid executable")?,
    )?;
    let output = query
        .arg("--document-mountpoint")
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path=/run/user/{}/bus", profile.uid()?),
        )
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", profile.uid()?))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "document portal startup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let source = PathBuf::from(String::from_utf8(output.stdout)?.trim())
        .join("by-app")
        .join(app_id);
    let destination = runtime.join("documents");
    fs::create_dir_all(&destination)?;
    let fuse = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")?;
    inherit(&fuse)?;
    // FUSE portal mounts normally accept only the host owner's credentials
    // and cannot be assumed to support ID-mapped mounts. Bindfs reads only
    // this instance's granted-document view as that owner, while presenting
    // the shifted guest UID. The root supervisor supplies the mount/FD, so no
    // host-wide user_allow_other setting or extra setuid helper is needed.
    let options = format!(
        "fd={},rootmode=40700,user_id={},group_id={},allow_other,default_permissions",
        fuse.as_raw_fd(),
        profile.uid()?,
        profile.gid()?
    );
    nix::mount::mount(
        Some("sandbox-documents"),
        &destination,
        Some("fuse.bindfs"),
        nix::mount::MsFlags::MS_NOSUID | nix::mount::MsFlags::MS_NODEV,
        Some(options.as_str()),
    )?;
    let mut bindfs = user_command(profile, "bindfs")?;
    bindfs
        .args([
            "-f",
            &format!("--force-user={}", uid_offset + profile.uid()?),
            &format!("--force-group={}", uid_offset + profile.gid()?),
            "--chown-deny",
            "--chmod-deny",
            "--direct-io",
            "-o",
            "attr_timeout=0,entry_timeout=0,negative_timeout=0",
        ])
        .arg(source)
        .arg(format!("/dev/fd/{}", fuse.as_raw_fd()));
    helpers.children.push(bindfs.spawn()?);
    // Closing our copy also makes a failed daemon startup abort the mount,
    // rather than leaving the readiness stat waiting on an unserved FUSE FD.
    drop(fuse);
    fs::metadata(&destination)?;
    Ok((socket, destination))
}
fn stage_mount(
    profile: &Profile,
    root: &Path,
    stage: &Path,
    index: usize,
    bind: &Mount,
    nspawn_arguments: &mut Vec<String>,
    uid_offset: u32,
) -> Result<()> {
    safe_mount(&bind.source, &bind.target)?;
    let source = PathBuf::from(&bind.source);
    let file = pin(profile, &source, bind.mode == "rw")?;
    let canonical = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    safe_mount(&canonical.to_string_lossy(), &bind.target)?;
    let directory = file.metadata()?.is_dir();
    let destination = root.join(bind.target.trim_start_matches('/'));
    if directory {
        fs::create_dir_all(&destination)?;
    } else {
        fs::create_dir_all(destination.parent().ok_or("missing parent")?)?;
        File::create(&destination)?;
    }
    own(&destination, profile.uid()?, profile.gid()?)?;
    let pinned = stage.join(index.to_string());
    if directory {
        fs::create_dir_all(&pinned)?;
    } else {
        File::create(&pinned)?;
    }
    inherit(&file)?;
    let mut mount = Command::new("mount");
    mount.arg("--bind");
    if bind.mode == "device" || file.metadata()?.uid() == profile.uid()? {
        // Expose the host owner's IDs as the guest owner, and preserve this
        // instance's disjoint high IDs for guest root, system services and
        // Podman. All other low host IDs, especially host root, stay unmapped.
        // Split the high range around the owner to avoid overlapping mappings.
        for (option, owner) in [
            ("--map-users", profile.uid()?),
            ("--map-groups", profile.gid()?),
        ] {
            mount.arg(format!("{option}={owner}:{}:1", uid_offset + owner));
            mount.arg(format!("{option}={uid_offset}:{uid_offset}:{owner}"));
            let remainder = 262144 - owner - 1;
            let start = uid_offset + owner + 1;
            mount.arg(format!("{option}={start}:{start}:{remainder}"));
        }
    }
    command_ok(
        mount
            .arg(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .arg(&pinned),
    )?;
    if bind.mode == "device" {
        // /run is normally nodev. The private staging mount inherits that
        // flag, which would make a correctly owned GPU/USB node fail on open.
        // Clear it only on the mount containing these checked device clones.
        command_ok(
            Command::new("mount")
                .args(["-o", "remount,bind,dev"])
                .arg(&pinned),
        )?;
    }
    let flag = if bind.mode != "ro" {
        "--bind"
    } else {
        "--bind-ro"
    };
    nspawn_arguments.push(format!(
        "{flag}={}:{}:noidmap",
        pinned.display(),
        bind.target
    ));
    Ok(())
}

// Create a mountpoint without following guest-controlled symlinks out of the
// selected parent mount. Directory FDs also keep concurrent path replacements
// from redirecting later components. The caller runs with the owner's IDs.
fn create_mountpoint_beneath(root: &File, relative: &Path, directory: bool) -> Result<()> {
    let components: Vec<_> = relative.components().collect();
    let mut parent = root.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err("nested mountpoints must stay beneath their parent".into());
        };
        let is_directory = index + 1 < components.len() || directory;
        let flags = OFlag::O_PATH
            | OFlag::O_CLOEXEC
            | OFlag::O_NOFOLLOW
            | if is_directory {
                OFlag::O_DIRECTORY
            } else {
                OFlag::empty()
            };
        let descriptor = match openat(&parent, *name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(Errno::ENOENT) => {
                let created = if is_directory {
                    mkdirat(&parent, *name, Mode::S_IRWXU)
                } else {
                    openat(
                        &parent,
                        *name,
                        OFlag::O_CREAT
                            | OFlag::O_EXCL
                            | OFlag::O_WRONLY
                            | OFlag::O_CLOEXEC
                            | OFlag::O_NOFOLLOW,
                        Mode::S_IRUSR | Mode::S_IWUSR,
                    )
                    .map(drop)
                };
                // Reopen with no-follow flags even when another process won
                // the creation race; never trust an existing symlink.
                match created {
                    Ok(()) | Err(Errno::EEXIST) => (),
                    Err(error) => return Err(error.into()),
                }
                openat(&parent, *name, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        let file = File::from(descriptor);
        let metadata = file.metadata()?;
        if metadata.file_type().is_symlink() || (!is_directory && metadata.is_dir()) {
            return Err("nested mountpoint has an incompatible type or is a symlink".into());
        }
        parent = file;
    }
    Ok(())
}

fn prepare_nested_mountpoints(profile: &Profile, mounts: &[Mount]) -> Result<()> {
    as_user(profile, || {
        for child in mounts
            .iter()
            .filter(|mount| matches!(mount.mode.as_str(), "ro" | "rw"))
        {
            let target = Path::new(&child.target);
            // An outer bind hides mountpoints created in the temporary image.
            // Prepare each nested target in its nearest containing bind source
            // instead. In particular, a new bind beneath a persistent home
            // must exist in that home before nspawn overlays it on the image.
            let parent = mounts
                .iter()
                .filter(|parent| {
                    matches!(parent.mode.as_str(), "ro" | "rw")
                        && target != Path::new(&parent.target)
                        && target.starts_with(&parent.target)
                })
                .max_by_key(|parent| Path::new(&parent.target).components().count());
            let Some(parent) = parent else { continue };
            let relative = target.strip_prefix(&parent.target)?;
            let root = OpenOptions::new()
                .read(true)
                .custom_flags((OFlag::O_PATH | OFlag::O_DIRECTORY).bits())
                .open(&parent.source)?;
            let directory = fs::metadata(&child.source)?.is_dir();
            create_mountpoint_beneath(&root, relative, directory).map_err(|error| {
                format!(
                    "cannot prepare nested mountpoint {} beneath {}: {error}",
                    child.target, parent.source
                )
            })?;
        }
        Ok(())
    })
}
fn clone_devices(profile: &Profile, source: &Path, destination: &Path, depth: usize) -> Result<()> {
    if depth > 5 {
        return Err("device directory nesting exceeds limit".into());
    }
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() {
        fs::create_dir_all(destination)?;
        // Keep directory entries root-owned: the owner can use the cloned
        // nodes, but must never replace a path that the root refresher chowns.
        for entry in fs::read_dir(source)? {
            let e = entry?;
            let target = destination.join(e.file_name());
            if clone_devices(profile, &e.path(), &target, depth + 1).is_err() {
                remove_device_clone(&target)?;
            }
        }
        for entry in fs::read_dir(destination)? {
            let entry = entry?;
            if fs::symlink_metadata(source.join(entry.file_name())).is_err() {
                remove_device_clone(&entry.path())?;
            }
        }
    } else if meta.file_type().is_char_device() {
        let file = pin(profile, source, true)?;
        let actual = file.metadata()?;
        if !actual.file_type().is_char_device() {
            return Err("device changed type".into());
        }
        if fs::symlink_metadata(destination).is_ok_and(|meta| meta.rdev() != actual.rdev()) {
            remove_device_clone(destination)?;
        }
        if !destination.exists() {
            mknod(
                destination,
                SFlag::S_IFCHR,
                Mode::S_IRUSR | Mode::S_IWUSR,
                actual.rdev(),
            )?;
        }
        own(destination, profile.uid()?, profile.gid()?)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
    } else {
        return Err("only character devices and device directories can be forwarded".into());
    }
    Ok(())
}

fn remove_device_clone(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(metadata) => {
            if metadata.file_type().is_char_device() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o000))?;
                own(path, 0, 0)?;
            }
            fs::remove_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

struct DeviceSource {
    source: PathBuf,
    destination: PathBuf,
    pattern: bool,
}

impl DeviceSource {
    fn refresh(&self, profile: &Profile) -> Result<()> {
        if !self.pattern {
            if clone_devices(profile, &self.source, &self.destination, 0).is_err() {
                if fs::symlink_metadata(&self.destination)
                    .is_ok_and(|metadata| metadata.file_type().is_char_device())
                {
                    fs::set_permissions(&self.destination, fs::Permissions::from_mode(0o000))?;
                    own(&self.destination, 0, 0)?;
                } else if self.destination.is_dir() {
                    // Keep the directory inode mounted in the guest. An
                    // unplugged bus may disappear and return later.
                    for entry in fs::read_dir(&self.destination)? {
                        remove_device_clone(&entry?.path())?;
                    }
                } else {
                    remove_device_clone(&self.destination)?;
                }
            }
            return Ok(());
        }
        fs::create_dir_all(&self.destination)?;
        let pattern = self.source.to_str().ok_or("invalid device pattern")?;
        let mut permitted = std::collections::HashSet::new();
        for source in glob::glob(pattern)? {
            let source = source?;
            if !fs::symlink_metadata(&source)?.file_type().is_char_device() {
                continue;
            }
            let name = source.file_name().ok_or("device has no filename")?;
            let target = self.destination.join(name);
            if clone_devices(profile, &source, &target, 0).is_ok() {
                permitted.insert(name.to_owned());
            }
        }
        // Revoke stale nodes as well as adding new ones. Otherwise a device
        // removed, renumbered, or made inaccessible on the host stays usable.
        for entry in fs::read_dir(&self.destination)? {
            let entry = entry?;
            if !permitted.contains(&entry.file_name()) {
                remove_device_clone(&entry.path())?;
            }
        }
        Ok(())
    }
}
struct Allocation {
    uid_offset: u32,
    _lock: File,
    transient: bool,
}
impl Drop for Allocation {
    fn drop(&mut self) {
        if self.transient {
            let _ = fs::remove_file(format!("/var/lib/sandbox/uid-ranges/{}", self.uid_offset));
        }
    }
}
fn uid_range(machine: &str, transient: bool) -> Result<Allocation> {
    let directory = Path::new("/var/lib/sandbox/uid-ranges");
    fs::create_dir_all(directory)?;
    let allocator = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".lock"))?;
    allocator.lock()?;
    // Persistent homes retain their subordinate ownership across restarts.
    // Reservations are root-owned and serialized across all profile services.
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() == ".lock" {
            continue;
        }
        if fs::read_to_string(entry.path())? == machine {
            let uid_offset = entry.file_name().to_string_lossy().parse()?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())?;
            file.try_lock()?;
            return Ok(Allocation {
                uid_offset,
                _lock: file,
                transient,
            });
        }
    }
    let hash = machine
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    for n in 0..8192u32 {
        let uid_offset = 0x40000000 + ((hash.wrapping_add(n)) % 8192) * 262144;
        let path = directory.join(uid_offset.to_string());
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.try_lock()?;
                file.write_all(machine.as_bytes())?;
                return Ok(Allocation {
                    uid_offset,
                    _lock: file,
                    transient,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err("no sandbox UID ranges available".into())
}
// Always use a private network namespace, even for offline profiles: abstract
// Unix sockets (including X11) live in the network namespace. Its owning user
// namespace lets pasta configure it after dropping host root privileges.
fn netns(profile: &Profile, helpers: &mut Helpers) -> Result<PathBuf> {
    let original = fs::metadata("/proc/self/ns/net")?.ino();
    let mut c = user_command(profile, "unshare")?;
    c.args([
        "--user",
        "--map-root-user",
        "--net",
        "--",
        "/bin/sh",
        "-ec",
        "ip link set lo up; exec sleep infinity",
    ]);
    let mut child = c.spawn()?;
    let path = PathBuf::from(format!("/proc/{}/ns/net", child.id()));
    for _ in 0..200 {
        if child.try_wait()?.is_some() {
            return Err("network namespace setup failed".into());
        }
        if fs::metadata(&path).is_ok_and(|s| s.ino() != original) {
            helpers.children.push(child);
            return Ok(path);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("network namespace setup timed out".into())
}
fn notify_ready() -> Result<()> {
    use std::os::linux::net::SocketAddrExt;
    let name = std::env::var("NOTIFY_SOCKET")?;
    let socket = UnixDatagram::unbound()?;
    if let Some(abstract_name) = name.strip_prefix('@') {
        let address = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())?;
        socket.send_to_addr(b"READY=1", &address)?;
    } else {
        socket.send_to(b"READY=1", name)?;
    }
    Ok(())
}

fn received_ready(socket: &UnixDatagram) -> Result<bool> {
    let mut message = [0; 4096];
    match socket.recv(&mut message) {
        Ok(length) => Ok(message[..length]
            .split(|byte| *byte == b'\n')
            .any(|field| field == b"READY=1")),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn serve(profile: &Profile, instance: &str) -> Result<i32> {
    if !geteuid().is_root() {
        return Err("the nspawn supervisor runs only through its installed system service".into());
    }
    let canonical = fs::canonicalize(&profile.dir)?;
    if !canonical.starts_with("/nix/store") {
        return Err("system services require an immutable Nix store profile".into());
    }
    // Named homes can be deleted while stopped. Keep the owner-side lifecycle
    // lock until this supervisor exits, so deletion and startup never overlap.
    let _storage_lock = if instance.starts_with("n-") {
        Some(as_user(profile, || {
            crate::storage::lock_instance(profile, instance)
        })?)
    } else {
        None
    };
    let machine = profile.machine(instance)?;
    let runtime = PathBuf::from(format!("/run/{machine}"));
    if !runtime.is_dir() {
        return Err("missing systemd RuntimeDirectory".into());
    }
    // A running instance may outlive the host generation that declared it.
    // Keep its complete guest/profile closure alive until systemd removes the
    // runtime directory, including when the supervisor is forcibly killed.
    command_ok(
        Command::new("nix-store")
            .arg("--add-root")
            .arg(runtime.join("profile-root"))
            .args(["--indirect", "--realise"])
            .arg(&canonical)
            .stdout(Stdio::null()),
    )?;
    // Some guest directories must be owned by the host owner before ID
    // mapping. Hide their ancestors from host processes during preparation,
    // otherwise the owner could race root's target creation with symlinks.
    let image_directory = runtime.join("image");
    fs::create_dir_all(&image_directory)?;
    fs::set_permissions(&image_directory, fs::Permissions::from_mode(0o700))?;
    let root = image_directory.join("root");
    let stage = runtime.join("mounts");
    fs::create_dir_all(&stage)?;
    command_ok(
        Command::new("cp")
            .args(["-a", "--no-preserve=ownership"])
            .arg(profile.dir.join("root"))
            .arg(&root),
    )?;
    // The fresh tree belongs to root; the application's home belongs to its
    // container user before nspawn applies its private user namespace mapping.
    let home = root.join(profile.get("home")?.trim_start_matches('/'));
    fs::create_dir_all(&home)?;
    own(&home, profile.uid()?, profile.gid()?)?;
    let user_runtime = profile.runtime(instance)?;
    let wayland = as_user(profile, || {
        Ok(fs::read_to_string(user_runtime.join("wayland"))?)
    })?;
    if !wayland.is_empty()
        && (!wayland
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)))
    {
        return Err("invalid Wayland socket name".into());
    }
    let persistent_home = profile.persistent_home(instance)?;
    let allocation = uid_range(&machine, persistent_home.is_none())?;
    let uid_offset = allocation.uid_offset;
    let mut helpers = Helpers {
        children: vec![],
        latches: vec![],
        cleanup: vec![],
    };
    let mut nspawn_arguments = vec![
        "--quiet".into(),
        "--keep-unit".into(),
        "--register=yes".into(),
        "--settings=no".into(),
        "--link-journal=no".into(),
        "--notify-ready=yes".into(),
        "--kill-signal=SIGRTMIN+3".into(),
        format!("--machine={machine}"),
        format!("--directory={}", root.display()),
        format!("--private-users={uid_offset}:262144"),
        "--private-users-ownership=map".into(),
        "--bind-ro=/nix/store:/nix/store:noidmap".into(),
        "--tmpfs=/tmp:mode=1777".into(),
        "--resolv-conf=off".into(),
    ];
    let namespace = netns(profile, &mut helpers)?;
    nspawn_arguments.push(format!("--network-namespace-path={}", namespace.display()));
    if profile.flag("network") {
        // pasta configures a dedicated netns and drops host root privileges.
        let mut pasta = Command::new("pasta");
        pasta
            .args([
                "--foreground",
                "--config-net",
                "--runas",
                profile.get("username")?,
                "--userns",
            ])
            .arg(
                namespace
                    .parent()
                    .ok_or("missing namespace parent")?
                    .join("user"),
            )
            .arg("--netns")
            .arg(&namespace)
            .args(&profile.pasta);
        helpers.children.push(pasta.spawn()?);
    }
    let mut mounts = profile.mounts.clone();
    if let Some(home_source) = persistent_home {
        as_user(profile, || {
            fs::create_dir_all(&home_source)?;
            Ok(())
        })?;
        mounts.insert(
            0,
            Mount {
                mode: "rw".into(),
                source: home_source.to_string_lossy().into(),
                target: profile.get("home")?.into(),
            },
        );
    }
    if !wayland.is_empty() {
        mounts.push(Mount {
            mode: "ro".into(),
            source: format!("/run/user/{}/{wayland}", profile.uid()?),
            target: "/run/host/wayland".into(),
        });
    }
    for (flag, source, target) in [
        (
            "audio",
            format!("/run/user/{}/pipewire-0", profile.uid()?),
            "/run/host/pipewire".into(),
        ),
        (
            "audio",
            format!("/run/user/{}/pulse/native", profile.uid()?),
            "/run/host/pulse".into(),
        ),
    ] {
        if profile.flag(flag) && Path::new(&source).exists() {
            mounts.push(Mount {
                mode: "rw".into(),
                source,
                target,
            });
        }
    }
    if profile.flag("portals") {
        let (socket, documents) =
            prepare_portals(profile, instance, &runtime, uid_offset, &mut helpers)?;
        mounts.push(Mount {
            mode: "rw".into(),
            source: socket.to_string_lossy().into(),
            target: "/run/host/portal-bus".into(),
        });
        // This mount is prepared privately by root and already presents the
        // guest's shifted IDs. It must not be mapped a second time.
        nspawn_arguments.push(format!(
            "--bind={}:{}:noidmap",
            documents.display(),
            "/run/host/doc"
        ));
        if profile.flag("hostSessionBus") {
            mounts.push(Mount {
                mode: "rw".into(),
                source: socket.to_string_lossy().into(),
                target: "/run/host/session-bus".into(),
            });
        }
    }
    if profile.flag("inheritAppearance") && !profile.flag("portals") {
        // Keep this proxy separate from the optional host session bus: an
        // application-specific policy must not widen the appearance bridge.
        let directory = user_runtime.join("appearance");
        as_user(profile, || {
            fs::create_dir_all(&directory)?;
            Ok(())
        })?;
        let rules = [
            "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.*".into(),
            "--broadcast=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.SettingChanged".into(),
        ];
        let socket = proxy(profile, &directory, false, &mut helpers, None, Some(&rules))?;
        mounts.push(Mount {
            mode: "rw".into(),
            source: socket.to_string_lossy().into(),
            target: "/run/host/appearance-bus".into(),
        });
    }
    for (enabled, system, target) in [
        (
            profile.flag("hostSessionBus") && !profile.flag("portals"),
            false,
            "/run/host/session-bus",
        ),
        (
            !profile.system_rules.is_empty(),
            true,
            "/run/host/system-bus",
        ),
    ] {
        if enabled {
            let socket = proxy(profile, &user_runtime, system, &mut helpers, None, None)?;
            mounts.push(Mount {
                mode: "rw".into(),
                source: socket.to_string_lossy().into(),
                target: target.into(),
            });
        }
    }
    if profile.flag("nix") {
        let socket = user_runtime.join("nix-daemon");
        as_user(profile, || {
            if socket.symlink_metadata().is_ok() {
                fs::remove_file(&socket)?;
            }
            Ok(())
        })?;
        let mut c = user_command(
            profile,
            std::env::current_exe()?
                .to_str()
                .ok_or("invalid executable path")?,
        )?;
        c.arg("--nix-proxy")
            .arg(&socket)
            .arg("/nix/var/nix/daemon-socket/socket");
        helpers.children.push(c.spawn()?);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            if Instant::now() > deadline {
                return Err("Nix daemon proxy startup timed out".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        mounts.push(Mount {
            mode: "rw".into(),
            source: socket.to_string_lossy().into(),
            target: "/nix/var/nix/daemon-socket/socket".into(),
        });
    }
    // Host root is deliberately absent from owner-mapped bind mounts. It
    // cannot mkdir there (EOVERFLOW), so prepare targets as the host owner
    // before nspawn enters its mount setup. Do not weaken the UID mappings.
    prepare_nested_mountpoints(profile, &mounts)?;
    let mut devices = Vec::new();
    for (i, bind) in mounts.iter().enumerate() {
        if bind.mode == "device-pattern" {
            let selection = DeviceSource {
                source: PathBuf::from(&bind.source),
                destination: stage.join(format!("device-patterns-{i}")),
                pattern: true,
            };
            selection.refresh(profile)?;
            let clone = Mount {
                mode: "device".into(),
                source: selection.destination.to_string_lossy().into(),
                target: format!("/run/host/device-patterns/{i}"),
            };
            stage_mount(
                profile,
                &root,
                &stage,
                i,
                &clone,
                &mut nspawn_arguments,
                uid_offset,
            )?;
            devices.push(selection);
            continue;
        }
        if bind.mode == "device" {
            // nspawn supplies shared virtual devices with their required modes.
            // Replacing /dev/null with an owner-only clone breaks guest system
            // services running under other UIDs.
            if [
                "/dev/null",
                "/dev/zero",
                "/dev/full",
                "/dev/random",
                "/dev/urandom",
                "/dev/tty",
            ]
            .contains(&bind.source.as_str())
            {
                continue;
            }
            let deferred_directory =
                ["/dev/dri", "/dev/input", "/dev/bus/usb"].contains(&bind.source.as_str());
            if !Path::new(&bind.source).exists() && !deferred_directory {
                continue;
            }
            let cloned = stage.join(format!("devices-{i}"));
            if deferred_directory {
                fs::create_dir_all(&cloned)?;
            }
            if let Err(error) = clone_devices(profile, Path::new(&bind.source), &cloned, 0) {
                // Explicit device lists can include hardware whose ACLs are
                // unavailable in this session. Forward only accessible nodes;
                // lack of access must not prevent the whole sandbox booting.
                eprintln!("Skipping device {}: {error}", bind.source);
                if !deferred_directory {
                    continue;
                }
            }
            if !cloned.exists() {
                continue;
            }
            let mut clone = bind.clone();
            clone.source = cloned.to_string_lossy().into();
            stage_mount(
                profile,
                &root,
                &stage,
                i,
                &clone,
                &mut nspawn_arguments,
                uid_offset,
            )?;
            devices.push(DeviceSource {
                source: PathBuf::from(&bind.source),
                destination: cloned,
                pattern: false,
            });
        } else {
            stage_mount(
                profile,
                &root,
                &stage,
                i,
                bind,
                &mut nspawn_arguments,
                uid_offset,
            )?;
        }
    }
    nspawn_arguments.push(profile.get("init")?.into());
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))?;
    // nspawn relays PID 1's boot-complete notification. Receive that privately,
    // then check the activation result once: systemd can finish booting with a
    // failed wanted service. Only the supervisor may tell the host we're ready.
    // The socket is outside the guest root, beneath a root-only directory, so
    // neither the owner nor a guest process can forge the relayed notification.
    let notification_path = image_directory.join("notify");
    let notifications = UnixDatagram::bind(&notification_path)?;
    notifications.set_nonblocking(true)?;
    fs::set_permissions(&notification_path, fs::Permissions::from_mode(0o600))?;
    let mut nspawn = Command::new("systemd-nspawn");
    nspawn
        .args(&nspawn_arguments)
        .env("NOTIFY_SOCKET", &notification_path);
    let mut child = nspawn.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(110);
    let mut ready = false;
    let mut stopping = false;
    let mut refresh = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(1));
        }
        if stop.load(Ordering::Relaxed) && !stopping {
            let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
            stopping = true;
        }
        if !ready && !stopping && (received_ready(&notifications)? || Instant::now() > deadline) {
            let status = Command::new("systemctl")
                .args([
                    "--machine",
                    &machine,
                    "is-active",
                    "--quiet",
                    "sandbox-ready.service",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if status.success() {
                notify_ready()?;
                ready = true;
            } else {
                let _ = Command::new("systemctl")
                    .args([
                        "--machine",
                        &machine,
                        "status",
                        "--no-pager",
                        "--full",
                        "sandbox-ready.service",
                    ])
                    .status();
                let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
                let reason = if Instant::now() > deadline {
                    "container boot or Home Manager activation timed out"
                } else {
                    "sandbox home activation failed; inspect the host sandbox unit's journal"
                };
                return Err(reason.into());
            }
        }
        if !stopping {
            for helper in &mut helpers.children {
                if let Some(status) = helper.try_wait()? {
                    let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
                    return Err(format!("sandbox helper exited: {status}").into());
                }
            }
        }
        if refresh.elapsed() > Duration::from_secs(1) {
            for source in &devices {
                let _ = source.refresh(profile);
            }
            refresh = Instant::now();
        }
        thread::sleep(Duration::from_millis(if ready || stopping {
            100
        } else {
            10
        }));
    }
}
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
#[cfg(test)]
mod tests {
    #[test]
    fn nested_mountpoint_creation_cannot_follow_symlinks() {
        use super::create_mountpoint_beneath;
        use std::{fs, os::unix::fs::symlink, path::Path};

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("existing"), "keep this content").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        symlink(outside.path().join("existing"), root.path().join("file")).unwrap();
        let directory = fs::File::open(root.path()).unwrap();

        assert!(create_mountpoint_beneath(&directory, Path::new("escape/new"), true).is_err());
        assert!(create_mountpoint_beneath(&directory, Path::new("file"), false).is_err());
        assert!(create_mountpoint_beneath(&directory, Path::new("../new"), true).is_err());
        assert!(!outside.path().join("new").exists());
        assert_eq!(
            fs::read_to_string(outside.path().join("existing")).unwrap(),
            "keep this content"
        );
    }

    #[test]
    fn uid_ranges_do_not_map_host_root() {
        for hash in [0u32, 8191, 8192, u32::MAX] {
            let uid_offset = 0x40000000 + (hash % 8192) * 262144;
            assert!(uid_offset > 65535);
            assert!((uid_offset as u64) + 262144 < (u32::MAX as u64));
        }
    }
}
