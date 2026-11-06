use std::path::Path;

// NixOS installs these commands at stable system paths. Other systemd hosts
// generally install them in /usr/bin. Never search a caller-controlled PATH
// from the privileged entry helper.
pub fn systemctl() -> &'static str {
    if Path::new("/run/current-system/sw/bin/systemctl").exists() {
        "/run/current-system/sw/bin/systemctl"
    } else {
        "/usr/bin/systemctl"
    }
}

pub fn systemd_run() -> &'static str {
    if Path::new("/run/current-system/sw/bin/systemd-run").exists() {
        "/run/current-system/sw/bin/systemd-run"
    } else {
        "/usr/bin/systemd-run"
    }
}

pub fn pkexec() -> &'static str {
    if Path::new("/run/wrappers/bin/pkexec").exists() {
        "/run/wrappers/bin/pkexec"
    } else {
        "/usr/bin/pkexec"
    }
}
