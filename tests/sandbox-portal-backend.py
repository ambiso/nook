"""Deterministic consent UI for tests of the real host portal frontend.

Only the desktop backend is substituted. App identification, request handling,
document export/FUSE permissions, and PipeWire FD creation are production code.
"""

import json
import subprocess
import time
from pathlib import Path

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName("org.freedesktop.impl.portal.desktop.sandboxtest", bus)
objects = []


def record(app_id):
    with open("/home/alice/portal-appids", "a") as output:
        output.write(str(app_id) + "\n")


class Session(dbus.service.Object):
    @dbus.service.method("org.freedesktop.impl.portal.Session")
    def Close(self):
        self.remove_from_connection()


class Backend(dbus.service.Object):
    color_scheme = 1

    @dbus.service.signal("org.freedesktop.impl.portal.Settings", signature="ssv")
    def SettingChanged(self, namespace, key, value):
        pass

    @dbus.service.method("org.example.SandboxTest", in_signature="u")
    def SetColorScheme(self, value):
        self.color_scheme = int(value)
        self.SettingChanged(
            "org.freedesktop.appearance", "color-scheme", dbus.UInt32(value)
        )

    @dbus.service.method(
        "org.freedesktop.DBus.Properties", in_signature="s", out_signature="a{sv}"
    )
    def GetAll(self, interface):
        if interface == "org.freedesktop.impl.portal.Notification":
            return {
                "version": dbus.UInt32(2),
                "SupportedOptions": dbus.Dictionary({}, signature="sv"),
            }
        if interface == "org.freedesktop.impl.portal.ScreenCast":
            return {
                "version": dbus.UInt32(5),
                "AvailableSourceTypes": dbus.UInt32(1),
                "AvailableCursorModes": dbus.UInt32(1),
            }
        return {"version": dbus.UInt32(1)}

    @dbus.service.method(
        "org.freedesktop.DBus.Properties", in_signature="ss", out_signature="v"
    )
    def Get(self, interface, key):
        return self.GetAll(interface)[key]

    @dbus.service.method(
        "org.freedesktop.impl.portal.Settings",
        in_signature="as",
        out_signature="a{sa{sv}}",
    )
    def ReadAll(self, namespaces):
        return {
            "org.freedesktop.appearance": {
                "color-scheme": dbus.UInt32(self.color_scheme)
            }
        }

    @dbus.service.method(
        "org.freedesktop.impl.portal.Settings", in_signature="ss", out_signature="v"
    )
    def Read(self, namespace, key):
        if namespace != "org.freedesktop.appearance" or key != "color-scheme":
            raise dbus.exceptions.DBusException(
                "No such setting", name="org.freedesktop.portal.Error.NotFound"
            )
        return dbus.UInt32(self.color_scheme)

    @dbus.service.method(
        "org.freedesktop.impl.portal.AppChooser",
        in_signature="ossasa{sv}",
        out_signature="ua{sv}",
    )
    def ChooseApplication(self, handle, app_id, parent, choices, options):
        record(app_id)
        return 0, {"choice": "sandbox-url-test"}

    @dbus.service.method(
        "org.freedesktop.impl.portal.Notification", in_signature="ssa{sv}"
    )
    def AddNotification(self, app_id, notification_id, notification):
        record(app_id)
        Path("/home/alice/portal-notifications").write_text(
            str(notification["title"]) + "\n"
        )

    @dbus.service.method("org.freedesktop.impl.portal.Notification", in_signature="ss")
    def RemoveNotification(self, app_id, notification_id):
        pass

    @dbus.service.method(
        "org.freedesktop.impl.portal.FileChooser",
        in_signature="osssa{sv}",
        out_signature="ua{sv}",
    )
    def OpenFile(self, handle, app_id, parent, title, options):
        record(app_id)
        if title == "qt-open":
            # Qt subscribes only after its OpenFile reply, unlike GTK. Model
            # the consent dialog's human delay for that client; other tests
            # keep immediate responses to exercise subscription races.
            time.sleep(0.2)
        if title == "cancel":
            return 1, dbus.Dictionary({}, signature="sv")
        selected = Path("/home/alice/host-files/日本語.txt")
        if title == "temporary":
            selected = Path("/tmp/portal-outside-home.txt")
        if options.get("directory", False):
            selected = Path("/home/alice/host-files/directory")
        return 0, {
            "uris": dbus.Array([selected.as_uri()], signature="s"),
            "writable": dbus.Boolean(title == "writable"),
        }

    @dbus.service.method(
        "org.freedesktop.impl.portal.FileChooser",
        in_signature="osssa{sv}",
        out_signature="ua{sv}",
    )
    def SaveFile(self, handle, app_id, parent, title, options):
        record(app_id)
        return 0, {
            "uris": dbus.Array(
                [Path("/home/alice/host-files/保存.txt").as_uri()], signature="s"
            )
        }

    @dbus.service.method(
        "org.freedesktop.impl.portal.ScreenCast",
        in_signature="oosa{sv}",
        out_signature="ua{sv}",
    )
    def CreateSession(self, handle, session_handle, app_id, options):
        record(app_id)
        objects.append(Session(bus, session_handle))
        return 0, dbus.Dictionary({}, signature="sv")

    @dbus.service.method(
        "org.freedesktop.impl.portal.ScreenCast",
        in_signature="oosa{sv}",
        out_signature="ua{sv}",
    )
    def SelectSources(self, handle, session_handle, app_id, options):
        return 0, dbus.Dictionary({}, signature="sv")

    @dbus.service.method(
        "org.freedesktop.impl.portal.ScreenCast",
        in_signature="oossa{sv}",
        out_signature="ua{sv}",
    )
    def Start(self, handle, session_handle, app_id, parent, options):
        nodes = json.loads(subprocess.check_output(["pw-dump"]))
        node_id = next(
            node["id"]
            for node in nodes
            if node.get("info", {}).get("props", {}).get("node.name")
            == "portal-test-source"
        )
        return 0, {
            "streams": dbus.Array(
                [
                    dbus.Struct(
                        (
                            dbus.UInt32(node_id),
                            dbus.Dictionary(
                                {"source_type": dbus.UInt32(1)}, signature="sv"
                            ),
                        )
                    )
                ],
                signature="(ua{sv})",
            )
        }


backend = Backend(bus, "/org/freedesktop/portal/desktop")
GLib.MainLoop().run()
