#![forbid(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use nook::{
    config::{Profile, Result},
    supervise,
};
use std::{
    env,
    path::{Component, PathBuf},
};

#[derive(Parser)]
#[command(name = "nook-root", about = "Internal privileged Nook helper")]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Run the installed supervisor service.
    Serve(Target),
    /// Switch a compatible running guest to a new installed generation.
    Reload(Target),
    /// Enter a running guest after pkexec authentication.
    Enter(Enter),
}

#[derive(Args)]
struct Target {
    profile: PathBuf,
    instance: String,
}

#[derive(Args)]
struct Enter {
    #[command(flatten)]
    target: Target,
    #[arg(long, value_parser = parse_entry_environment)]
    entry_environment: Vec<String>,
    #[arg(long, value_parser = parse_entry_directory)]
    entry_directory: Option<PathBuf>,
    #[arg(last = true)]
    command: Vec<String>,
}

fn allowed_entry_environment(key: &str) -> bool {
    [
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "VTE_VERSION",
        "LANG",
        "LANGUAGE",
        "TZ",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
        "DESKTOP_STARTUP_ID",
        "XDG_ACTIVATION_TOKEN",
        "GTK_IM_MODULE",
        "QT_IM_MODULE",
        "QT_IM_MODULES",
        "XMODIFIERS",
    ]
    .contains(&key)
        || key.strip_prefix("LC_").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
        })
}

fn parse_entry_directory(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() && !path.components().any(|part| part == Component::ParentDir) {
        Ok(path)
    } else {
        Err("entry directory must be absolute and contain no parent traversal".into())
    }
}

fn parse_entry_environment(value: &str) -> std::result::Result<String, String> {
    if value
        .split_once('=')
        .is_some_and(|(key, _)| allowed_entry_environment(key))
    {
        Ok(value.to_owned())
    } else {
        Err("only terminal, locale, desktop and input-method variables may be forwarded".into())
    }
}

fn run() -> Result<i32> {
    // Refuse before parsing caller-controlled arguments. This binary has no
    // useful unprivileged mode and is installed only for systemd and pkexec.
    if !nix::unistd::geteuid().is_root() {
        return Err("the privileged Nook helper must run as root".into());
    }
    let cli = Cli::parse();
    match cli.command {
        Action::Enter(request) => {
            if env::var_os("PKEXEC_UID").is_none() {
                return Err("entry requires pkexec authentication".into());
            }
            supervise::enter(
                &Profile::load(&request.target.profile)?,
                &request.target.instance,
                &request.command,
                &request.entry_environment,
                request.entry_directory.as_deref(),
            )
        }
        Action::Serve(target) => {
            if env::var_os("PKEXEC_UID").is_some() {
                return Err("pkexec authorization is restricted to guest entry".into());
            }
            supervise::serve(&Profile::load(&target.profile)?, &target.instance)
        }
        Action::Reload(target) => {
            if env::var_os("PKEXEC_UID").is_some() {
                return Err("pkexec authorization is restricted to guest entry".into());
            }
            supervise::reload(&Profile::load(&target.profile)?, &target.instance)
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("nook-root: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::path::Path;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn entry_environment_is_only_guest_ui_data() {
        for allowed in [
            "TERM_PROGRAM=terminal",
            "LC_TIME=ja_JP.UTF-8",
            "LANG=C.UTF-8",
        ] {
            assert!(parse_entry_environment(allowed).is_ok());
        }
        for rejected in [
            "HOME=/",
            "LD_PRELOAD=/tmp/library.so",
            "DISPLAY=:77",
            "DBUS_SESSION_BUS_ADDRESS=host",
            "LC_=x",
        ] {
            assert!(parse_entry_environment(rejected).is_err());
        }
    }

    #[test]
    fn entry_directory_is_an_absolute_path_without_parent_traversal() {
        for directory in ["/home/test", "/home/test/Projects/project's %h"] {
            assert_eq!(
                parse_entry_directory(directory).unwrap(),
                Path::new(directory)
            );
        }
        for directory in ["Projects", "/home/test/../other", ""] {
            assert!(parse_entry_directory(directory).is_err());
        }
    }
}
