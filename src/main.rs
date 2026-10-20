#![forbid(unsafe_code)]

mod completion;
mod portal;
mod proxy;
mod tui;

use clap::{Args, Parser, Subcommand};
use nook::{
    config::{self, Profile, Result, catalog, valid_instance},
    storage,
};
use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, ExitStatus},
};

#[derive(Parser)]
#[command(name = "nk", about = "Nook: native systemd sandboxes", version)]
struct Cli {
    /// Profile catalog; defaults to the current owner's installed profiles
    #[arg(long, env = "NOOK_PROFILES", value_hint = clap::ValueHint::DirPath)]
    profiles: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
    #[arg(long, hide = true, num_args = 2, value_names = ["SOCKET", "TARGET"])]
    nix_proxy: Option<Vec<String>>,
    #[arg(long, hide = true)]
    portal_bridge: Option<String>,
    #[arg(long, hide = true)]
    appearance_bridge: Option<String>,
    #[arg(long, hide = true)]
    document_mountpoint: bool,
}

#[derive(Subcommand)]
enum Action {
    /// List configured profiles and running instances
    List,
    /// Launch an instance if necessary, then run a command or shell
    #[command(visible_aliases = ["l", "start", "s"])]
    Launch(Launch),
    /// Launch the profile's default GUI application with optional arguments
    Gui(Launch),
    /// Run the configured shell; used by nk-PROFILE wrappers
    Shell(Launch),
    /// Stop an instance; ephemeral homes are deleted on shutdown
    Stop(Stop),
    /// Permanently delete a stopped, named instance and its saved home
    Delete(Delete),
}

#[derive(Args)]
struct Instance {
    profile: String,
    /// main, n-NAME for a persistent instance, or an existing e-ID
    #[arg(long, default_value = "main", value_parser = parse_instance)]
    instance: String,
    /// Name a persistent instance using this profile as its template
    #[arg(long, conflicts_with = "instance", value_parser = parse_name)]
    name: Option<String>,
}

impl Instance {
    fn id(&self) -> String {
        match &self.name {
            Some(name) => format!("n-{name}"),
            None => self.instance.clone(),
        }
    }
}

#[derive(Args)]
struct Stop {
    #[arg(required_unless_present_any = ["all"], value_name = "PROFILE")]
    profile: Option<String>,
    /// Stop every running instance of PROFILE, or every configured profile if omitted
    #[arg(long)]
    all: bool,
    #[arg(
        long,
        default_value = "main",
        value_parser = parse_instance,
        conflicts_with_all = ["all", "name"]
    )]
    instance: String,
    #[arg(long, conflicts_with_all = ["all", "instance"], value_parser = parse_name)]
    name: Option<String>,
}

impl Stop {
    fn id(&self) -> String {
        match &self.name {
            Some(name) => format!("n-{name}"),
            None => self.instance.clone(),
        }
    }
}

#[derive(Args)]
struct Launch {
    #[command(flatten)]
    selection: Instance,
    /// Create an independent instance with a clean, temporary home
    #[arg(long, conflicts_with_all = ["instance", "name"])]
    ephemeral: bool,
    /// Require an already running instance instead of starting it
    #[arg(long, conflicts_with = "ephemeral")]
    join: bool,
    /// Command after --; GUI and shell wrappers accept application arguments
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Args)]
struct Delete {
    #[command(flatten)]
    selection: Instance,
    /// Skip the interactive confirmation
    #[arg(long, short = 'y')]
    yes: bool,
}

fn require_stopped(profile: &Profile, instance: &str) -> Result<()> {
    // A failed is-active call can mean a missing bus, not a stopped service.
    // Destructive operations require a successful, explicit state query.
    let output = Command::new("systemctl")
        .args([
            "show",
            "--property=ActiveState",
            "--value",
            &profile.unit(instance)?,
        ])
        .output()?;
    if !output.status.success() {
        return Err("cannot verify instance state; refusing deletion".into());
    }
    if String::from_utf8_lossy(&output.stdout).trim() != "inactive" {
        return Err("stop the instance with nk stop before deleting it".into());
    }
    Ok(())
}

fn prepare_deletion(profile: &Profile, instance: &str) -> Result<storage::SavedInstance> {
    let saved = storage::SavedInstance::open(profile, instance)?;
    require_stopped(profile, instance)?;
    Ok(saved)
}

fn delete_instance(
    profile: &Profile,
    instance: &str,
    saved: &storage::SavedInstance,
) -> Result<()> {
    // The supervisor holds this same lock for its entire lifetime. Checking
    // systemd again under the lock covers a launch racing the confirmation.
    let _lock = storage::lock_instance(profile, instance)?;
    require_stopped(profile, instance)?;
    saved.remove()
}

fn confirm_deletion(
    profile: &Profile,
    instance: &str,
    saved: &storage::SavedInstance,
) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err("deletion requires confirmation in a terminal; use --yes for scripts".into());
    }
    eprint!(
        "Permanently delete {}/{instance} and all data in {:?}? [y/N] ",
        profile.get("name")?,
        saved.path,
    );
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn default_profiles() -> PathBuf {
    // Module-generated wrappers select their own catalog. A bare packaged nk
    // instead discovers this owner's profiles in the installed host registry.
    match nix::unistd::User::from_uid(nix::unistd::getuid()) {
        Ok(Some(user)) => {
            PathBuf::from("/etc/sandbox/profiles").join(config::owner_key(&user.name))
        }
        // Return an absent path so catalog loading reports failure, never a
        // different user's profiles or a caller-controlled environment name.
        _ => PathBuf::from("/etc/sandbox/profiles/.unknown-account"),
    }
}

fn parse_instance(value: &str) -> std::result::Result<String, String> {
    if valid_instance(value) {
        Ok(value.to_owned())
    } else {
        Err("use main, n-NAME, or e- followed by 16 lowercase hex digits".into())
    }
}

fn parse_name(value: &str) -> std::result::Result<String, String> {
    if config::valid_name(value) {
        Ok(value.to_owned())
    } else {
        Err("use 1–24 lowercase letters, digits or hyphens".into())
    }
}

fn find_profile<'a>(profiles: &'a [Profile], name: &str) -> Result<&'a Profile> {
    profiles
        .iter()
        .find(|profile| profile.values["name"] == name)
        .ok_or_else(|| format!("unknown sandbox profile: {name}").into())
}

fn control_instance(profile: &Profile, instance: &str, verb: &str) -> Result<ExitStatus> {
    Ok(Command::new("systemctl")
        .args([verb, &profile.unit(instance)?])
        .status()?)
}

fn is_running(profile: &Profile, instance: &str) -> bool {
    profile.unit(instance).is_ok_and(|unit| {
        Command::new("systemctl")
            .args(["is-active", "--quiet", &unit])
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn new_ephemeral_id() -> Result<String> {
    let mut random = [0; 8];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("e-{suffix}"))
}

fn start_instance(profile: &Profile, instance: &str, require_wayland: bool) -> Result<()> {
    let runtime = profile.runtime(instance)?;
    fs::create_dir_all(&runtime)?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;

    // The root supervisor accepts only a socket basename under this owner's
    // runtime directory. Never import DISPLAY, XAUTHORITY or a host socket path.
    let wayland = env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if !wayland
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err("WAYLAND_DISPLAY must name a socket within XDG_RUNTIME_DIR".into());
    }
    if require_wayland && wayland.is_empty() {
        return Err("GUI launch requires a Wayland session".into());
    }
    // Concurrent cold starts must never let the supervisor read a truncated
    // socket name. Publish each complete selection with an atomic rename.
    let mut selection = tempfile::NamedTempFile::new_in(&runtime)?;
    selection.write_all(wayland.as_bytes())?;
    selection.persist(runtime.join("wayland"))?;
    if !control_instance(profile, instance, "start")?.success() {
        let unit = profile.unit(instance)?;
        // Query only on failure: ordinary starts need no extra bus round trip.
        // Home Manager can install nk without installing its host services.
        let state = Command::new("systemctl")
            .args(["show", "--property=LoadState", "--value", &unit])
            .output()?;
        if state.status.success() && String::from_utf8_lossy(&state.stdout).trim() == "not-found" {
            return Err(format!(
                concat!(
                    "no matching host service: {}\n\n",
                    "Rebuild NixOS with nook.nixosModules.default and this user's profiles enabled.\n",
                    "If the module is already imported, check that NixOS and Home Manager ",
                    "use the same Nook version and configuration.\n",
                    "Running home-manager switch alone does not install host services."
                ), unit
            ).into());
        }
        return Err(format!("sandbox startup failed; inspect journalctl -u {unit}").into());
    }
    if profile
        .supervisor_runtime(instance)?
        .join("activation-failed")
        .exists()
    {
        eprintln!(
            concat!(
                "nk: Home Manager activation failed; continuing in recovery mode.\n",
                "Run `nook-home-manager switch -b backup` inside the sandbox, ",
                "then stop and relaunch {}/{}."
            ),
            profile.get("name")?,
            instance
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EntryPoint {
    Command,
    Shell,
    Gui,
}

fn privileged_helper() -> Result<PathBuf> {
    let current = env::current_exe()?;
    let name = current.file_name().and_then(|name| name.to_str());
    let binary = if name == Some(".nk-wrapped") {
        ".nook-root-wrapped"
    } else {
        "nook-root"
    };
    Ok(current
        .parent()
        .ok_or("nk executable has no parent directory")?
        .join(binary))
}

fn launch(
    profile: &Profile,
    instance: &str,
    command: &[String],
    start_if_stopped: bool,
    entry_point: EntryPoint,
) -> Result<i32> {
    let gui = matches!(entry_point, EntryPoint::Gui);
    if !is_running(profile, instance) {
        if !start_if_stopped {
            return Err("instance is not running; omit --join to start it".into());
        }
        if gui && profile.get("gui")?.is_empty() {
            return Err("profile has no default GUI application".into());
        }
        start_instance(profile, instance, gui)?;
    }
    let running = profile.running_profile(instance)?;
    let mut arguments = Vec::new();
    let executable = match entry_point {
        EntryPoint::Command => None,
        EntryPoint::Shell => Some(running.get("shell")?),
        EntryPoint::Gui => Some(running.get("gui")?),
    };
    if let Some(executable) = executable {
        if executable.is_empty() {
            return Err("profile has no default GUI application".into());
        }
        arguments.push(executable.to_owned());
    }
    arguments.extend_from_slice(command);
    // systemd's machine bus transport needs host privileges to enter the
    // machine namespaces. Polkit authorizes the separate, narrow privileged
    // helper, which validates the installed profile and enters as its owner.
    let mut enter = Command::new("/run/wrappers/bin/pkexec");
    enter
        .arg(privileged_helper()?)
        .arg("enter")
        .arg(profile.get("name")?)
        .arg(instance);
    if !gui {
        // pkexec replaces the host working directory. Translate it while still
        // unprivileged, then carry it as data to the guest user service.
        if let Ok(directory) = env::current_dir() {
            enter
                .arg("--entry-directory")
                .arg(running.guest_working_directory(instance, &directory)?);
        }
    }
    enter.arg("--").args(arguments);
    Ok(enter.status()?.code().unwrap_or(1))
}

#[derive(Clone)]
struct Choice {
    profile: usize,
    instance: String,
    running: bool,
}

fn choices(profiles: &[Profile]) -> Result<Vec<Choice>> {
    let output = Command::new("systemctl")
        .args([
            "list-units",
            "--all",
            "--no-legend",
            "--plain",
            "sandbox-*@*.service",
        ])
        .output()?;
    if !output.status.success() {
        return Err("cannot list sandbox services".into());
    }
    let units = String::from_utf8_lossy(&output.stdout);
    let mut choices = Vec::new();
    for (index, profile) in profiles.iter().enumerate() {
        choices.push(Choice {
            profile: index,
            instance: "main".into(),
            running: is_running(profile, "main"),
        });
        // Loaded units disappear after shutdown or a host reboot. The saved
        // homes are the durable catalog for named instances.
        let mut instances: BTreeSet<String> = profile.saved_instances()?.into_iter().collect();
        let prefixes = [
            format!("sandbox-{}-{}@", profile.owner_key()?, profile.get("name")?),
            format!("sandbox-{}-{}@", profile.host_uid()?, profile.get("name")?),
        ];
        for line in units.lines() {
            let instance = line
                .split_whitespace()
                .next()
                .and_then(|unit| prefixes.iter().find_map(|prefix| unit.strip_prefix(prefix)))
                .and_then(|unit| unit.strip_suffix(".service"));
            if let Some(instance) = instance
                && instance != "main"
                && valid_instance(instance)
                && is_running(profile, instance)
            {
                instances.insert(instance.into());
            }
        }
        for instance in instances {
            choices.push(Choice {
                profile: index,
                running: is_running(profile, &instance),
                instance,
            });
        }
    }
    Ok(choices)
}

fn stop_all(profiles: &[Profile], profile: Option<&str>) -> Result<i32> {
    let targets: Vec<Choice> = choices(profiles)?
        .into_iter()
        .filter(|choice| {
            profile.is_none_or(|profile| profiles[choice.profile].values["name"] == profile)
        })
        .collect();
    if targets.is_empty() {
        return Err("no running instances".into());
    }
    let mut exit_code = 0;
    let mut failures = 0;
    for choice in targets {
        let profile = &profiles[choice.profile];
        if !is_running(profile, &choice.instance) {
            continue;
        }
        if !control_instance(profile, &choice.instance, "stop")?.success() {
            exit_code = exit_code.max(1);
            failures += 1;
            eprintln!(
                "nk: failed to stop {}/{instance}",
                profile.values["name"],
                instance = choice.instance
            );
        } else {
            println!(
                "Stopped {}/{instance}",
                profile.values["name"],
                instance = choice.instance
            );
        }
    }
    if failures > 0 {
        eprintln!("nk: {failures} instance stop request(s) failed");
    }
    Ok(exit_code)
}

fn run() -> Result<i32> {
    completion::complete();
    let cli = Cli::parse();
    if let Some(arguments) = cli.nix_proxy {
        return proxy::nix_proxy(&arguments[0], &arguments[1]);
    }
    if let Some(address) = cli.portal_bridge {
        return portal::bridge(&address, false);
    }
    if let Some(address) = cli.appearance_bridge {
        return portal::bridge(&address, true);
    }
    if cli.document_mountpoint {
        return portal::document_mountpoint();
    }
    let profiles = catalog(&cli.profiles.unwrap_or_else(default_profiles))?;
    let Some(action) = cli.command else {
        return match tui::select(&profiles)? {
            Some(choice) => launch(
                &profiles[choice.profile],
                &choice.instance,
                &[],
                true,
                EntryPoint::Command,
            ),
            None => Ok(0),
        };
    };
    match action {
        Action::List => {
            for choice in choices(&profiles)? {
                println!(
                    "{}\t{}\t{}",
                    profiles[choice.profile].get("name")?,
                    choice.instance,
                    if choice.running { "running" } else { "stopped" }
                );
            }
            Ok(0)
        }
        Action::Stop(request) => {
            if request.all {
                stop_all(&profiles, request.profile.as_deref())
            } else {
                let Some(profile_name) = request.profile.as_deref() else {
                    return Err("stop requires a profile, or --all".into());
                };
                let profile = find_profile(&profiles, profile_name)?;
                Ok(control_instance(profile, &request.id(), "stop")?
                    .code()
                    .unwrap_or(1))
            }
        }
        Action::Delete(request) => {
            let profile = find_profile(&profiles, &request.selection.profile)?;
            let instance = request.selection.id();
            let saved = prepare_deletion(profile, &instance)?;
            if request.yes || confirm_deletion(profile, &instance, &saved)? {
                delete_instance(profile, &instance, &saved)?;
                println!("Deleted {}/{instance}", profile.get("name")?);
            }
            Ok(0)
        }
        action => {
            let (request, entry_point) = match action {
                Action::Launch(request) => (request, EntryPoint::Command),
                Action::Gui(request) => (request, EntryPoint::Gui),
                Action::Shell(request) => (request, EntryPoint::Shell),
                _ => unreachable!(),
            };
            let profile = find_profile(&profiles, &request.selection.profile)?;
            let instance = if request.ephemeral {
                new_ephemeral_id()?
            } else {
                request.selection.id()
            };
            launch(
                profile,
                &instance,
                &request.command,
                !request.join,
                entry_point,
            )
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("nk: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
        for command in ["launch", "l", "start", "s"] {
            let cli = Cli::try_parse_from(["nk", command, "dev", "--join"]).unwrap();
            let Some(Action::Launch(request)) = cli.command else {
                panic!("expected launch command");
            };
            assert!(request.join);
        }
        for removed in ["join", "j"] {
            assert!(Cli::try_parse_from(["nk", removed, "dev"]).is_err());
        }
    }

    #[test]
    fn ephemeral_and_named_instances_are_exclusive() {
        assert!(Cli::try_parse_from(["nk", "launch", "dev", "--ephemeral"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "nk",
                "launch",
                "dev",
                "--ephemeral",
                "--instance",
                "n-review"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["nk", "launch", "dev", "--instance", "../dev"]).is_err());
        for arguments in [
            vec!["--name", "project", "--ephemeral"],
            vec!["--name", "project", "--instance", "n-other"],
            vec!["--name", "../escape"],
            vec!["--name", ""],
            vec!["--ephemeral", "--join"],
        ] {
            assert!(
                Cli::try_parse_from(["nk", "launch", "dev"].into_iter().chain(arguments)).is_err()
            );
        }
    }

    #[test]
    fn friendly_names_address_the_same_persistent_instance() {
        let cli = Cli::try_parse_from(["nk", "launch", "dev", "--name", "my-project"]).unwrap();
        let Some(Action::Launch(request)) = cli.command else {
            panic!("expected launch command");
        };
        assert_eq!(request.selection.id(), "n-my-project");

        let cli = Cli::try_parse_from(["nk", "stop", "dev", "--name", "my-project"]).unwrap();
        let Some(Action::Stop(request)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(request.id(), "n-my-project");
    }

    #[test]
    fn stop_all_selects_a_profile_or_everything() {
        let cli = Cli::try_parse_from(["nk", "stop", "dev", "--all"]).unwrap();
        let Some(Action::Stop(request)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(request.profile.as_deref(), Some("dev"));
        assert!(request.all);

        let cli = Cli::try_parse_from(["nk", "stop", "--all"]).unwrap();
        let Some(Action::Stop(request)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(request.profile, None);
        assert!(request.all);

        for arguments in [
            vec!["--all", "--instance", "n-project"],
            vec!["--all", "--name", "project"],
        ] {
            assert!(
                Cli::try_parse_from(["nk", "stop", "dev"].into_iter().chain(arguments.clone()))
                    .is_err(),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn stop_accepts_one_profile_or_all() {
        let cli = Cli::try_parse_from(["nk", "stop", "dev"]).unwrap();
        let Some(Action::Stop(request)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(request.profile.as_deref(), Some("dev"));
        assert!(!request.all);

        let cli = Cli::try_parse_from(["nk", "stop", "--all"]).unwrap();
        let Some(Action::Stop(request)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(request.profile, None);
        assert!(request.all);

        assert!(Cli::try_parse_from(["nk", "stop"]).is_err());
    }
}
