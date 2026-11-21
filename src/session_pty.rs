use crate::config::Result;
use dbus::{blocking::Connection, channel::Channel};
use rustix::{
    event::{PollFd, PollFlags, poll},
    fs::{Mode, OFlags, fchmod, fcntl_getfl, fcntl_setfl},
    io::{Errno, read, write},
    process::{Pid, PidfdFlags, pidfd_open},
    pty::{OpenptFlags, ioctl_tiocgptpeer},
    termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr, tcsetwinsize},
};
use signal_hook::{SigId, consts::signal::*, low_level};
use std::{
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::{net::UnixStream, process::ExitStatusExt},
    },
    process::{Command, Stdio},
    time::Duration,
};

pub(crate) fn run(
    mut command: Command,
    machine: &str,
    guest_uid: u32,
    entry: &str,
    args: &[String],
) -> Result<i32> {
    // systemd-run --machine --pty also uses OpenMachinePTY, but never tells
    // machined which user will open the slave. Get the same PTY and set its
    // owner before handing it to the guest's existing user service manager.
    // The privileged file-descriptor operation must use the host system bus.
    let mut channel = Channel::open_private("unix:path=/run/dbus/system_bus_socket")?;
    channel.register()?;
    let bus = Connection::from(channel);
    let manager = bus.with_proxy(
        "org.freedesktop.machine1",
        "/org/freedesktop/machine1",
        Duration::from_secs(10),
    );
    let interface = "org.freedesktop.machine1.Manager";
    let (master, path): (OwnedFd, String) =
        manager.method_call(interface, "OpenMachinePTY", (machine,))?;
    let (owner,): (u32,) =
        manager.method_call(interface, "MapFromMachineUser", (machine, guest_uid))?;
    drop(bus);
    if owner == 0 || owner == u32::MAX {
        return Err("invalid host UID for the sandbox terminal owner".into());
    }

    // The kernel supplies the peer of this exact master. Never chown a path
    // in the guest, where a privileged process could race pathname lookup.
    let slave = ioctl_tiocgptpeer(
        &master,
        OpenptFlags::RDWR | OpenptFlags::CLOEXEC | OpenptFlags::NOCTTY,
    )?;
    nix::unistd::fchown(&slave, Some(nix::unistd::Uid::from_raw(owner)), None)?;
    fchmod(&slave, Mode::RUSR | Mode::WUSR)?;

    let input = io::stdin();
    let output = io::stdout();
    let resized = SignalPipe::new(&[SIGWINCH, SIGCONT])?;
    let interrupted = SignalPipe::new(&[SIGHUP, SIGINT, SIGQUIT, SIGTERM])?;
    tcsetwinsize(&master, tcgetwinsize(&input)?)?;
    let _input_mode = RawTerminal::new(input.as_fd())?;
    let _output_mode = RawTerminal::new(output.as_fd())?;

    command
        .arg(format!("--property=TTYPath={path}"))
        .arg("--")
        .arg(entry)
        .args(args)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot invoke trusted systemd-run: {error}"))?;
    drop(command); // Close the parent's slave FDs so the master sees hangup.

    let forwarded = (|| {
        let pid = Pid::from_raw(child.id().try_into()?).ok_or("missing systemd-run PID")?;
        let exited = pidfd_open(pid, PidfdFlags::empty())?;
        forward(
            &master,
            input.as_fd(),
            output.as_fd(),
            &resized,
            &interrupted,
            &exited,
        )
    })();
    drop(master);
    if forwarded.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    forwarded?;
    let status = status?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

struct RawTerminal<'a> {
    fd: BorrowedFd<'a>,
    saved: Termios,
    flags: OFlags,
}

impl<'a> RawTerminal<'a> {
    fn new(fd: BorrowedFd<'a>) -> Result<Self> {
        let saved = tcgetattr(fd)?;
        let flags = fcntl_getfl(fd)?;
        let terminal = Self { fd, saved, flags };
        let mut raw = terminal.saved.clone();
        raw.make_raw();
        tcsetattr(fd, OptionalActions::Now, &raw)?;
        fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
        Ok(terminal)
    }
}

impl Drop for RawTerminal<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.fd, OptionalActions::Now, &self.saved);
        let _ = fcntl_setfl(self.fd, self.flags);
    }
}

struct SignalPipe {
    receiver: UnixStream,
    handlers: Vec<SigId>,
}

impl SignalPipe {
    fn new(signals: &[i32]) -> io::Result<Self> {
        let (receiver, sender) = UnixStream::pair()?;
        let mut pipe = Self {
            receiver,
            handlers: Vec::new(),
        };
        for &signal in signals {
            pipe.handlers
                .push(low_level::pipe::register(signal, sender.try_clone()?)?);
        }
        Ok(pipe)
    }
}

impl Drop for SignalPipe {
    fn drop(&mut self) {
        for id in self.handlers.drain(..) {
            low_level::unregister(id);
        }
    }
}

struct Buffer {
    bytes: [u8; 8192],
    sent: usize,
    len: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            bytes: [0; 8192],
            sent: 0,
            len: 0,
        }
    }

    fn pending(&self) -> bool {
        self.sent < self.len
    }

    // Read at most one chunk in each direction until the destination drains
    // it. A blocked guest or terminal cannot grow the relay's memory use.
    fn read(&mut self, source: impl AsFd) -> rustix::io::Result<usize> {
        self.sent = 0;
        let result = read(source, self.bytes.as_mut_slice());
        self.len = *result.as_ref().unwrap_or(&0);
        result
    }

    fn write(&mut self, destination: impl AsFd) -> rustix::io::Result<()> {
        match write(destination, &self.bytes[self.sent..self.len]) {
            Ok(0) => return Err(Errno::IO),
            Ok(n) => self.sent += n,
            Err(Errno::AGAIN | Errno::INTR) => (),
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

fn forward(
    master: &OwnedFd,
    input: BorrowedFd<'_>,
    output: BorrowedFd<'_>,
    resized: &SignalPipe,
    interrupted: &SignalPipe,
    exited: &OwnedFd,
) -> Result<()> {
    fcntl_setfl(master, fcntl_getfl(master)? | OFlags::NONBLOCK)?;
    let mut to_guest = Buffer::new();
    let mut from_guest = Buffer::new();
    let mut child_exited = false;
    let mut guest_output_open = true;
    let mut guest_input_open = true;
    let mut events = Vec::with_capacity(6);
    loop {
        if child_exited && !from_guest.pending() {
            // Flush output already queued in the PTY after systemd-run exits.
            // A background process keeping the slave open must not hold up exit.
            match from_guest.read(master) {
                Ok(0) | Err(Errno::IO | Errno::AGAIN) => return Ok(()),
                Ok(_) | Err(Errno::INTR) => (),
                Err(error) => return Err(error.into()),
            }
            if !from_guest.pending() {
                continue;
            }
        }

        events.clear();
        let mut watch = |fd, flags| {
            let index = events.len();
            events.push(PollFd::from_borrowed_fd(fd, flags));
            index
        };
        let stop_event = watch(interrupted.receiver.as_fd(), PollFlags::IN);
        let resize_event = watch(resized.receiver.as_fd(), PollFlags::IN);
        let exit_event = (!child_exited).then(|| watch(exited.as_fd(), PollFlags::IN));
        let read_input = !child_exited && guest_input_open && !to_guest.pending();
        let input_event = read_input.then(|| watch(input, PollFlags::IN));
        let output_event = from_guest.pending().then(|| watch(output, PollFlags::OUT));
        let mut guest_events = PollFlags::empty();
        if !child_exited && guest_output_open && !from_guest.pending() {
            guest_events |= PollFlags::IN;
        }
        if !child_exited && guest_input_open && to_guest.pending() {
            guest_events |= PollFlags::OUT;
        }
        let master_event = (!guest_events.is_empty()).then(|| watch(master.as_fd(), guest_events));
        match poll(&mut events, None) {
            Ok(_) => (),
            Err(Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }

        let ready =
            |event: Option<usize>| event.map_or(PollFlags::empty(), |i| events[i].revents());
        if !ready(Some(stop_event)).is_empty() {
            return Err("sandbox terminal session interrupted".into());
        }
        if !ready(Some(resize_event)).is_empty() {
            let _ = read(&resized.receiver, &mut [0; 4096][..])?;
            tcsetwinsize(master, tcgetwinsize(input)?)?;
        }
        child_exited |= !ready(exit_event).is_empty();
        if !ready(output_event).is_empty() {
            from_guest.write(output)?;
        }
        let master_ready = ready(master_event);
        if guest_events.contains(PollFlags::IN)
            && master_ready.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
        {
            match from_guest.read(master) {
                Ok(0) | Err(Errno::IO) => {
                    guest_output_open = false;
                    guest_input_open = false;
                }
                Ok(_) | Err(Errno::AGAIN | Errno::INTR) => (),
                Err(error) => return Err(error.into()),
            }
        }
        if guest_input_open
            && master_ready.intersects(PollFlags::OUT | PollFlags::HUP | PollFlags::ERR)
            && to_guest.pending()
        {
            match to_guest.write(master) {
                Ok(()) => (),
                Err(Errno::IO) => guest_input_open = false,
                Err(error) => return Err(error.into()),
            }
        }
        if !child_exited && !ready(input_event).is_empty() {
            match to_guest.read(input) {
                Ok(0) | Err(Errno::IO) => return Err("host terminal disconnected".into()),
                Ok(_) | Err(Errno::AGAIN | Errno::INTR) => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
}
