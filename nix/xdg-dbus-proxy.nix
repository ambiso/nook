{ pkgs }:

# CVE-2026-94422 permits a filtered client to forge reply metadata and bypass
# the D-Bus policy. Keep the fallback until supported nixpkgs revisions ship
# 0.1.9 or newer; then use the package from nixpkgs unchanged.
if pkgs.lib.versionAtLeast pkgs.xdg-dbus-proxy.version "0.1.9" then
  pkgs.xdg-dbus-proxy
else
  pkgs.xdg-dbus-proxy.overrideAttrs (_: {
    version = "0.1.9";
    src = pkgs.fetchurl {
      url = "https://github.com/flatpak/xdg-dbus-proxy/releases/download/0.1.9/xdg-dbus-proxy-0.1.9.tar.xz";
      hash = "sha256-VFDdpYbsO7PKcJ0xHoRUh4g/qjsJz1YmCNfoT0MR3O0=";
    };
  })
