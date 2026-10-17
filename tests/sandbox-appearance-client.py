"""Check that the restricted appearance connection follows host changes."""

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
loop = GLib.MainLoop()
received = []


def changed(namespace, key, value):
    if namespace == "org.freedesktop.appearance" and key == "color-scheme":
        received.append(int(value))
        loop.quit()


bus.add_signal_receiver(
    changed,
    "SettingChanged",
    "org.freedesktop.portal.Settings",
    "org.freedesktop.portal.Desktop",
    "/org/freedesktop/portal/desktop",
)
settings = dbus.Interface(
    bus.get_object(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        introspect=False,
    ),
    "org.freedesktop.portal.Settings",
)
assert int(settings.ReadOne("org.freedesktop.appearance", "color-scheme")) == 1
print("appearance watcher ready", flush=True)
GLib.timeout_add_seconds(20, lambda: (loop.quit(), False)[1])
loop.run()
assert received == [2], received
print("appearance change received", flush=True)
