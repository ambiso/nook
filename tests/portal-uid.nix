{
  pkgs,
  home-manager,
}:
let
  hostUid = 1002;
  portalPython = pkgs.python3.withPackages (p: [
    p.dbus-python
    p.pygobject3
  ]);
  fakePortal = pkgs.writeText "nook-test-appearance-portal.py" ''
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus)

    class Settings(dbus.service.Object):
        @dbus.service.method(
            "org.freedesktop.portal.Settings", in_signature="ss", out_signature="v"
        )
        def ReadOne(self, namespace, key):
            assert (namespace, key) == (
                "org.freedesktop.appearance", "color-scheme"
            )
            return dbus.UInt32(1)

    Settings(bus, "/org/freedesktop/portal/desktop")
    GLib.MainLoop().run()
  '';
in
pkgs.testers.runNixOSTest {
  name = "nook-portal-uid-default";
  nodes.machine = { ... }: {
    imports = [ (import ../nix/nixos.nix { inherit home-manager; }) ];
    nixpkgs.pkgs = pkgs.lib.mkForce pkgs;
    system.stateVersion = "26.05";
    services.dbus.enable = true;
    users.users.alice = {
      isNormalUser = true;
      uid = hostUid;
      linger = true;
    };
    home-manager.useGlobalPkgs = true;
    home-manager.useUserPackages = true;
    home-manager.users.alice = {
      home.stateVersion = "26.05";
      programs.nook = {
        enable = true;
        sandboxes.dev = {
          shell = pkgs.bashInteractive;
          gpu = false;
          audio = false;
          network = false;
          hostSessionBus = false;
          portals = false;
          inheritAppearance = true;
        };
      };
    };
    # The host service answers one Settings call, without a display server or
    # the full desktop portal stack. The D-Bus proxy and guest bridge are real.
    systemd.user.services.nook-test-appearance-portal = {
      wantedBy = [ "default.target" ];
      after = [ "dbus.socket" ];
      environment.DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString hostUid}/bus";
      serviceConfig = {
        Type = "dbus";
        BusName = "org.freedesktop.portal.Desktop";
        ExecStart = "${portalPython}/bin/python3 ${fakePortal}";
      };
      unitConfig.ConditionUser = "alice";
    };
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("home-manager-alice.service")
    machine.wait_for_unit("user@${toString hostUid}.service")
    machine.wait_until_succeeds(
        "su - alice -c 'XDG_RUNTIME_DIR=/run/user/${toString hostUid} "
        "systemctl --user is-active --quiet nook-test-appearance-portal.service'"
    )
    assert machine.succeed("id -u alice").strip() == "${toString hostUid}"
    assert machine.succeed("su - alice -c 'nk launch dev -- id -u'").strip() == "${toString hostUid}"
    # sd-bus can reach the host service through the filtered proxy. This rules
    # out a missing host service or proxy socket.
    direct = machine.succeed(
        "su - alice -c 'nk launch dev -- busctl "
        "--address=unix:path=/run/host/appearance-bus call "
        "org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop "
        "org.freedesktop.portal.Settings ReadOne ss "
        "org.freedesktop.appearance color-scheme'"
    )
    assert "1" in direct, direct
    # A libdbus client must be able to authenticate to the guest session bus
    # before the bridge call can meaningfully test the upstream connection.
    machine.succeed(
        "su - alice -c 'nk launch dev -- dbus-send --session --print-reply "
        "--reply-timeout=5000 --dest=org.freedesktop.DBus "
        "/org/freedesktop/DBus org.freedesktop.DBus.ListNames'"
    )
    # dbus-send uses the guest's ordinary libdbus. Nook's bridge first
    # authenticates to the guest bus, then to the host's filtered proxy.
    # With no guest UID override, the guest uses the host's declared 1002.
    reply = machine.succeed(
        "su - alice -c 'nk launch dev -- dbus-send --session --print-reply "
        "--reply-timeout=5000 --dest=org.freedesktop.portal.Desktop "
        "/org/freedesktop/portal/desktop "
        "org.freedesktop.portal.Settings.ReadOne "
        "string:org.freedesktop.appearance string:color-scheme'"
    )
    assert "uint32 1" in reply, reply
  '';
}
