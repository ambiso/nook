{ home-manager }:
{
  pkgs,
  lib,
  config,
  osConfig ? null,
  ...
}:
let
  inherit (lib) types;
  homeConfig = config;
  outer = config.programs.nook;
  hostTimeZone = outer.host.timeZone;
  launcher = outer.package;
  desktopEnvironmentVariables = [
    "XDG_CURRENT_DESKTOP"
    "XDG_SESSION_DESKTOP"
    "DESKTOP_SESSION"
    "XDG_MENU_PREFIX"
    "GDMSESSION"
    "GNOME_DESKTOP_SESSION_ID"
    "KDE_FULL_SESSION"
    "KDE_SESSION_VERSION"
  ];
  terminalEnvironmentVariables = [
    "TERM"
    "COLORTERM"
  ];
  resourceLimitNames = [
    "CPU"
    "FSIZE"
    "DATA"
    "STACK"
    "CORE"
    "RSS"
    "NOFILE"
    "AS"
    "NPROC"
    "MEMLOCK"
    "LOCKS"
    "SIGPENDING"
    "MSGQUEUE"
    "NICE"
    "RTPRIO"
    "RTTIME"
  ];
  resourceLimitsType = types.submodule {
    options = lib.genAttrs resourceLimitNames (
      name:
      lib.mkOption {
        type = types.nullOr (types.either types.int types.str);
        default = null;
        description = "Value for systemd's Limit${name} and DefaultLimit${name} settings.";
      }
    );
  };
  mountType = types.submodule {
    options = {
      source = lib.mkOption {
        type = types.str;
        description = "Absolute host path; @host-runtime@/ expands to the owner's runtime directory at launch.";
      };
      target = lib.mkOption { type = types.str; };
      readOnly = lib.mkOption {
        type = types.bool;
        default = false;
      };
    };
  };
  managedMountSourceType = types.submodule {
    options = {
      scope = lib.mkOption {
        type = types.enum [
          "host"
          "hostHome"
          "profile"
          "instance"
        ];
        default = "instance";
        description = ''
          Namespace used to resolve the source at launch. Profile volumes are
          deliberately shared by every instance; instance volumes follow the
          selected instance's persistence lifetime.
        '';
      };
      path = lib.mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Absolute path for host sources or a path relative to the host home
          for hostHome sources. Managed profile and instance volumes use the
          mount attribute name and do not accept a path.
        '';
      };
    };
  };
  managedMountTargetType = types.submodule {
    options = {
      scope = lib.mkOption {
        type = types.enum [ "sandboxHome" ];
        default = "sandboxHome";
      };
      path = lib.mkOption {
        type = types.str;
        description = "Path below the sandbox user's home.";
      };
    };
  };
  managedMountType = types.submodule {
    options = {
      source = lib.mkOption {
        type = managedMountSourceType;
        default = { };
      };
      target = lib.mkOption {
        type = types.oneOf [
          types.str
          managedMountTargetType
        ];
      };
      readOnly = lib.mkOption {
        type = types.bool;
        default = false;
      };
    };
  };
  profileType = types.submoduleWith {
    specialArgs = { inherit pkgs; };
    # Apply shared modules within each profile's module evaluation, so defaults,
    # imports and overrides have the same semantics as other NixOS/HM options.
    modules = [
      outer.defaults
      profileModule
    ];
  };
  profileModule =
    { name, config, ... }:
    let
      cfg = config;
      shellPackage = cfg.shell;
      shell = builtins.baseNameOf (lib.getExe shellPackage);
      home = homeConfig.home.homeDirectory;
      username = homeConfig.home.username;
      stateRoot =
        if lib.hasPrefix "/" cfg.stateDirectory then
          cfg.stateDirectory
        else
          "${home}/${cfg.stateDirectory}";
      resourceLimits = lib.filterAttrs (_: value: value != null) cfg.resourceLimits;
      validRelativePath =
        path:
        path != ""
        && !(lib.hasPrefix "/" path)
        && !(lib.hasInfix "\\" path)
        && !(lib.hasInfix ":" path)
        && lib.all (component: component != "" && component != "." && component != "..") (
          lib.splitString "/" path
        );
      typedMounts = lib.mapAttrsToList (
        volumeName: mount:
        let
          scope = mount.source.scope;
          suppliedPath = mount.source.path;
          source =
            if scope == "host" then
              if suppliedPath != null && lib.hasPrefix "/" suppliedPath then
                suppliedPath
              else
                throw "programs.nook.sandboxes.${name}.mounts.${volumeName}: host sources require an absolute path"
            else if scope == "hostHome" then
              if suppliedPath != null && validRelativePath suppliedPath then
                suppliedPath
              else
                throw "programs.nook.sandboxes.${name}.mounts.${volumeName}: hostHome sources require a safe relative path"
            else if suppliedPath != null then
              throw "programs.nook.sandboxes.${name}.mounts.${volumeName}: ${scope} volumes use their attribute name and do not accept source.path"
            else if builtins.match "[a-z0-9][a-z0-9-]{0,63}" volumeName == null then
              throw "programs.nook.sandboxes.${name}.mounts.${volumeName}: managed volume names must be at most 64 lowercase letters, digits, or hyphens"
            else
              "volumes/${volumeName}";
          target =
            if builtins.isString mount.target then
              mount.target
            else if validRelativePath mount.target.path then
              "${home}/${mount.target.path}"
            else
              throw "programs.nook.sandboxes.${name}.mounts.${volumeName}: sandboxHome targets require a safe relative path";
        in
        {
          inherit source target;
          sourceScope =
            {
              host = "host";
              hostHome = "host-home";
              profile = "profile";
              instance = "instance";
            }
            .${scope};
          inherit (mount) readOnly;
        }
      ) cfg.mounts;
      uid = outer.guest.uid;
      gid = outer.guest.gid;
      defaultNetworkArgs = [
        "--no-map-gw"
        "--dns-forward"
        "169.254.1.1"
        # Pasta defaults every forwarding direction to "auto". Always pass
        # these before the profile's arguments so specifying one mapping does
        # not accidentally enable automatic forwarding in the other three.
        "--tcp-ports"
        "none"
        "--udp-ports"
        "none"
        "--tcp-ns"
        "none"
        "--udp-ns"
        "none"
      ];
      nixEnabled = cfg.nix || cfg.nixBuildLogs || hm != null;
      hm =
        if cfg.homeManagerModules == [ ] then
          null
        else
          (home-manager.lib.homeManagerConfiguration {
            inherit pkgs;
            extraSpecialArgs = { inherit name; };
            modules = cfg.homeManagerModules ++ [
              {
                home.username = lib.mkDefault username;
                home.homeDirectory = lib.mkDefault home;
                home.stateVersion = lib.mkDefault homeConfig.home.stateVersion;
                systemd.user.startServices = lib.mkDefault true;
              }
            ];
          }).config;
      appPackage = pkgs.symlinkJoin {
        name = "sandbox-apps-${name}";
        paths =
          cfg.packages
          ++ [
            shellPackage
            pkgs.bashInteractive
            pkgs.coreutils-full
            pkgs.dbus
            pkgs.systemd
            pkgs.procps
            pkgs.gnugrep
            pkgs.ripgrep
            pkgs.eza
            pkgs.xdg-utils
            pkgs.which
            pkgs.strace
            pkgs.wayland-utils
            pkgs.vulkan-tools
          ]
          ++ lib.optionals (hm != null) hm.home.packages;
      };
      activation = pkgs.writeShellScript "sandbox-activate-${name}" ''
        set -eu
        status=$(${pkgs.systemd}/bin/systemctl --user is-system-running --wait || true)
        case "$status" in running|degraded) ;; *) echo "User manager failed: $status" >&2; exit 1 ;; esac
        ${lib.optionalString cfg.portals "${pkgs.systemd}/bin/systemctl --user start sandbox-portals.service"}
        ${lib.optionalString (
          cfg.inheritAppearance && !cfg.portals
        ) "${pkgs.systemd}/bin/systemctl --user start sandbox-appearance.service"}
        ${lib.optionalString (hm != null) ''
          generation=${hm.home.activationPackage}
          current_home="''${XDG_STATE_HOME:-$HOME/.local/state}/home-manager/gcroots/current-home"
          completed_home="''${XDG_STATE_HOME:-$HOME/.local/state}/sandbox/home-generation"
          # The user manager starts the installed services and recreates its
          # runtime directory on every boot. Only configuration activation is
          # cached. A separate success marker covers our SSH materialization
          # too, and never lets a partially failed activation count as done.
          if [ "$(cat "$completed_home" 2>/dev/null || true)" = "$generation" ] \
              && [ "$(readlink -f "$current_home" || true)" = "$generation" ]; then
            echo "Sandbox home generation unchanged; reusing its configuration"
          else
            # The old launcher mounted managed files over empty placeholders.
            # Native Home Manager must replace those files with links. Enable
            # its normal backup handling only for the first legacy migration,
            # preserving user changes and refusing to overwrite earlier backups.
            if [ -f "$HOME/.hm-generation" ] && [ ! -e "$current_home" ]; then
              export HOME_MANAGER_BACKUP_EXT=pre-systemd
            fi
            # The public store is mounted without ID translation, so OpenSSH
            # sees store files as nobody-owned. Materialize only HM's managed SSH
            # config. Before reactivation, remove an earlier copy only when it
            # still matches the previous generation; user edits remain protected.
            old_ssh="''${XDG_STATE_HOME:-$HOME/.local/state}/home-manager/gcroots/current-home/home-files/.ssh/config"
            if [ -f "$HOME/.ssh/config" ] && [ ! -L "$HOME/.ssh/config" ] \
                && [ -f "$old_ssh" ] && cmp -s "$HOME/.ssh/config" "$old_ssh"; then
              rm "$HOME/.ssh/config"
            fi
            PATH="${pkgs.nix}/bin:$PATH" ${hm.home.activationPackage}/activate
            ${lib.optionalString
              (lib.any (file: file.enable && file.target == ".ssh/config") (builtins.attrValues hm.home.file))
              ''
                if [ -L "$HOME/.ssh/config" ]; then
                  target=$(readlink -f "$HOME/.ssh/config")
                  temporary=$(mktemp "$HOME/.ssh/config.XXXXXX")
                  cp "$target" "$temporary"
                  chmod 600 "$temporary"
                  mv -T "$temporary" "$HOME/.ssh/config"
                fi
              ''
            }
            mkdir -p "$(dirname "$completed_home")"
            temporary=$(mktemp "$completed_home.XXXXXX")
            printf '%s\n' "$generation" > "$temporary"
            mv -T "$temporary" "$completed_home"
          fi
        ''}
      '';
      homeManagerRecovery = pkgs.writeShellScriptBin "nook-home-manager" ''
        set -eu
        if [ "''${1:-}" != switch ]; then
          echo "Usage: nook-home-manager switch [-b EXTENSION]" >&2
          exit 2
        fi
        shift
        if [ "$#" -eq 0 ]; then
          exec ${activation}
        fi
        if [ "$#" -eq 2 ] && { [ "$1" = -b ] || [ "$1" = --backup-extension ]; }; then
          case "$2" in *[!A-Za-z0-9._-]*|"") echo "Invalid backup extension" >&2; exit 2;; esac
          export HOME_MANAGER_BACKUP_EXT="$2"
          exec ${activation}
        fi
        echo "Usage: nook-home-manager switch [-b EXTENSION]" >&2
        exit 2
      '';
      nixLog = pkgs.writeShellScriptBin "nix-log" ''
        exec ${pkgs.nix}/bin/nix \
          --extra-experimental-features 'nix-command read-only-local-store' \
          --offline \
          log --store 'local?read-only=true' "$@"
      '';
      entry = pkgs.writeShellScript "sandbox-command-${name}" ''
        set -eu
        # Entry is dispatched by the trusted guest user manager. Export the
        # evaluated guest session environment here as well, so applications do
        # not depend on client environment forwarding or manager import state.
        ${lib.concatStringsSep "\n" (
          lib.mapAttrsToList (
            name: value: "export ${name}=${lib.escapeShellArg (toString value)}"
          ) container.systemd.globalEnvironment
        )}
        export HOME=${lib.escapeShellArg home}
        export XDG_RUNTIME_DIR=/run/user/${toString uid}
        export PATH=${sessionTools}/bin:/run/wrappers/bin:${appPackage}/bin:/run/current-system/sw/bin
        export SHELL=${appPackage}/bin/${shell}
        export WAYLAND_DISPLAY=wayland-0
        export XDG_DATA_DIRS=${lib.escapeShellArg container.environment.variables.XDG_DATA_DIRS}
        export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
        unset DBUS_SYSTEM_BUS_ADDRESS DISPLAY XAUTHORITY
        ${lib.optionalString cfg.startX "export DISPLAY=:0"}
        ${lib.optionalString cfg.hostSessionBus "export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/host/session-bus"}
        ${lib.optionalString (
          cfg.systemBusPolicies != { }
        ) "export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/host/system-bus"}
        exec "$@"
      '';
      sessionTools =
        pkgs.runCommand "sandbox-session-tools" { nativeBuildInputs = [ pkgs.makeWrapper ]; }
          ''
            mkdir -p "$out/bin"
            for tool in systemctl systemd-run busctl systemd-analyze; do
              if [ -x "${appPackage}/bin/$tool" ]; then
                makeWrapper "${appPackage}/bin/$tool" "$out/bin/$tool" \
                  --run 'export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"; unset DBUS_SYSTEM_BUS_ADDRESS'
              fi
            done
            if [ -x "${appPackage}/bin/podman" ]; then
              makeWrapper "${appPackage}/bin/podman" "$out/bin/podman" \
                --run 'export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"; unset DBUS_SYSTEM_BUS_ADDRESS' \
                --run 'if [ -z "''${TMPDIR:-}" ]; then export TMPDIR="$HOME/.local/share/containers/tmp"; install -d -m 0700 -- "$TMPDIR"; fi'
            fi
            ${lib.optionalString (hm != null) ''
              ln -s ${homeManagerRecovery}/bin/nook-home-manager "$out/bin/nook-home-manager"
            ''}
            ${lib.optionalString cfg.nixBuildLogs ''
              ln -s ${nixLog}/bin/nix-log "$out/bin/nix-log"
            ''}
          '';
      container =
        (import "${pkgs.path}/nixos/lib/eval-config.nix" {
          inherit pkgs;
          system = pkgs.stdenv.hostPlatform.system;
          specialArgs = {
            inherit
              username
              uid
              gid
              home
              appPackage
              sessionTools
              activation
              ;
            inherit shellPackage;
            inherit (cfg)
              environment
              startX
              podman
              portals
              inheritAppearance
              nixBuildLogs
              dnsSearch
              devicePatterns
              ;
            inherit resourceLimits;
            inherit hostTimeZone;
            inherit launcher;
            inherit nixEnabled;
          };
          modules = [ ./container.nix ] ++ cfg.nixosModules;
        }).config;
      rules = policies: lib.mapAttrsToList (n: action: "--${action}=${n}") policies;
      # An application's extra policy (for example Signal's secrets access)
      # replaces an option default. Required portal rules must be added here,
      # otherwise that extra policy silently disables the selected portals.
      sessionRules =
        rules cfg.sessionBusPolicies
        ++ lib.optional cfg.portals "--talk=org.freedesktop.portal.*"
        ++ lib.optionals (cfg.inheritAppearance && !cfg.portals) [
          "--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.*"
          "--broadcast=org.freedesktop.portal.Desktop=org.freedesktop.portal.Settings.SettingChanged"
        ];
      # Byte lengths delimit values, including embedded newlines and tabs.
      # A version header distinguishes this format from legacy line records.
      textPairs =
        attrs:
        "nook-pairs-v1\n"
        + lib.concatStrings (
          lib.mapAttrsToList (
            k: v: "${k}\t${toString (builtins.stringLength (toString v))}\t${toString v}\n"
          ) attrs
        );
      mounts = [
        {
          source = "home";
          sourceScope = "instance";
          target = home;
          readOnly = false;
        }
      ]
      ++ (map (mount: mount // { sourceScope = "host"; }) cfg.bindMounts)
      ++ typedMounts
      ++ lib.optionals cfg.nixBuildLogs [
        {
          source = "/nix/var/log/nix";
          sourceScope = "host";
          target = "/nix/var/log/nix";
          readOnly = true;
        }
        {
          source = "/nix/var/nix/db";
          sourceScope = "host";
          target = "/nix/var/nix/db";
          readOnly = true;
        }
      ]
      ++ lib.optionals cfg.gpu [
        {
          source = "${pkgs.mesa}";
          sourceScope = "host";
          target = "/run/opengl-driver";
          readOnly = true;
        }
      ]
      ++ lib.optionals (cfg.gpu && pkgs.stdenv.hostPlatform.system == "x86_64-linux") [
        {
          source = "${pkgs.pkgsi686Linux.mesa}";
          sourceScope = "host";
          target = "/run/opengl-driver-32";
          readOnly = true;
        }
      ];
      manifest = pkgs.writeText "sandbox-profile-${name}" (textPairs {
        inherit
          name
          username
          home
          uid
          gid
          ;
        identityVersion = 2;
        supervisorContract = 2;
        supervisorExecutable = "${launcher}/bin/.nook-root-wrapped";
        shell = "${appPackage}/bin/${shell}";
        gui =
          if cfg.guiEntryPoint == null then "" else "${appPackage}/${lib.removePrefix "/" cfg.guiEntryPoint}";
        entry = entry;
        init = "${container.system.build.toplevel}/init";
        activation = activation;
        system = "${container.system.build.toplevel}";
        hostname = container.networking.hostName;
        state = stateRoot;
        persistent = if cfg.persistent then 1 else 0;
        network = if cfg.network then 1 else 0;
        podman = if cfg.podman then 1 else 0;
        audio = if cfg.audio then 1 else 0;
        nix = if nixEnabled then 1 else 0;
        hostSessionBus = if cfg.hostSessionBus then 1 else 0;
        portals = if cfg.portals then 1 else 0;
        inheritAppearance = if cfg.inheritAppearance then 1 else 0;
        extraSystemCalls = lib.concatStringsSep " " cfg.extraSystemCalls;
        resourceLimits = lib.concatStringsSep "\n" (
          lib.mapAttrsToList (limit: value: "${limit}=${toString value}") resourceLimits
        );
        portalAppId = "eu.ambiso.sandbox.profile_${lib.replaceStrings [ "-" ] [ "_" ] name}";
      });
      mountFile = pkgs.writeText "sandbox-mounts-${name}" (
        "nook-mounts-v2\n"
        + lib.concatMapStrings (
          m: "${if m.readOnly then "ro" else "rw"}\t${m.sourceScope}\t${m.source}\t${m.target}\n"
        ) mounts
        + lib.concatMapStrings (path: "device\t${path}\t${path}\n") (
          lib.unique (lib.optionals cfg.gpu [ "/dev/dri" ] ++ cfg.devices)
        )
        + lib.concatMapStrings (pattern: "device-pattern\t${pattern}\t${pattern}\n") cfg.devicePatterns
      );
      profile = pkgs.runCommand "sandbox-profile-${name}" { } ''
        mkdir -p "$out/root/etc" "$out/root/usr/lib" "$out/root/tmp" "$out/root/var"
        echo 'ID=nixos' > "$out/root/etc/os-release"
        # An initialized image with an empty ID gets an instance-specific ID
        # without rerunning first-boot presets. NixOS supplies the unit links.
        touch "$out/root/etc/machine-id"
        ln -s ${manifest} "$out/profile"
        ln -s ${mountFile} "$out/mounts"
        ln -s ${pkgs.writeText "sandbox-environment" (textPairs cfg.environment)} "$out/environment"
        ln -s ${pkgs.writeText "sandbox-host-environment" (lib.concatStringsSep "\n" (lib.unique cfg.hostEnvironment))} "$out/host-environment"
        ln -s ${pkgs.writeText "session-bus-rules" (lib.concatStringsSep "\n" (lib.unique sessionRules))} "$out/session-rules"
        ln -s ${pkgs.writeText "system-bus-rules" (lib.concatStringsSep "\n" (rules cfg.systemBusPolicies))} "$out/system-rules"
        ln -s ${
          pkgs.writeText "pasta-options" (lib.concatStringsSep "\n" (defaultNetworkArgs ++ cfg.networkArgs))
        } "$out/pasta"
      '';
    in
    {
      imports = [ (lib.mkRenamedOptionModule [ "desktopFileSuffix" ] [ "desktopNameSuffix" ]) ];
      options = {
        shell = lib.mkOption {
          type = types.package;
          default = pkgs.fish;
          defaultText = lib.literalExpression "pkgs.fish";
          example = lib.literalExpression "pkgs.bashInteractive";
          description = "Shell package for this sandbox; its main executable is used for interactive entry.";
        };
        packages = lib.mkOption {
          type = types.listOf types.package;
          default = [ ];
        };
        nixosModules = lib.mkOption {
          type = types.listOf types.deferredModule;
          default = [ ];
          description = "Additional NixOS modules for the sandbox's native system manager.";
        };
        homeManagerModules = lib.mkOption {
          type = types.listOf types.deferredModule;
          default = [ ];
          description = ''
            Home Manager configuration activated once per successful generation
            in each instance home. Use systemd user services for work needed on
            every boot.
          '';
        };
        persistent = lib.mkOption {
          type = types.bool;
          default = true;
          description = "Persist the main instance's home. Explicitly named instances always persist; ephemeral instances never do.";
        };
        stateDirectory = lib.mkOption {
          type = types.str;
          default = "${homeConfig.xdg.stateHome}/nook/${name}";
          defaultText = lib.literalExpression ''config.xdg.stateHome + "/nook/${name}"'';
          description = ''
            Absolute host directory for this profile's persistent state. The
            default follows Home Manager's configured xdg.stateHome. Relative
            values remain supported and are resolved under the home directory.
          '';
        };
        guiEntryPoint = lib.mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Default GUI executable in the sandbox; also exports a host application launcher.";
        };
        guiName = lib.mkOption {
          type = types.nullOr types.str;
          default = null;
        };
        desktopFile = lib.mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Upstream desktop filename to preserve for window matching; a basic entry is generated if unavailable.";
        };
        desktopNameSuffix = lib.mkOption {
          type = types.str;
          default = "";
          description = "Append to the displayed application name while preserving its desktop ID.";
        };
        startX = lib.mkOption {
          type = types.bool;
          default = false;
          description = "Run sandbox-local Xwayland over the forwarded Wayland connection.";
        };
        gpu = lib.mkOption {
          type = types.bool;
          default = true;
        };
        audio = lib.mkOption {
          type = types.bool;
          default = true;
        };
        network = lib.mkOption {
          type = types.bool;
          default = true;
        };
        networkArgs = lib.mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "--udp-ports"
            "192.0.2.10%eth0/8080"
          ];
          description = ''
            Additional arguments for pasta, applied after Nook disables
            automatic forwarding in both directions. Later pasta port options
            replace the corresponding `none` default, so a profile can publish
            an explicit mapping without enabling unrelated forwarding.
          '';
        };
        extraSystemCalls = lib.mkOption {
          type = types.listOf (types.strMatching "[@A-Za-z0-9_-]+");
          default = [ ];
          example = [ "perf_event_open" ];
          description = "System calls or systemd syscall groups to add to systemd-nspawn's default allowlist.";
        };
        resourceLimits = lib.mkOption {
          type = resourceLimitsType;
          default = { };
          example = {
            MEMLOCK = "1G";
            NOFILE = 1048576;
          };
          description = ''
            POSIX resource limits inherited by sandbox commands and services.
            Each non-null value configures both the host supervisor's Limit*
            setting and the corresponding guest systemd DefaultLimit* setting.
            Values use systemd syntax, including "infinity" and soft:hard pairs.
          '';
        };
        dnsSearch = lib.mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "DNS search domains inside the sandbox.";
        };
        portals = lib.mkOption {
          type = types.bool;
          default = false;
          description = "Allow desktop portals, including user-approved access to host files and screen capture.";
        };
        inheritAppearance = lib.mkOption {
          type = types.bool;
          default = true;
          description = "Read host appearance settings and follow changes through the Settings portal. This does not enable file selection, screen capture, or other desktop portals.";
        };
        nix = lib.mkOption {
          type = types.bool;
          default = false;
          description = "Broker untrusted-client access to the host Nix daemon.";
        };
        nixBuildLogs = lib.mkOption {
          type = types.bool;
          default = cfg.nix;
          defaultText = lib.literalExpression "config.nix";
          description = ''
            Expose the host's retained Nix build logs and store database
            read-only, and install nix-log for querying them. This defaults to
            the value of nix, can be disabled independently, and implies Nix
            daemon access when enabled explicitly. It reveals logs from builds
            requested by other host users and sandboxes.
          '';
        };
        podman = lib.mkOption {
          type = types.bool;
          default = false;
        };
        devices = lib.mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "/dev/kvm" ];
          description = "Host character devices or device directories to forward when accessible to the owner. Add /dev/kvm to allow hardware-accelerated virtual machines.";
        };
        devicePatterns = lib.mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "/dev/hidraw*" ];
          description = "Patterns for top-level host character devices; only devices accessible to the owner are forwarded, including after hotplug.";
        };
        bindMounts = lib.mkOption {
          type = types.listOf mountType;
          default = [ ];
          description = "Low-level absolute host bind mounts. Prefer mounts for managed and home-relative storage.";
        };
        mounts = lib.mkOption {
          type = types.attrsOf managedMountType;
          default = { };
          example = lib.literalExpression ''
            {
              projects = {
                source = { scope = "hostHome"; path = "Projects"; };
                target = { scope = "sandboxHome"; path = "Projects"; };
              };
              build-store = {
                source.scope = "instance";
                target = "/build-store";
              };
            }
          '';
          description = ''
            Typed host and managed-volume mounts. Instance volumes are private
            to the selected main, named, or ephemeral instance. Profile
            volumes are explicitly shared by every instance of this sandbox.
          '';
        };
        environment = lib.mkOption {
          type = types.attrsOf types.str;
          default = { };
          description = "Environment variables embedded in the world-readable Nix store profile. Do not put secrets here.";
        };
        hostEnvironment = lib.mkOption {
          type = types.listOf (types.strMatching "[A-Za-z_][A-Za-z0-9_]*");
          default = [ ];
          example = [ "MY_EDITOR_INTEGRATION" ];
          description = "Host environment variable names whose current values may be forwarded when entering this sandbox. Values are not stored in the Nix store.";
        };
        hostSessionBus = lib.mkOption {
          type = types.bool;
          default = true;
        };
        sessionBusPolicies = lib.mkOption {
          type = types.attrsOf (
            types.enum [
              "see"
              "talk"
              "own"
            ]
          );
          default = {
            "org.freedesktop.DBus" = "talk";
            "eu.ambiso.sandbox.${name}" = "own";
            "eu.ambiso.sandbox.${name}.*" = "own";
          }
          // lib.optionalAttrs cfg.portals { "org.freedesktop.portal.*" = "talk"; };
        };
        systemBusPolicies = lib.mkOption {
          type = types.attrsOf (
            types.enum [
              "see"
              "talk"
              "own"
            ]
          );
          default = { };
        };
        _profile = lib.mkOption {
          type = types.package;
          readOnly = true;
          internal = true;
        };
        _container = lib.mkOption {
          type = types.unspecified;
          readOnly = true;
          internal = true;
        };
        _appPackage = lib.mkOption {
          type = types.package;
          readOnly = true;
          internal = true;
        };
      };
      config = {
        hostEnvironment = lib.mkBefore (desktopEnvironmentVariables ++ terminalEnvironmentVariables);
        _profile = profile;
        _container = container;
        _appPackage = appPackage;
      };
    };
  cfg = outer.sandboxes;
  guiNameFor =
    name: c:
    if c.guiName != null then
      c.guiName
    else
      builtins.baseNameOf (if c.guiEntryPoint == null then name else c.guiEntryPoint);
  desktopFileFor =
    name: c: if c.desktopFile != null then c.desktopFile else "${guiNameFor name c}.desktop";
  catalog = pkgs.linkFarm "sandbox-profiles" (
    lib.mapAttrsToList (name: c: {
      inherit name;
      path = c._profile;
    }) cfg
  );
  # The launcher is installed on the host, while application packages live in
  # the guest. Export their icon data explicitly so GNOME can resolve Icon=.
  icons = pkgs.buildEnv {
    name = "sandbox-icons";
    paths = lib.mapAttrsToList (_: profile: profile._appPackage) cfg;
    pathsToLink = [
      "/share/icons"
      "/share/pixmaps"
    ];
    ignoreCollisions = true;
  };
  wrapper =
    pkgs.runCommand "nook-launchers"
      {
        nativeBuildInputs = [
          pkgs.makeWrapper
          pkgs.desktop-file-utils
        ];
      }
      ''
        mkdir -p "$out/bin" "$out/share/applications"
        for directory in icons pixmaps; do
          if [ -d "${icons}/share/$directory" ]; then
            ln -s "${icons}/share/$directory" "$out/share/$directory"
          fi
        done
        makeWrapper ${launcher}/bin/nk "$out/bin/nk" --set NOOK_PROFILES ${catalog}
        ln -s nk "$out/bin/sandbox"
        for completion in bash-completion zsh fish; do
          if [ -d "${launcher}/share/$completion" ]; then
            ln -s "${launcher}/share/$completion" "$out/share/$completion"
          fi
        done
        ${lib.concatStringsSep "\n" (
          lib.mapAttrsToList (
            name: c:
            let
              guiName = guiNameFor name c;
              desktop = desktopFileFor name c;
              fallbackDesktop = pkgs.makeDesktopItem {
                name = lib.removeSuffix ".desktop" desktop;
                desktopName = guiName;
                exec = "${guiName} %U";
                icon = "application-x-executable";
                categories = [ "Utility" ];
                terminal = false;
              };
            in
            ''
              makeWrapper ${launcher}/bin/nk "$out/bin/nk-${name}" --set NOOK_PROFILES ${catalog} --add-flags "shell ${name} --"
              ln -s nk-${name} "$out/bin/sandbox-${name}"
              ${lib.optionalString (c.guiEntryPoint != null) ''
                makeWrapper ${launcher}/bin/nk "$out/bin/${guiName}" --set NOOK_PROFILES ${catalog} --add-flags "gui ${name} --"
                destination="$out/share/applications/${desktop}"
                if [ -f "${c._appPackage}/share/applications/${desktop}" ]; then
                  # Wayland app IDs / StartupWMClass resolve to the upstream
                  # desktop ID. Renaming this file to the executable name breaks
                  # reverse-DNS IDs such as info.mumble.Mumble in GNOME's overview.
                  cp "${c._appPackage}/share/applications/${desktop}" "$destination"
                else
                  # A GUI entrypoint is sufficient to publish an application,
                  # including custom scripts and packages without desktop data.
                  cp "${fallbackDesktop}/share/applications/${desktop}" "$destination"
                fi
                chmod u+w "$destination"
                sed -i "s|^Exec=[^ ]*|Exec=$out/bin/${guiName}|; /^X-Flatpak=/d" "$destination"
                # Selecting a GUI entrypoint explicitly opts into application
                # search. Upstream helper/desktop-specific visibility flags
                # must not hide this launcher from GNOME.
                desktop-file-edit --set-key=TryExec --set-value="$out/bin/${guiName}" \
                  --set-key=DBusActivatable --set-value=false \
                  --remove-key=Hidden --remove-key=NoDisplay \
                  --remove-key=OnlyShowIn --remove-key=NotShowIn "$destination"
                ${lib.optionalString (c.desktopNameSuffix != "") ''
                  original_name=$(sed -n 's/^Name=//p' "$destination" | head -n1)
                  desktop-file-edit --set-name="$original_name"${lib.escapeShellArg c.desktopNameSuffix} "$destination"
                ''}
              ''}
            ''
          ) cfg
        )}
      '';
in
{
  imports =
    map
      (
        id:
        lib.mkRemovedOptionModule [ "programs" "nook" "host" id ] ''
          Host account IDs are now resolved at launch. Remove this setting, including
          mkForce overrides. To change IDs inside the guest, use programs.nook.guest.${id}.
        ''
      )
      [
        "uid"
        "gid"
      ];
  options.programs.nook = {
    enable = lib.mkEnableOption "Nook sandbox profiles and launchers";
    package = lib.mkOption {
      type = types.package;
      # Keep generated profile formats and their parser on the same revision.
      # An unrelated pkgs.nook overlay may refer to an older module API.
      default = pkgs.callPackage ../. { };
      defaultText = lib.literalExpression "pkgs.callPackage <nook> { }";
      description = "Nook launcher package, shared by this user's host services and guest helpers.";
    };
    sandboxes = lib.mkOption {
      type = types.attrsOf profileType;
      default = { };
      description = "Named systemd-nspawn sandboxes, each configured as a module.";
    };
    defaults = lib.mkOption {
      type = types.deferredModule;
      default = { };
      description = "Module applied to every sandbox. Use mkDefault for scalar defaults that individual profiles may override.";
    };
    guest.uid = lib.mkOption {
      type = types.ints.between 1 99999;
      default = 1000;
      description = "User ID inside the guest, independent of the host account.";
    };
    guest.gid = lib.mkOption {
      type = types.ints.between 1 99999;
      default = 100;
      description = "Primary group ID inside the guest, independent of the host account.";
    };
    host.timeZone = lib.mkOption {
      type = types.nullOr types.str;
      default = if osConfig == null then null else osConfig.time.timeZone;
      defaultText = lib.literalExpression "osConfig.time.timeZone";
      description = "Host timezone inherited by sandbox NixOS systems; set this when evaluating Home Manager outside NixOS.";
    };
    profiles = lib.mkOption {
      type = types.package;
      readOnly = true;
      internal = true;
    };
  };
  config = lib.mkMerge [
    { programs.nook.profiles = catalog; }
    (lib.mkIf outer.enable {
      assertions = lib.mapAttrsToList (name: _: {
        assertion = builtins.match "[a-z0-9][a-z0-9-]{0,23}" name != null;
        message = "Sandbox names must be at most 24 lowercase letters, digits, or hyphens.";
      }) cfg;
      home.packages = lib.mkIf (cfg != { }) [ wrapper ];
      # GNOME watches the user applications directory. Link each entry there
      # through Home Manager so activating a new Nix profile also produces a
      # file change in that directory, rather than only swapping a profile link.
      xdg.dataFile = lib.mapAttrs' (
        name: c:
        lib.nameValuePair "applications/${desktopFileFor name c}" {
          source = "${wrapper}/share/applications/${desktopFileFor name c}";
        }
      ) (lib.filterAttrs (_: c: c.guiEntryPoint != null) cfg);
    })
  ];
}
