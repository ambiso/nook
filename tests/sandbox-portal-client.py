"""Exercise host-selected files and a real PipeWire FD through the bridge."""

import argparse
import itertools
import os
from pathlib import Path
import socket
import stat
from urllib.parse import unquote, urlparse

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument(
    "--open-only",
    action="store_true",
    help="Check a host file grant without changing files",
)
arguments = parser.parse_args()

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
desktop = bus.get_object(
    "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop"
)
tokens = itertools.count()


def request(interface, method, *arguments, **options):
    token = f"test_{next(tokens)}"
    expected = (
        "/org/freedesktop/portal/desktop/request/"
        + bus.get_unique_name()[1:].replace(".", "_")
        + "/"
        + token
    )
    response = []
    loop = GLib.MainLoop()

    def receive(code, results):
        response.append((int(code), results))
        loop.quit()

    # Subscribe before calling, as real toolkits do. This catches races and
    # incorrectly translated sender components in request object paths.
    match = bus.add_signal_receiver(
        receive,
        "Response",
        "org.freedesktop.portal.Request",
        "org.freedesktop.portal.Desktop",
        expected,
    )
    options["handle_token"] = token
    handle = desktop.get_dbus_method(method, interface)(
        *arguments, dbus.Dictionary(options, signature="sv")
    )
    assert handle == expected, (handle, expected)
    timeout = GLib.timeout_add_seconds(20, lambda: (loop.quit(), False)[1])
    if not response:
        loop.run()
    GLib.source_remove(timeout)
    match.remove()
    assert response, "portal request timed out"
    return response[0]


def chosen_path(results):
    path = Path(unquote(urlparse(str(results["uris"][0])).path))
    assert path.is_relative_to(Path(os.environ["XDG_RUNTIME_DIR"]) / "doc"), path
    return path


settings = dbus.Interface(desktop, "org.freedesktop.portal.Settings")
assert int(settings.Read("org.freedesktop.appearance", "color-scheme")) == 1
try:
    settings.Read("invalid", "invalid")
except dbus.exceptions.DBusException as error:
    assert error.get_dbus_name() == "org.freedesktop.portal.Error.NotFound"
else:
    raise AssertionError("portal error reply was lost")

chooser = "org.freedesktop.portal.FileChooser"
code, result = request(chooser, "OpenFile", "", "temporary")
assert code == 0
assert chosen_path(result).read_text() == "host temporary file\n"
if arguments.open_only:
    print("direct host portal file grant passed")
    raise SystemExit(0)
code, result = request(chooser, "OpenFile", "", "open")
assert code == 0
opened = chosen_path(result)
print("PORTAL_OPENED_PATH=" + str(opened))
assert opened.read_text() == "host selection 日本語\n"
try:
    opened.write_text("unexpected write")
except PermissionError:
    pass
else:
    raise AssertionError("read-only portal grant allowed writing")

code, result = request(chooser, "OpenFile", "", "writable")
assert code == 0
chosen_path(result).write_text("updated through portal\n")

code, result = request(chooser, "SaveFile", "", "save")
assert code == 0
chosen_path(result).write_text("saved through portal\n")

code, result = request(
    chooser, "OpenFile", "", "directory", directory=dbus.Boolean(True)
)
assert code == 0
assert (chosen_path(result) / "child.txt").read_text() == "selected directory\n"
assert request(chooser, "OpenFile", "", "cancel")[0] == 1
assert (
    request(
        "org.freedesktop.portal.OpenURI",
        "OpenURI",
        "",
        "https://example.invalid/portal-test",
    )[0]
    == 0
)

screen_cast = "org.freedesktop.portal.ScreenCast"
code, result = request(screen_cast, "CreateSession", session_handle_token="capture")
assert code == 0
session = result["session_handle"]
assert "/session/" + bus.get_unique_name()[1:].replace(".", "_") + "/" in session
assert (
    request(
        screen_cast, "SelectSources", dbus.ObjectPath(session), types=dbus.UInt32(1)
    )[0]
    == 0
)
assert request(screen_cast, "Start", dbus.ObjectPath(session), "")[0] == 0
descriptor = desktop.get_dbus_method("OpenPipeWireRemote", screen_cast)(
    dbus.ObjectPath(session), dbus.Dictionary({}, signature="sv")
).take()
assert stat.S_ISSOCK(os.fstat(descriptor).st_mode)
with socket.socket(fileno=descriptor) as pipewire:
    assert pipewire.getsockopt(socket.SOL_SOCKET, socket.SO_TYPE) == socket.SOCK_STREAM
dbus.Interface(
    bus.get_object("org.freedesktop.portal.Desktop", session),
    "org.freedesktop.portal.Session",
).Close()
print("portal files, permissions, sessions and PipeWire FD passed")

# Exercise toolkit selection too: a working D-Bus API is insufficient if a
# native GTK application silently falls back to a sandbox-local file dialog.
import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk

Gtk.init()
loop = GLib.MainLoop()
selected = []


def gtk_selected(dialog, result):
    selected.append(dialog.open_finish(result).get_path())
    loop.quit()


dialog = Gtk.FileDialog(title="open")
dialog.open(None, None, gtk_selected)
timeout = GLib.timeout_add_seconds(20, lambda: (loop.quit(), False)[1])
loop.run()
GLib.source_remove(timeout)
assert selected, "GTK did not use the host portal"
assert Path(selected[0]).is_relative_to(Path(os.environ["XDG_RUNTIME_DIR"]) / "doc")
assert Path(selected[0]).read_text() == "updated through portal\n"
print("GTK host file dialog passed")
