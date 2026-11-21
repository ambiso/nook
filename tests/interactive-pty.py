import errno
import os
import pty
import pwd
import select
import shlex
import signal
import subprocess
import termios
import time


def as_alice(command):
    return ["su", "-", "alice", "-c", command]


def check_terminal(command, interact, expected_status):
    pid, master = pty.fork()
    if pid == 0:
        termios.tcsetwinsize(0, (24, 100))
        # su -c starts a new session. Change credentials in place so this
        # foreground process group receives SIGWINCH from the host terminal.
        alice = pwd.getpwnam("alice")
        os.initgroups(alice.pw_name, alice.pw_gid)
        os.setgid(alice.pw_gid)
        os.setuid(alice.pw_uid)
        os.chdir(alice.pw_dir)
        os.execve(
            alice.pw_shell,
            ["-" + os.path.basename(alice.pw_shell), "-c", command],
            {
                "HOME": alice.pw_dir,
                "USER": alice.pw_name,
                "LOGNAME": alice.pw_name,
                "SHELL": alice.pw_shell,
                "TERM": "xterm-256color",
                "XDG_RUNTIME_DIR": f"/run/user/{alice.pw_uid}",
            },
        )

    output = bytearray()
    deadline = time.monotonic() + 20

    def read():
        remaining = max(0, deadline - time.monotonic())
        assert select.select([master], [], [], remaining)[0], bytes(output[-2048:])
        try:
            data = os.read(master, 8192)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            return False
        output.extend(data)
        return bool(data)

    def expect(marker):
        while marker not in output:
            assert read(), (len(marker), marker[:80], bytes(output[-2048:]))

    try:
        interact(master, expect)
        while read():
            pass
        # Bound the wait even if the process closes its terminal early.
        while True:
            result, status = os.waitpid(pid, os.WNOHANG)
            if result:
                pid = None
                assert os.waitstatus_to_exitcode(status) == expected_status, bytes(output[-2048:])
                break
            assert time.monotonic() < deadline, bytes(output[-2048:])
            time.sleep(0.01)
    finally:
        os.close(master)
        if pid is not None:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


# Reopen the PTY by path from a separate process, as terminal pinentry does.
check_owner = (
    'path=$(tty); owner_and_mode=$(stat -c \'%u %a\' "$path"); '
    'if test "$owner_and_mode" != "1234 600"; then '
    'echo "Unexpected PTY owner and mode: $owner_and_mode" >&2; exit 1; fi; '
    'bash -ec \': 3<>"$1"; : 4<>/dev/tty\' reopen "$path"'
)

pipe = subprocess.run(
    as_alice(
        "nk launch check -- bash -ec "
        + shlex.quote(
            'test ! -t 0; test ! -t 1; mkdir -p "$HOME/work with spaces"; '
            "printf 'pipe output'; exit 37"
        )
    ),
    input=b"",
    capture_output=True,
    timeout=30,
)
assert pipe.returncode == 37 and pipe.stdout == b"pipe output", pipe


def cli_session(master, expect):
    expect(b"x" * 32768)
    expect(b"NOOK-CLI-READY")
    termios.tcsetwinsize(master, (30, 120))
    os.write(master, b"\n")
    expect(b"NOOK-CLI-RESIZED")
    os.write(master, b"\x03")


cli_check = (
    check_owner
    + '; test "$TERM" = xterm-256color; test "$NOOK_TEST_ENTRY" = forwarded; '
    'test "$PWD" = "$HOME/work with spaces"; test "$(stty size)" = "24 100"; '
    "printf '%32768s' '' | tr ' ' x; printf 'NOOK-CLI-READY\\n'; read -r; "
    'for attempt in {1..50}; do test "$(stty size)" = "30 120" && break; sleep .02; done; '
    'test "$(stty size)" = "30 120"; '
    "trap 'exit 42' INT; printf 'NOOK-CLI-RESIZED\\n'; read -r"
)
check_terminal(
    "cd '.sandbox/check/instances/main/home/work with spaces' && "
    "NOOK_TEST_ENTRY=forwarded nk launch check --join -- bash -ec "
    + shlex.quote(cli_check),
    cli_session,
    42,
)


def tui_session(master, expect):
    expect(b"Nook")
    os.write(master, b"\r")
    expect(b"NOOK-GUEST-PROMPT>")
    # The input echo contains the format string, not the complete marker.
    guest_check = check_owner + "; printf 'NOOK-%s\\n' TUI-OK"
    os.write(master, ("bash -ec " + shlex.quote(guest_check) + "; exit $?\r").encode())
    expect(b"NOOK-TUI-OK")


check_terminal("nk", tui_session, 0)
