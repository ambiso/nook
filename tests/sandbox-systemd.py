import shlex
import textwrap

machine.start()
machine.wait_for_unit("multi-user.target")
machine.wait_for_unit("home-manager-alice.service")
machine.wait_for_unit("unbound.service")
machine.wait_until_succeeds("test -S /run/user/1000/wayland-test")
machine.wait_until_succeeds("test -S /tmp/.X11-unix/X77")
machine.wait_until_succeeds("test -S /run/user/1000/pipewire-0")
machine.wait_until_succeeds("test -S /run/user/1000/pulse/native")


def as_owner(command):
    return "su - alice -c " + shlex.quote("set -e; " + command)


def launcher(args):
    # Deliberately supply host X11 variables; only Wayland should be forwarded.
    return as_owner(
        "WAYLAND_DISPLAY=wayland-test DISPLAY=:77 "
        "XAUTHORITY=/home/alice/host.xauth nk " + args
    )


def cli(args):
    return machine.succeed(launcher(args))


def inside(script, options=""):
    command = " -- bash -euc " + shlex.quote(textwrap.dedent(script))
    return cli("launch check " + options + command)


with subtest("automatic host UID/GID allocation and independent guest IDs"):
    machine.wait_for_unit("home-manager-bob.service")
    host_uid = int(machine.succeed("id -u bob").strip())
    host_gid = int(machine.succeed("id -g bob").strip())
    assert host_uid != 1000 and host_gid != 100
    machine.wait_for_unit(f"user@{host_uid}.service")
    command = "su - bob -c " + shlex.quote(
        "nk launch dev -- sh -ec 'test $(id -u) = 1000; test $(id -g) = 100; echo owner-check > ~/owner-check'"
    )
    machine.succeed(command)
    machine.succeed(
        f"test $(stat -c %u /home/bob/.sandbox/dev/home/owner-check) = {host_uid}"
    )
    machine.succeed(
        f"test $(stat -c %g /home/bob/.sandbox/dev/home/owner-check) = {host_gid}"
    )
    for key, value in {
        "gtk-theme": "Adwaita-dark",
        "icon-theme": "Adwaita",
        "color-scheme": "prefer-dark",
    }.items():
        configured = machine.succeed(
            "su - bob -c "
            + shlex.quote(
                "nk launch dev --join -- dconf read /org/gnome/desktop/interface/" + key
            )
        ).strip()
        assert configured == repr(value), (key, configured)
    machine.succeed("systemctl stop sandbox-ubob-dev@main.service")


try:
    shared_storage = "/home/alice/.sandbox/check/home/SharedStorage"
    assert (
        machine.succeed(f"findmnt -n -o FSTYPE -T {shared_storage}").strip() == "tmpfs"
    )
    machine.succeed(as_owner(f"echo mounted-storage > {shared_storage}/host-marker"))
    machine.succeed("test ! -e /home/alice/.sandbox/check/home/Inbox")
    # bwrap left mountpoint placeholders in the backing home. Migration must
    # back them up, preserving edited files and directory contents as well.
    machine.succeed(
        as_owner(
            "mkdir -p ~/.sandbox/check/home/legacy-directory; "
            "touch ~/.sandbox/check/home/.hm-generation ~/.sandbox/check/home/activation-check; "
            "echo 'legacy user edit' > ~/.sandbox/check/home/legacy-edit; "
            "echo 'legacy directory data' > ~/.sandbox/check/home/legacy-directory/original"
        )
    )
    # Refusing a cold start must leave the service stopped.
    machine.fail(launcher("launch check --join -- true"))
    machine.fail("systemctl is-active --quiet sandbox-ualice-check@main.service")
    # Concurrent cold starts must finish activation once and join one machine.
    first = launcher("launch check -- readlink /proc/self/ns/pid")
    machine.succeed(
        f"{first} >/tmp/ns-a & first=$!; {first} >/tmp/ns-b & second=$!; "
        'wait "$first"; wait "$second"'
    )
    machine.succeed("cmp /tmp/ns-a /tmp/ns-b")
    inside("true", "--join")
    assert inside("uname -n").strip() == "check-main"
    inside("getent hosts check-main")
    # Transient services receive concrete NixOS cursor search paths, including
    # the per-home directory used by Home Manager cursor themes.
    inside("""
        case "$XCURSOR_PATH" in "$HOME/.icons:$HOME/.local/share/icons:"*) ;; *) exit 1;; esac
        case ":$XCURSOR_PATH:" in *:/run/current-system/sw/share/icons:*) ;; *) exit 1;; esac
        case "$XCURSOR_PATH" in *'$HOME'*|*'${HOME}'*|*'$USER'*|*'${XDG_STATE_HOME}'*) exit 1;; esac
    """)
    # Activation and later application writes both use the guest's dconf service.
    assert (
        inside("dconf read /org/gnome/desktop/interface/gtk-theme").strip()
        == "'Adwaita-dark'"
    )
    inside("dconf write /org/gnome/desktop/interface/cursor-size 32")
    assert inside("dconf read /org/gnome/desktop/interface/cursor-size").strip() == "32"
    # The compatibility wrapper maps the backing home even when the shell's
    # logical current directory reaches it through a host symlink.
    project_directory = "/home/alice/.sandbox/check/home/Projects/project's %h/subdir"
    machine.succeed(
        as_owner(
            "mkdir -p "
            + shlex.quote(project_directory)
            + "; ln -s ~/.sandbox/check/home/Projects ~/Projects"
        )
    )
    for directory in [
        project_directory,
        "/home/alice/Projects/project's %h/subdir",
    ]:
        assert (
            machine.succeed(
                as_owner("cd " + shlex.quote(directory) + " && nk-check -c pwd")
            ).strip()
            == "/home/alice/Projects/project's %h/subdir"
        )
    assert (
        machine.succeed(as_owner("cd /tmp && nk-check -c pwd")).strip() == "/home/alice"
    )
    # Starting another instance must not reuse this one's machine identity.
    first_machine_id = inside("cat /etc/machine-id").strip()
    inside("""
        test $(stat -c %u ~/SharedStorage) = 1000
        test $(stat -c %g ~/SharedStorage) = 100
        grep -Fx mounted-storage ~/SharedStorage/host-marker
        echo guest-output > ~/SharedStorage/guest-marker
        mkdir -p ~/build ~/.cache/sccache
        touch ~/build/main-output ~/.cache/sccache/main-cache
    """)
    machine.succeed(as_owner(f"grep -Fx guest-output {shared_storage}/guest-marker"))
    inside("""
        test -f ~/activation-check.pre-systemd
        test ! -s ~/activation-check.pre-systemd
        grep -Fx 'legacy user edit' ~/legacy-edit.pre-systemd
        grep -Fx 'new managed configuration' ~/legacy-edit
        grep -Fx 'legacy directory data' ~/legacy-directory.pre-systemd/original
        grep -Fx 'managed directory' ~/legacy-directory/managed
    """)
    # Ordinary homes retain HM's collision checks. A failed activation leaves
    # the guest available for an ordinary backup-and-retry command.
    machine.succeed(
        as_owner(
            "mkdir -p ~/.sandbox/check/instances/n-collision/home; "
            "echo 'keep my edit' > ~/.sandbox/check/instances/n-collision/home/activation-check"
        )
    )
    recovery_output = machine.succeed(
        launcher("launch check --instance n-collision -- true") + " 2>&1"
    )
    assert "Home Manager activation failed; continuing in recovery mode" in recovery_output
    failure_log = machine.succeed(
        "journalctl -b -u sandbox-ualice-check@n-collision.service --no-pager"
    )
    assert "would be clobbered" in failure_log
    assert "Home Manager activation failed" in failure_log
    assert "activation timed out" not in failure_log
    machine.succeed("systemctl is-active --quiet sandbox-ualice-check@n-collision.service")
    machine.succeed("test -f /run/nook-ualice-check-n-collision/activation-failed")
    machine.succeed(
        as_owner(
            "grep -Fx 'keep my edit' ~/.sandbox/check/instances/n-collision/home/activation-check; "
            "test ! -e ~/.sandbox/check/instances/n-collision/home/activation-check.pre-systemd"
        )
    )
    cli(
        "launch check --instance n-collision -- "
        "nook-home-manager switch -b nook-backup"
    )
    machine.succeed(
        as_owner(
            "grep -Fx activated ~/.sandbox/check/instances/n-collision/home/activation-check; "
            "grep -Fx 'keep my edit' ~/.sandbox/check/instances/n-collision/home/activation-check.nook-backup"
        )
    )
    cli("stop check --instance n-collision")
    # A completed home skips activation on restart, but a different generation
    # must activate even when it uses the same persistent home. Removing the
    # success marker forces a retry, as after an interrupted activation.
    assert cli("launch cache-old -- uname -n").strip() == "cache-workstation"
    cli("stop cache-old")
    cli("launch cache-old -- true")
    cli("stop cache-old")
    cache_home = "/home/alice/.sandbox/cache-test/home"
    assert machine.succeed(f"cat {cache_home}/activation-history").splitlines() == [
        "cache-old"
    ]
    cli("launch cache-new -- true")
    cli("stop cache-new")
    assert machine.succeed(f"cat {cache_home}/cache-version").strip() == "cache-new"
    assert machine.succeed(f"cat {cache_home}/activation-history").splitlines() == [
        "cache-old",
        "cache-new",
    ]
    machine.succeed(as_owner(f"rm {cache_home}/.local/state/sandbox/home-generation"))
    cli("launch cache-new -- true")
    cli("stop cache-new")
    assert machine.succeed(f"cat {cache_home}/activation-history").splitlines() == [
        "cache-old",
        "cache-new",
        "cache-new",
    ]
    inside("grep -Fx nested-bind-works ~/Inbox/child/shared.txt")
    machine.succeed(
        "test $(stat -c %u /home/alice/.sandbox/check/home/Inbox) = 1000; "
        "test $(stat -c %u /home/alice/inbox-source/child) = 1000; "
        "test $(stat -c %u /home/alice/inbox-source/child/shared.txt) = 1000"
    )
    # The guest manages its own /etc/hosts rather than inheriting the host's.
    # Verify the override reaches a listener on this instance's loopback.
    machine.fail("grep -w project.test /etc/hosts")
    inside("""
        getent ahostsv4 project.test | grep -F 127.0.0.1
        python3 - <<'PY'
        from http.server import BaseHTTPRequestHandler, HTTPServer
        from threading import Thread
        from urllib.request import ProxyHandler, build_opener

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"guest loopback works")

        with HTTPServer(("127.0.0.1", 18763), Handler) as server:
            thread = Thread(target=server.serve_forever)
            thread.start()
            try:
                opener = build_opener(ProxyHandler({}))
                with opener.open("http://project.test:18763", timeout=5) as response:
                    assert response.read() == b"guest loopback works"
            finally:
                server.shutdown()
                thread.join()
        PY
    """)
    machine.succeed(
        "test -L /run/nook-ualice-check-main/profile-root; test $(readlink -f /run/nook-ualice-check-main/profile-root) = $(readlink -f /etc/sandbox/profiles/ualice/check)"
    )
    machine.succeed(
        "install -d -m700 -o alice -g users /home/alice/host-files /home/alice/host-files/directory"
    )
    machine.succeed(
        as_owner(
            "printf 'host selection 日本語\\n' > /home/alice/host-files/日本語.txt; printf 'selected directory\\n' > /home/alice/host-files/directory/child.txt"
        )
    )
    machine.succeed(
        as_owner("printf 'host temporary file\\n' > /tmp/portal-outside-home.txt")
    )
    inside("test ! -e ~/host-files/日本語.txt; test ! -e ~/host-files/directory")
    portal_result = inside(f"python3 {PORTAL_CLIENT}")
    assert "PipeWire FD passed" in portal_result
    assert "GTK host file dialog passed" in portal_result
    selected_document = next(
        line.removeprefix("PORTAL_OPENED_PATH=")
        for line in portal_result.splitlines()
        if line.startswith("PORTAL_OPENED_PATH=")
    )
    machine.succeed(
        as_owner(
            "grep -Fx 'saved through portal' /home/alice/host-files/保存.txt; grep -Fx 'updated through portal' /home/alice/host-files/日本語.txt"
        )
    )
    machine.succeed(
        as_owner(
            "grep -Fx eu.ambiso.sandbox.profile_check.main /home/alice/portal-appids"
        )
    )
    assert "/run/user/1000/doc/" in inside("timeout 20 sandbox-qt-portal-check")
    inside(
        "notify-send 'Sandbox notification'; xdg-open https://example.invalid/xdg-open-test"
    )
    machine.wait_until_succeeds(
        as_owner(
            "grep -Fx 'Sandbox notification' ~/portal-notifications; grep -Fx https://example.invalid/portal-test ~/portal-uris; grep -Fx https://example.invalid/xdg-open-test ~/portal-uris"
        )
    )
    inside(
        "fc-match 'sans-serif:lang=ja:charset=65e5' | grep 'Noto Sans CJK JP'; grep -F 'search sandbox.test' /etc/resolv.conf; getent ahostsv4 search-check | grep -F 192.0.2.73; test $(date +%Z) = CEST; command -v xdg-open rg ps which strace"
    )
    machine.succeed("mknod -m666 /dev/sandbox-hotplug-test c 1 3")
    machine.wait_until_succeeds(
        launcher("launch check -- bash -ec 'echo hotplug >/dev/sandbox-hotplug-test'")
    )
    machine.succeed("chmod 000 /dev/sandbox-hotplug-test")
    machine.wait_until_succeeds(
        launcher("launch check -- bash -ec 'test ! -e /dev/sandbox-hotplug-test'")
    )
    machine.succeed("chmod 666 /dev/sandbox-hotplug-test")
    machine.wait_until_succeeds(
        launcher("launch check -- bash -ec 'echo hotplug >/dev/sandbox-hotplug-test'")
    )
    machine.succeed("rm /dev/sandbox-hotplug-test")
    machine.wait_until_succeeds(
        launcher("launch check -- bash -ec 'test ! -e /dev/sandbox-hotplug-test'")
    )
    machine.succeed(
        as_owner(
            'TERM_PROGRAM=sandbox-test LC_TIME=ja_JP.UTF-8 sandbox launch check -- bash -ec \'test "${TERM_PROGRAM-unset}" = unset; test "${LC_TIME-unset}" = unset\''
        )
    )
    # Both protocols are bound on the host. The guest must have an independent
    # port space, rather than pasta binding forwarding sockets on its loopback.
    inside("""
        python3 - <<'PY'
        import socket

        try:
            with socket.create_connection(("127.0.0.1", 18761), timeout=1):
                raise AssertionError("host listener was mirrored into guest loopback")
        except ConnectionRefusedError:
            pass

        for family, address in [(socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")]:
            with socket.socket(family, socket.SOCK_STREAM) as tcp:
                tcp.bind((address, 18761))
                tcp.listen()
            with socket.socket(family, socket.SOCK_DGRAM) as udp:
                udp.bind((address, 18761))
        PY
    """)
    machine.succeed(
        "curl --fail --max-time 5 http://127.0.0.1:18761 | grep 'host loopback works'"
    )
    # The other pasta forwarding direction is disabled too: a listener opened
    # by an untrusted guest process must not silently become a host listener.
    inside(
        "systemd-run --user --unit=sandbox-network-listener "
        "python3 -m http.server 18762 --bind 0.0.0.0"
    )
    inside(
        "python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", 18762), timeout=2).close()'"
    )
    # Pasta discovers listeners periodically. Give the insecure automatic
    # publication behavior enough time to appear before checking the host.
    machine.succeed("sleep 2")
    machine.fail("curl --fail --max-time 2 http://127.0.0.1:18762")
    inside("systemctl --user stop sandbox-network-listener.service")
    machine.succeed(
        as_owner(
            "nix --extra-experimental-features nix-command store info --json "
            "| grep -Eq '\"trusted\"[[:space:]]*:[[:space:]]*true'"
        )
    )
    inside(
        "nix store info --json | grep -Eq '\"trusted\"[[:space:]]*:[[:space:]]*false'"
    )
    inside(
        f'result=$(timeout 30 nix build --option substitute false --no-link --print-out-paths --file {NIX_BUILD_CHECK}); grep daemon-proxy-works "$result"'
    )
    # Application profiles can use the filtered host bus directly, whereas
    # dev uses the private bus and bridge exercised above. Verify both paths.
    assert "direct host portal file grant passed" in cli(
        f"launch direct -- python3 {PORTAL_CLIENT} --open-only"
    )
    machine.succeed(
        as_owner("grep -Fx eu.ambiso.sandbox.profile_direct.main ~/portal-appids")
    )
    cli("stop direct")
    cli(
        "launch no-portals -- bash -ec 'test ! -e /run/host/portal-bus; test ! -e /run/host/doc; ! systemctl --user is-active --quiet sandbox-portals.service; ! dbus-send --session --print-reply --dest=org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.DBus.Peer.Ping'"
    )
    # Appearance follows the host without granting interactive portal access.
    assert "host dark theme active" in cli(
        "launch no-portals --join -- sandbox-qt-portal-check --appearance-only"
    )
    for address in ("/run/user/1000/bus", "/run/host/session-bus"):
        assert "u 1" in cli(
            f"launch no-portals --join -- busctl --address=unix:path={address} call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Settings ReadOne ss org.freedesktop.appearance color-scheme"
        )
        cli(
            f"launch no-portals --join -- bash -ec '! busctl --address=unix:path={address} call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.FileChooser OpenFile ssa{{sv}} empty empty 0'"
        )
    watcher = launcher(
        f"launch no-portals --join -- env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus python3 {APPEARANCE_CLIENT}"
    )
    machine.execute(f"{watcher} >/tmp/appearance-watcher.log 2>&1 &")
    machine.wait_until_succeeds(
        "grep -q 'appearance watcher ready' /tmp/appearance-watcher.log", timeout=30
    )
    machine.succeed(
        as_owner(
            "busctl --address=unix:path=/run/user/1000/bus call org.freedesktop.impl.portal.desktop.sandboxtest /org/freedesktop/portal/desktop org.example.SandboxTest SetColorScheme u 2"
        )
    )
    machine.wait_until_succeeds(
        "grep -q 'appearance change received' /tmp/appearance-watcher.log", timeout=30
    )
    machine.succeed(
        as_owner(
            "busctl --address=unix:path=/run/user/1000/bus call org.freedesktop.impl.portal.desktop.sandboxtest /org/freedesktop/portal/desktop org.example.SandboxTest SetColorScheme u 1"
        )
    )
    cli("stop no-portals")
    cli(
        "launch no-appearance -- bash -ec 'test ! -e /run/host/appearance-bus; ! systemctl --user is-active --quiet sandbox-appearance.service; ! busctl --user call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Settings ReadOne ss org.freedesktop.appearance color-scheme'"
    )
    cli("stop no-appearance")
    assert "activated" in inside("cat ~/activation-check")
    inside("systemctl --system is-active --quiet profile-module-check.service")
    machine.succeed(as_owner("test ! -x /run/nook-ualice-check-main/image"))
    machine.succeed("install -m600 /dev/null /home/alice/shared/host-root-secret")
    machine.succeed(
        "systemd-run --machine=sbx-1000-check-main --quiet --wait --pipe "
        f"--service-type=exec {COREUTILS}/bin/test ! -r /shared/host-root-secret"
    )
    machine.succeed("systemctl is-active --quiet sandbox-ualice-check@main.service")

    # Machined is socket-activated. Starting another sandbox after both units
    # were stopped must activate the socket first; starting the service directly
    # would make a later configuration switch fail to start its socket. The
    # daemon's lifetime must not own containers that are already running.
    invocation_before_machined_restart = machine.succeed(
        "systemctl show sandbox-ualice-check@main.service -p InvocationID --value"
    ).strip()
    machine.succeed("systemctl stop systemd-machined.socket systemd-machined.service")
    machine.succeed("systemctl is-active --quiet sandbox-ualice-check@main.service")
    cli("launch fallback -- true")
    machine.succeed("systemctl is-active --quiet systemd-machined.socket")
    machine.succeed("systemctl is-active --quiet systemd-machined.service")
    cli("stop fallback")
    assert (
        machine.succeed(
            "systemctl show sandbox-ualice-check@main.service -p InvocationID --value"
        ).strip()
        == invocation_before_machined_restart
    )
    inside("true", "--join")

    result = inside("""
        systemctl --user is-active --quiet postgres.service
        for i in {1..100}; do
            pg_isready -q -h "$XDG_RUNTIME_DIR" && break
            sleep .1
        done
        psql -h "$XDG_RUNTIME_DIR" -d postgres -Atc 'SELECT 42'
    """)
    assert "42" in result
    # GUI backend selection comes from the trusted guest profile even if the
    # long-lived user manager no longer carries its initial environment.
    inside(
        "systemctl --user unset-environment "
        "NIXOS_OZONE_WL QT_QPA_PLATFORM MOZ_ENABLE_WAYLAND XDG_SESSION_TYPE "
        "SSH_ASKPASS"
    )
    inside("""
        touch ~/persistent
        echo shared > /shared/marker
        test ! -e /tmp/.X11-unix/X77
        test "$DISPLAY" = :0
        test -z "${XAUTHORITY:-}"
        test "$NIXOS_OZONE_WL" = 1
        test "$QT_QPA_PLATFORM" = wayland
        test "$XDG_SESSION_TYPE" = wayland
        test -x "$SSH_ASKPASS"
        test "$MOZ_ENABLE_WAYLAND" = 1
        test "$SANDBOX_SHARED_MODULE" = applied
        test "$SANDBOX_PROFILE_IMPORT" = applied
        test -S /run/user/1000/bus
        test -w /dev/sandbox-test
        echo check >/dev/sandbox-test
        test ! -r /dev/kmsg
    """)

    # Real listeners exist on the host under both X11 addressing forms. A
    # missing filesystem path alone would not prove abstract-socket isolation.
    isolation_check = """
import socket
for address in ["/tmp/.X11-unix/X77", chr(0) + "/tmp/.X11-unix/X77"]:
    with socket.socket(socket.AF_UNIX) as client:
        try:
            client.connect(address)
        except (FileNotFoundError, ConnectionRefusedError):
            pass
        else:
            raise RuntimeError("host X11 socket is reachable")
"""
    inside("python3 -c " + shlex.quote(isolation_check))

    # GNOME resolves the upstream desktop ID, not the wrapper executable's
    # name. Icon data must also be present in the host profile's search path.
    desktop = "/etc/profiles/per-user/alice/share/applications/org.example.SandboxCheck.desktop"
    machine.succeed(f"desktop-file-validate {desktop}")
    machine.succeed(f"grep -Fx 'Name=Sandbox Icon Check (sandbox)' {desktop}")
    machine.succeed(f"grep -Fx 'DBusActivatable=false' {desktop}")
    machine.fail(f"grep '^X-Flatpak=' {desktop}")
    machine.fail(
        "test -e /etc/profiles/per-user/alice/share/applications/check-gui.desktop"
    )
    machine.succeed(
        "test -f /etc/profiles/per-user/alice/share/icons/hicolor/scalable/apps/org.example.SandboxCheck.svg"
    )
    fallback_desktop = (
        "/etc/profiles/per-user/alice/share/applications/fallback-gui.desktop"
    )
    machine.succeed(f"desktop-file-validate {fallback_desktop}")
    machine.succeed(
        "test -L /home/alice/.local/share/applications/org.example.SandboxCheck.desktop; "
        "test -L /home/alice/.local/share/applications/fallback-gui.desktop; "
        "test ! -e /home/alice/.local/share/applications/no-portals.desktop"
    )
    # GNOME uses GIO's application discovery/search. A syntactically valid
    # desktop file alone does not prove it is visible or searchable.
    application_search = """
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio
for desktop_id, query in [
    ("org.example.SandboxCheck.desktop", "Sandbox Icon Check"),
    ("fallback-gui.desktop", "fallback-gui"),
]:
    app = Gio.DesktopAppInfo.new(desktop_id)
    assert app is not None, desktop_id
    assert app.get_filename().startswith("/home/alice/.local/share/applications/"), app.get_filename()
    assert app.should_show(), desktop_id
    assert "nook-launchers/bin/" in app.get_executable(), app.get_executable()
    assert any(desktop_id in group for group in Gio.DesktopAppInfo.search(query)), query
"""
    machine.succeed(
        as_owner(
            f"XDG_CURRENT_DESKTOP=GNOME {DESKTOP_PYTHON} -c "
            + shlex.quote(application_search)
        )
    )
    assert "Usage" in cli("gui check -- --help 2>&1")
    assert "fallback GUI started" in cli("gui fallback")
    cli("stop fallback")
    inside("test ! -e /.flatpak-info")

    # Exercise the actual TUI in a pseudo-terminal and quit without launching.
    machine.succeed(
        "(sleep 1; printf q) | "
        + as_owner(
            "TERM=xterm-256color timeout 10 script --quiet --return --command "
            "'stty rows 24 cols 100; nk' /tmp/sandbox-tui.log"
        )
    )
    machine.succeed("grep -q Nook /tmp/sandbox-tui.log")

    # Create a named instance through the real TUI, then leave its shell. The
    # home and service must outlive the terminal, and remain selectable stopped.
    machine.succeed(
        "mkdir -p /tmp/sandbox-tui-profiles; "
        "ln -s /etc/sandbox/profiles/ualice/check /tmp/sandbox-tui-profiles/check"
    )
    machine.succeed(
        "(sleep 1; printf 'nfrom-tui\\r'; sleep 2; "
        "printf 'touch ~/tui-created; exit\\r'; "
        "for tui_attempt in $(seq 1 200); do "
        "test -e /home/alice/.sandbox/check/instances/n-from-tui/home/tui-created "
        "&& break; sleep 0.1; done) | "
        + as_owner(
            "TERM=xterm-256color timeout 60 script --quiet --return --command "
            "'stty rows 24 cols 100; nk --profiles /tmp/sandbox-tui-profiles' "
            "/tmp/sandbox-tui-create.log"
        )
    )
    machine.succeed(
        "test -e /home/alice/.sandbox/check/instances/n-from-tui/home/tui-created"
    )
    assert "check\tn-from-tui\trunning" in cli("list")
    cli("stop check --name from-tui")
    assert "check\tn-from-tui\tstopped" in cli("list")
    inside("test -e ~/tui-created", "--name from-tui")
    machine.fail(launcher("delete check --name from-tui --yes"))
    cli("stop check --name from-tui")
    machine.fail(launcher("delete check --name from-tui </dev/null"))
    cli("delete check --name from-tui --yes")
    assert "check\tn-from-tui\t" not in cli("list")
    machine.succeed("test ! -e /home/alice/.sandbox/check/instances/n-from-tui")
    machine.succeed(f"grep -Fx guest-output {shared_storage}/guest-marker")

    # A template's non-persistent main home must not make named homes temporary.
    cli("launch fallback --name project -- bash -ec 'touch ~/saved'")
    cli("stop fallback --name project")
    cli("launch fallback --name project -- bash -ec 'test -e ~/saved'")
    cli("stop fallback --name project")

    # Another user cannot start an owner's unit or use its privileged entry.
    machine.fail(
        "su - bob -c 'systemctl --no-ask-password start sandbox-ualice-check@n-foreign.service'"
    )

    # Named homes persist independently. Ephemeral homes are clean and are
    # deleted when stopped, while explicitly shared host mounts remain shared.
    inside(
        """
        test ! -e ~/persistent
        test ! -e ~/build/main-output
        test ! -e ~/.cache/sccache/main-cache
        mkdir -p ~/build ~/.cache/sccache
        touch ~/named ~/build/named-output ~/.cache/sccache/named-cache
    """,
        "--name review",
    )
    inside("test ! -e ~/build/named-output; test ! -e ~/.cache/sccache/named-cache")
    assert inside("uname -n", "--name review").strip() == "check-review"
    inside("getent hosts check-review", "--name review")
    assert (
        inside("cat /etc/machine-id", "--instance n-review").strip() != first_machine_id
    )
    inside("test ! -e " + shlex.quote(selected_document), "--instance n-review")
    cli("stop check --instance n-review")
    assert "check\tn-review\tstopped" in cli("list")
    inside(
        """
        test -e ~/named
        test ! -e ~/persistent
        test -e ~/build/named-output
        test -e ~/.cache/sccache/named-cache
        test ! -e ~/build/main-output
        test ! -e ~/.cache/sccache/main-cache
    """,
        "--instance n-review",
    )
    cli("stop check --instance n-review")
    inside(
        "test ! -e ~/persistent; test -e /shared/marker; "
        "test ! -e ~/build/main-output; test ! -e ~/build/named-output; "
        "test ! -e ~/.cache/sccache/main-cache; test ! -e ~/.cache/sccache/named-cache; "
        "mkdir -p ~/build ~/.cache/sccache; "
        "touch ~/temporary ~/build/temporary-output ~/.cache/sccache/temporary-cache; test ! -e "
        + shlex.quote(selected_document),
        "--ephemeral",
    )
    inside(
        "test ! -e ~/build/temporary-output; test ! -e ~/.cache/sccache/temporary-cache"
    )
    ephemeral = next(
        line.split()[1]
        for line in cli("list").splitlines()
        if line.split()[1].startswith("e-")
    )
    assert inside("uname -n", "--instance " + ephemeral).strip() == "check-" + ephemeral
    cli("stop check --instance " + ephemeral)
    machine.fail("test -e /run/nook-ualice-check-" + ephemeral)
    inside(
        """
        test ! -e ~/temporary
        test ! -e ~/persistent
        test ! -e ~/build/temporary-output
        test ! -e ~/.cache/sccache/temporary-cache
    """,
        "--instance " + ephemeral,
    )
    cli("stop check --instance " + ephemeral)

    # Stopping must halt PostgreSQL cleanly, including joined clients. A later
    # boot must reactivate the same persistent home and retain its UID mapping.
    sleeper = launcher("launch check -- bash -c 'touch ~/joiner-ready; exec sleep 300'")
    machine.execute(f"{sleeper} >/tmp/joiner-log 2>&1 & echo $! >/tmp/joiner-pid")
    machine.wait_until_succeeds("test -e /home/alice/.sandbox/check/home/joiner-ready")
    cli("stop check")
    machine.succeed("test ! -e /home/alice/.sandbox/check/home/pgdata/postmaster.pid")
    inside("""
        test -e ~/persistent
        test ! -L ~/.ssh/config
        test $(stat -c %u ~/.ssh/config) = 1000
        ssh -G example.invalid | grep -Fx 'hostname example.invalid'
        systemctl --user is-active --quiet postgres.service
        test $(wc -l < /proc/self/uid_map) = 1
        test $(awk '{print $2}' /proc/self/uid_map) -gt 0
    """)
    inside("""
        wayland-info | grep wl_compositor
        xdpyinfo >/dev/null
    """)
    inside("""
        busctl --user status >/dev/null
        dbus-send --session --type=method_call --print-reply \
            --dest=org.freedesktop.DBus /org/freedesktop/DBus \
            org.freedesktop.DBus.GetId >/dev/null
    """)

    # Steam also creates nested namespaces through its Nixpkgs FHS wrapper.
    inside("sandbox-test-fhs -ec 'test -x /bin/bash; test -r /proc/self/status'")

    # A real image with nonzero file ownership exercises more than podman info:
    # import/chown, subordinate UID/GID mappings, and an enforced memory limit.
    inside(f"""
        image=$(podman import --signature-policy {POLICY} {IMAGE} sandbox-uid-check)
        podman run --signature-policy {POLICY} --rm --network=none \
            --user=123:456 --memory=64m --pids-limit=32 \
            -v /nix/store:/nix/store:ro "$image" /bin/sh -ec '
                IFS= read -r limit < /sys/fs/cgroup/memory.max
                test "$limit" = 67108864
                test $({COREUTILS}/bin/stat -c %u /bin/sh) = 123
            '
        podman info --format '{{{{.Host.CgroupManager}}}}' | grep -x systemd
        podman unshare cat /proc/self/uid_map
    """)
    inside("timeout 5 pw-cli info 0; timeout 5 pactl info")

    # A compatible host switch reloads the guest in place. The supervisor process
    # stays intact while the pinned profile advances to the new generation.
    unit = "sandbox-ualice-check@main.service"
    invocation = machine.succeed(
        f"systemctl show {unit} -p InvocationID --value"
    ).strip()
    old_profile = machine.succeed(
        "readlink -f /run/nook-ualice-check-main/profile-root"
    ).strip()
    old_shell = next(
        line.split("\t", 2)[2]
        for line in machine.succeed(f"cat {old_profile}/profile").splitlines()
        if line.startswith("shell\t")
    )
    cli("launch fallback -- true")
    incompatible_unit = "sandbox-ualice-fallback@main.service"
    incompatible_invocation = machine.succeed(
        f"systemctl show {incompatible_unit} -p InvocationID --value"
    ).strip()
    incompatible_profile = machine.succeed(
        "readlink -f /run/nook-ualice-fallback-main/profile-root"
    ).strip()
    cli("launch no-portals -- true")
    cli("stop no-portals")
    machine.fail("systemctl is-active --quiet sandbox-ualice-no-portals@main.service")
    assert "\t" in machine.succeed(f"cat {old_profile}/profile").split("activation\t", 1)[1]
    assert "\t" in machine.succeed(f"cat {old_profile}/profile").split("system\t", 1)[1]
    machine.succeed(f"{UPDATED_SYSTEM}/bin/switch-to-configuration test", timeout=120)
    installed_profile = machine.succeed(
        "readlink -f /etc/sandbox/profiles/ualice/check"
    ).strip()
    assert (
        machine.succeed(f"systemctl show {unit} -p InvocationID --value").strip()
        == invocation
    )
    assert (
        machine.succeed("readlink -f /run/nook-ualice-check-main/profile-root").strip()
        == installed_profile
    )
    assert (
        machine.succeed(
            f"systemctl show {incompatible_unit} -p InvocationID --value"
        ).strip()
        == incompatible_invocation
    )
    assert (
        machine.succeed(
            "readlink -f /run/nook-ualice-fallback-main/profile-root"
        ).strip()
        == incompatible_profile
    )
    machine.succeed(
        f"journalctl -b -u {incompatible_unit} --no-pager | "
        "grep -F 'reload skipped for main: restart required: '"
    )
    cli("stop fallback")
    assert (
        machine.succeed(
            "systemctl --machine=sbx-1000-check-main is-active --quiet "
            "sandbox-ready.service"
        ).strip()
        == ""
    )
    new_system = next(
        line.split("\t", 2)[2]
        for line in machine.succeed(f"cat {installed_profile}/profile").splitlines()
        if line.startswith("system\t")
    )
    assert machine.succeed(
        "systemd-run --machine=sbx-1000-check-main --quiet --wait --pipe "
        "/run/current-system/sw/bin/readlink -f /run/current-system"
    ).strip() == new_system
    assert inside("printf '%s' \"$NOOK_TEST_REVISION\"").strip() == "updated"
    assert inside("cat ~/reload-version").strip() == "updated"
    assert inside("hello").strip() == "Hello, world!"
    assert (
        cli("shell check -- -c " + shlex.quote("printf '%s' \"$0\"")).strip() != old_shell
    )
    assert (
        cli("shell check -- -c " + shlex.quote("printf '%s' \"$SHELL\"")).strip() != old_shell
    )

    # An explicit stop/start picks up the newly installed profile.
    cli("stop check")
    assert inside("printf '%s' \"$NOOK_TEST_REVISION\"").strip() == "updated"
    cli("stop check")
    machine.succeed(f"{BASE_SYSTEM}/bin/switch-to-configuration test", timeout=120)
except Exception:
    machine.execute("journalctl -b --no-pager -u sandbox-ualice-check@main.service")
    raise
