{ pkgs, home-manager }:
let
  hostUid = 1002;
  guestUid = 1000;
  nook = pkgs.callPackage ../. { };
  portalPython = pkgs.python3.withPackages (p: [
    p.dbus-python
    p.pygobject3
  ]);
  fakeDocuments = pkgs.writeText "nook-test-documents-portal.py" ''
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName("org.freedesktop.portal.Documents", bus)

    class Documents(dbus.service.Object):
        @dbus.service.method("org.freedesktop.portal.Documents", out_signature="ay")
        def GetMountPoint(self):
            return dbus.ByteArray(b"/host-documents\0")

    Documents(bus, "/org/freedesktop/portal/documents")
    GLib.MainLoop().run()
  '';
in
pkgs.testers.runNixOSTest {
  name = "nook-proxy-client-uid";
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
        guest.uid = guestUid;
        sandboxes.dev = {
          shell = pkgs.bashInteractive;
          gpu = false;
          audio = false;
          network = false;
          portals = false;
          inheritAppearance = false;
          hostSessionBus = true;
          sessionBusPolicies."org.freedesktop.portal.Documents" = "talk";
        };
      };
    };
    # Exercise Nook as a client of the filtered host bus without requiring a
    # graphical desktop or the document portal's FUSE mount.
    systemd.user.services.nook-test-documents-portal = {
      wantedBy = [ "default.target" ];
      after = [ "dbus.socket" ];
      environment.DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/${toString hostUid}/bus";
      serviceConfig = {
        Type = "dbus";
        BusName = "org.freedesktop.portal.Documents";
        ExecStart = "${portalPython}/bin/python3 ${fakeDocuments}";
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
        "systemctl --user is-active --quiet nook-test-documents-portal.service'"
    )
    assert machine.succeed("id -u alice").strip() == "${toString hostUid}"
    assert machine.succeed("su - alice -c 'nk launch dev -- id -u'").strip() == "${toString guestUid}"
    # sd-bus reaches the host service through the proxy on all three revisions.
    # This keeps a proxy setup failure separate from libdbus authentication.
    machine.succeed(
        "su - alice -c 'nk launch dev -- busctl "
        "--address=unix:path=/run/host/session-bus call "
        "org.freedesktop.portal.Documents /org/freedesktop/portal/documents "
        "org.freedesktop.portal.Documents GetMountPoint'"
    )
    # This particular client links Nook's libdbus and inherits the environment
    # supplied to a command by nk launch. Main sends the guest UID and fails;
    # pr/2's host UID and the peer-credentials fix both authenticate.
    result = machine.succeed(
        "su - alice -c 'nk launch dev -- ${nook}/bin/nk --document-mountpoint'"
    )
    assert result.strip() == "/host-documents", result
  '';
}
