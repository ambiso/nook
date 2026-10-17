# Run this check inside the existing dev sandbox: nix build .#checks.x86_64-linux.integration
{
  pkgs,
  home-manager,
  benchmark ? false,
  diagnostics ? false,
}:
let
  cacheProfile =
    { name, ... }:
    {
      gpu = false;
      audio = false;
      network = false;
      stateDirectory = ".sandbox/cache-test";
      homeManagerModules = [
        ({ lib, ... }: {
          home.file.cache-version.text = "${name}\n";
          home.activation.countActivations = lib.hm.dag.entryAfter [ "writeBoundary" ] ''
            echo ${pkgs.lib.escapeShellArg name} >> "$HOME/activation-history"
          '';
        })
      ];
    };
  portalPython = pkgs.python3.withPackages (p: [
    p.dbus-python
    p.pygobject3
  ]);
  testPortal = pkgs.runCommand "sandbox-test-portal" { } ''
    mkdir -p "$out/share/xdg-desktop-portal/portals"
    cat > "$out/share/xdg-desktop-portal/portals/sandboxtest.portal" <<'PORTAL'
    [portal]
    DBusName=org.freedesktop.impl.portal.desktop.sandboxtest
    Interfaces=org.freedesktop.impl.portal.FileChooser;org.freedesktop.impl.portal.Settings;org.freedesktop.impl.portal.ScreenCast;org.freedesktop.impl.portal.AppChooser;org.freedesktop.impl.portal.Notification;
    PORTAL
  '';
  urlHandler = pkgs.makeDesktopItem {
    name = "sandbox-url-test";
    desktopName = "Sandbox URL test";
    noDisplay = true;
    exec = "${pkgs.writeShellScript "sandbox-record-uri" ''
      printf '%s\n' "$@" >> "$HOME/portal-uris"
    ''} %u";
    mimeTypes = [
      "x-scheme-handler/https"
      "x-scheme-handler/http"
    ];
  };
  qtPortalCheck =
    pkgs.runCommand "sandbox-qt-portal-check"
      {
        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.qt6.wrapQtAppsHook
        ];
        buildInputs = [
          pkgs.qt6.qtbase
          pkgs.qt6.qtwayland
        ];
      }
      ''
        mkdir -p "$out/bin"
        ${pkgs.stdenv.cc}/bin/c++ -std=c++17 ${pkgs.writeText "sandbox-qt-portal-check.cpp" ''
          #include <QApplication>
          #include <QFile>
          #include <QFileDialog>
          #include <QStyleHints>
          #include <QTimer>
          #include <iostream>
          int main(int argc, char **argv) {
              QApplication app(argc, argv);
              if (app.arguments().contains("--appearance-only")) {
                  if (app.styleHints()->colorScheme() != Qt::ColorScheme::Dark)
                      return 3;
                  std::cout << "host dark theme active" << std::endl;
                  return 0;
              }
              // The theme queries portal capabilities asynchronously. Model
              // a user opening a dialog after the application's event loop starts.
              QTimer::singleShot(500, [&app] {
                  QFile file(QFileDialog::getOpenFileName(nullptr, "qt-open"));
                  if (!file.open(QIODevice::ReadOnly)) { app.exit(1); return; }
                  if (file.readAll() != "updated through portal\n") { app.exit(2); return; }
                  std::cout << file.fileName().toStdString() << std::endl;
                  app.quit();
              });
              return app.exec();
          }
        ''} $(pkg-config --cflags --libs Qt6Widgets) -o "$out/bin/sandbox-qt-portal-check"
        wrapQtApp "$out/bin/sandbox-qt-portal-check"
      '';
  fhsRuntime = pkgs.buildFHSEnv {
    name = "sandbox-test-fhs";
    targetPkgs = packages: [
      packages.bash
      packages.coreutils
    ];
    runScript = "bash";
  };
  desktopApp = pkgs.runCommand "sandbox-desktop-test" { } ''
    mkdir -p "$out/bin" "$out/share/applications" "$out/share/icons/hicolor/scalable/apps"
    ln -s ${pkgs.wayland-utils}/bin/wayland-info "$out/bin/check-gui"
    cat > "$out/share/applications/org.example.SandboxCheck.desktop" <<'DESKTOP'
    [Desktop Entry]
    Type=Application
    Name=Sandbox Icon Check
    Exec=check-gui %U
    TryExec=check-gui
    Icon=org.example.SandboxCheck
    StartupWMClass=org.example.SandboxCheck
    DBusActivatable=true
    X-Flatpak=old.wrong.Identity
    Hidden=true
    NoDisplay=true
    OnlyShowIn=KDE;
    NotShowIn=GNOME;
    DESKTOP
    echo '<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect width="48" height="48" fill="blue"/></svg>' > "$out/share/icons/hicolor/scalable/apps/org.example.SandboxCheck.svg"
  '';
  image = pkgs.runCommand "sandbox-podman-test.tar" { nativeBuildInputs = [ pkgs.gnutar ]; } ''
    mkdir -p root/bin
    cp ${pkgs.bash}/bin/bash root/bin/sh
    tar --owner=123 --group=456 -C root -cf "$out" .
  '';
  policy = pkgs.writeText "sandbox-test-image-policy.json" ''{"default":[{"type":"insecureAcceptAnything"}]}'';
in
pkgs.testers.runNixOSTest {
  name = "sandbox-nspawn";
  nodes.machine = { ... }: {
    imports = [
      (import ../nix/nixos.nix { inherit home-manager; })
    ];
    nixpkgs.pkgs = pkgs.lib.mkForce pkgs;
    services.pipewire.enable = true;
    services.pipewire.pulse.enable = true;
    services.pipewire.extraConfig.pipewire."90-sandbox-test"."context.objects" = [
      {
        factory = "adapter";
        args = {
          "factory.name" = "support.null-audio-sink";
          "node.name" = "portal-test-source";
          "media.class" = "Audio/Sink";
          "audio.position" = [
            "FL"
            "FR"
          ];
        };
      }
    ];
    time.timeZone = "Europe/Vienna";
    services.unbound = {
      enable = true;
      settings.server = {
        local-zone = ''"sandbox.test." static'';
        local-data = ''"search-check.sandbox.test. IN A 192.0.2.73"'';
      };
    };
    xdg.portal.extraPortals = [ testPortal ];
    xdg.portal.config.common.default = [ "sandboxtest" ];
    systemd.user.services.sandbox-test-portal = {
      wantedBy = [ "default.target" ];
      path = [ pkgs.pipewire ];
      serviceConfig = {
        Type = "dbus";
        BusName = "org.freedesktop.impl.portal.desktop.sandboxtest";
        ExecStart = "${portalPython}/bin/python3 ${./sandbox-portal-backend.py}";
      };
      unitConfig.ConditionUser = "alice";
    };
    virtualisation.memorySize = 4096;
    virtualisation.cores = 4;
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
      extraGroups = pkgs.lib.optional benchmark "systemd-journal";
    };
    users.users.bob = {
      isNormalUser = true;
      uid = 1001;
    };
    systemd.services.test-device = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.Type = "oneshot";
      script = "mknod -m666 /dev/sandbox-test c 1 3";
    };
    # An explicitly shared child mount beneath a backing home must receive
    # its own owner ID mapping. Instance-private build caches use no such bind.
    systemd.services.test-nested-storage = {
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-tmpfiles-setup.service" ];
      path = [ pkgs.util-linux ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        mkdir -p /home/alice/.sandbox/check/home/SharedStorage
        mount -t tmpfs -o mode=0750,uid=1000,gid=100 tmpfs /home/alice/.sandbox/check/home/SharedStorage
      '';
    };
    systemd.user.services.test-x11-sockets = {
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = "alice";
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${pkgs.writeText "host-x11-test.py" ''
        import socket
        import time
        sockets = []
        for address in ["/tmp/.X11-unix/X77", chr(0) + "/tmp/.X11-unix/X77"]:
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(address)
            listener.listen()
            sockets.append(listener)
        time.sleep(100000)
      ''}";
    };
    systemd.user.services.test-compositor = {
      wantedBy = [ "default.target" ];
      bindsTo = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      serviceConfig.ExecStart = "${pkgs.weston}/bin/weston --backend=headless --renderer=pixman --socket=wayland-test --idle-time=0 --no-config";
      unitConfig.ConditionUser = "alice";
    };
    systemd.user.services.xdg-desktop-portal-gtk.environment.WAYLAND_DISPLAY = "wayland-test";
    systemd.user.services.xdg-desktop-portal.environment.XDG_DATA_DIRS =
      "/run/current-system/sw/share:/etc/profiles/per-user/alice/share";
    systemd.user.services.sandbox-network-test = {
      # Use a non-ephemeral port so pasta's default auto-mirroring would
      # occupy it in the guest and make the independent-bind test fail.
      wantedBy = [ "default.target" ];
      unitConfig.ConditionUser = "alice";
      serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${pkgs.writeText "sandbox-host-network.py" ''
        from http.server import BaseHTTPRequestHandler, HTTPServer
        import socket

        udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        udp.bind(("127.0.0.1", 18761))

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"host loopback works")
        HTTPServer(("127.0.0.1", 18761), Handler).serve_forever()
      ''}";
    };
    home-manager.useGlobalPkgs = true;
    home-manager.useUserPackages = true;
    home-manager.users.alice = {
      home.username = "alice";
      home.homeDirectory = "/home/alice";
      home.stateVersion = "26.05";
      xdg.mimeApps = {
        enable = true;
        defaultApplications."x-scheme-handler/https" = [ "sandbox-url-test.desktop" ];
      };
      programs.nook.enable = true;
      programs.nook.defaults = { lib, ... }: {
        shell = lib.mkDefault pkgs.bashInteractive;
        gpu = lib.mkDefault false;
        environment.SANDBOX_SHARED_MODULE = "applied";
      };
      programs.nook.sandboxes.check = {
        imports = [
          { environment.SANDBOX_PROFILE_IMPORT = "applied"; }
        ];
        environment.GI_TYPELIB_PATH = pkgs.lib.makeSearchPath "lib/girepository-1.0" [
          pkgs.gtk4
          pkgs.pango.out
          pkgs.gdk-pixbuf
          pkgs.graphene
          pkgs.harfbuzz
          pkgs.gobject-introspection
        ];
        audio = true;
        devices = [
          "/dev/sandbox-test"
          "/dev/kmsg"
        ];
        devicePatterns = [ "/dev/sandbox-hotplug*" ];
        hostSessionBus = false;
        portals = true;
        # Extra application permissions must not replace portal access.
        sessionBusPolicies."org.freedesktop.secrets" = "talk";
        dnsSearch = [ "sandbox.test" ];
        startX = true;
        systemBusPolicies."org.freedesktop.hostname1" = "talk";
        podman = true;
        nixosModules = [
          {
            networking.hosts = {
              "127.0.0.1" = [ "project.test" ];
              "::1" = [ "project.test" ];
            };
            systemd.services.profile-module-check = {
              wantedBy = [ "multi-user.target" ];
              serviceConfig = {
                Type = "oneshot";
                RemainAfterExit = true;
                ExecStart = "${pkgs.coreutils}/bin/true";
              };
            };
          }
        ];
        guiEntryPoint = "/bin/check-gui";
        desktopFile = "org.example.SandboxCheck.desktop";
        desktopNameSuffix = " (sandbox)";
        packages = with pkgs; [
          desktopApp
          fhsRuntime
          bashInteractive
          wayland-utils
          portalPython
          fontconfig
          xdpyinfo
          postgresql
          openssh
          pipewire
          pulseaudio
          util-linux
          gnugrep
          podman
          curl
          libnotify
          qtPortalCheck
        ];
        bindMounts = [
          {
            source = "/home/alice/.sandbox/check/home/SharedStorage";
            target = "/home/alice/SharedStorage";
          }
          # Deliberately declare the child first. Both mountpoints are absent
          # initially, and preparation must use the nearest enclosing source.
          {
            source = "/home/alice/nested-share.txt";
            target = "/home/alice/Inbox/child/shared.txt";
          }
          {
            source = "/home/alice/inbox-source";
            target = "/home/alice/Inbox";
          }
          {
            source = "/home/alice/shared";
            target = "/shared";
          }
        ];
        homeManagerModules = [
          {
            programs.ssh = {
              enable = true;
              enableDefaultConfig = false;
              settings."example.invalid".hostname = "example.invalid";
            };
            home.file.activation-check.text = "activated\n";
            home.file.legacy-edit.text = "new managed configuration\n";
            home.file.legacy-directory.source = pkgs.writeTextDir "managed" "managed directory\n";
            systemd.user.services.postgres = {
              Unit.Description = "Sandbox test PostgreSQL";
              Service = {
                ExecStartPre = pkgs.writeShellScript "init-test-db" ''
                  if [ ! -d "$HOME/pgdata" ]; then
                    ${pkgs.postgresql}/bin/initdb -D "$HOME/pgdata" --no-locale --auth=trust
                  fi
                '';
                ExecStart = "${pkgs.postgresql}/bin/postgres -D %h/pgdata -k %t -c listen_addresses=''";
                KillSignal = "SIGINT";
              };
              Install.WantedBy = [ "default.target" ];
            };
          }
        ];
      };
      programs.nook.sandboxes.direct = {
        portals = true;
        hostSessionBus = true;
        gpu = false;
        audio = false;
        packages = [ portalPython ];
      };
      programs.nook.sandboxes.fallback = {
        persistent = false;
        gpu = false;
        audio = false;
        network = false;
        guiEntryPoint = "/bin/fallback-gui";
        packages = [ (pkgs.writeShellScriptBin "fallback-gui" "echo 'fallback GUI started'") ];
      };
      programs.nook.sandboxes.no-portals = {
        hostSessionBus = true;
        network = false;
        audio = false;
        packages = [
          portalPython
          qtPortalCheck
        ];
      };
      programs.nook.sandboxes.no-appearance = {
        hostSessionBus = false;
        inheritAppearance = false;
        network = false;
        audio = false;
      };
      programs.nook.sandboxes.cache-old.imports = [ cacheProfile ];
      programs.nook.sandboxes.cache-new.imports = [ cacheProfile ];
    };
    environment.systemPackages = [
      pkgs.desktop-file-utils
      urlHandler
    ]
    ++ pkgs.lib.optional diagnostics pkgs.trace-cmd;
    systemd.tmpfiles.rules = [
      "d /home/alice/.sandbox 0700 alice users -"
      "d /home/alice/.sandbox/check 0700 alice users -"
      "d /home/alice/.sandbox/check/home 0700 alice users -"
      "d /home/alice/shared 0700 alice users -"
      "d /home/alice/inbox-source 0700 alice users -"
      "f /home/alice/nested-share.txt 0600 alice users - nested-bind-works"
      "d /tmp/.X11-unix 1777 root root -"
    ];
  };
  testScript = ''
    IMAGE = "${image}"
    POLICY = "${policy}"
    COREUTILS = "${pkgs.coreutils}"
    PORTAL_CLIENT = "${./sandbox-portal-client.py}"
    APPEARANCE_CLIENT = "${./sandbox-appearance-client.py}"
    DESKTOP_PYTHON = "${portalPython}/bin/python3"
    BENCHMARK_SCRIPT = "${./benchmark-sandbox.py}"
    PROFILE_SCRIPT = "${./profile-sandbox-startup.py}"
    NIX_BUILD_CHECK = "${pkgs.writeText "sandbox-nix-build-check.nix" ''
      builtins.derivation {
        name = "sandbox-daemon-proxy-check";
        system = "${pkgs.stdenv.hostPlatform.system}";
        # Reattach dependency context when the guest evaluates this file, so
        # the host daemon includes Bash's closure in the build sandbox.
        builder = (builtins.storePath "${pkgs.bash}") + "/bin/bash";
        args = [ "-c" "echo daemon-proxy-works > $out" ];
      }
    ''}"
  ''
  + builtins.readFile (
    if diagnostics then
      ./profile-sandbox-startup-vm.py
    else if benchmark then
      ./benchmark-sandbox-vm.py
    else
      ./sandbox-systemd.py
  );
}
