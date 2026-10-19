#![forbid(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use nook::{
    config::{Profile, Result, owner_key, valid_name},
    supervise,
};
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
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
    #[arg(value_parser = parse_profile_name)]
    profile: String,
    instance: String,
    #[arg(long, value_parser = parse_entry_directory)]
    entry_directory: Option<PathBuf>,
    #[arg(last = true)]
    command: Vec<String>,
}

fn parse_profile_name(value: &str) -> std::result::Result<String, String> {
    if valid_name(value) {
        Ok(value.to_owned())
    } else {
        Err("profile must be 1–24 lowercase letters, digits or hyphens".into())
    }
}

fn parse_entry_directory(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() && !path.components().any(|part| part == Component::ParentDir) {
        Ok(path)
    } else {
        Err("entry directory must be absolute and contain no parent traversal".into())
    }
}

fn installed_entry_profile(name: &str) -> Result<Profile> {
    let caller: u32 = env::var("PKEXEC_UID")?.parse()?;
    let account = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(caller))?
        .ok_or("authenticated caller has no host account")?;
    if account.uid.is_root() || account.gid.as_raw() == 0 {
        return Err("sandbox owner must have non-root UID and primary GID".into());
    }
    let registered = Path::new("/etc/sandbox/profiles")
        .join(owner_key(&account.name))
        .join(name);
    let declared = fs::canonicalize(&registered).map_err(|error| {
        format!(
            "cannot resolve installed profile {}: {error}",
            registered.display()
        )
    })?;
    if !declared.starts_with("/nix/store/") {
        return Err("entry accepts only profiles installed by the host module".into());
    }
    // Load only after the caller's UID and bounded profile name have selected
    // a root-managed registry entry. A pkexec client never supplies a path for
    // the privileged process to open.
    let profile = Profile::load(&declared)
        .map_err(|error| format!("cannot load installed profile {name}: {error}"))?;
    if profile.get("name")? != name
        || profile.get("username")? != account.name
        || profile.host_uid()? != caller
    {
        return Err("installed profile does not belong to the authenticated caller".into());
    }
    Ok(profile)
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
            let profile = installed_entry_profile(&request.profile)?;
            supervise::enter(
                &profile,
                &request.instance,
                &request.command,
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
    fn entry_accepts_a_profile_name_not_a_path() {
        assert_eq!(parse_profile_name("dev-1").unwrap(), "dev-1");
        for rejected in ["", "../dev", "/nix/store/profile", "Dev", "dev_name"] {
            assert!(parse_profile_name(rejected).is_err());
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
