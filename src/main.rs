#![forbid(unsafe_code)]

mod config;
mod portal;
mod supervise;
mod tui;

use clap::{Args, Parser, Subcommand};
use config::{Profile, Result, catalog, valid_instance};
use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus},
};

#[derive(Parser)]
#[command(name = "nk", about = "Nook: native systemd sandboxes", version)]
struct Cli {
    #[arg(long, env = "NOOK_PROFILES", default_value_os_t = default_profiles())]
    profiles: PathBuf,
    #[command(subcommand)]
    command: Option<Action>,
    // Only installed system services invoke these entry points. The supervisor
    // independently checks root credentials and immutable profile provenance.
    #[arg(long, hide = true, num_args = 2, value_names = ["PROFILE", "INSTANCE"])]
    serve: Option<Vec<String>>,
    #[arg(long, hide = true, num_args = 2, value_names = ["SOCKET", "TARGET"])]
    nix_proxy: Option<Vec<String>>,
    #[arg(long, hide = true, num_args = 2, value_names = ["PROFILE", "INSTANCE"])]
    enter: Option<Vec<String>>,
    #[arg(long, hide = true)]
    portal_bridge: Option<String>,
    #[arg(long, hide = true)]
    appearance_bridge: Option<String>,
    #[arg(long, hide = true)]
    document_mountpoint: bool,
    #[arg(long, hide = true, value_parser = parse_entry_environment)]
    entry_environment: Vec<String>,
    #[arg(long, hide = true, value_parser = parse_entry_directory)]
    entry_directory: Option<PathBuf>,
    #[arg(last = true, hide = true)]
    enter_command: Vec<String>,
}

#[derive(Subcommand)]
enum Action {
    /// List configured profiles and running instances
    List,
    /// Start an instance if necessary, then run a command or shell
    #[command(visible_alias = "l")]
    Launch(Launch),
    /// Join an instance that is already running
    #[command(visible_alias = "j")]
    Join(Launch),
    /// Launch the profile's default GUI application with optional arguments
    Gui(Launch),
    /// Run the configured shell; used by nk-PROFILE wrappers
    Shell(Launch),
    /// Stop an instance; ephemeral homes are deleted on shutdown
    Stop(Instance),
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
struct Launch {
    #[command(flatten)]
    selection: Instance,
    /// Create an independent instance with a clean, temporary home
    #[arg(long, conflicts_with_all = ["instance", "name"])]
    ephemeral: bool,
    /// Command after --; GUI and shell wrappers accept application arguments
    #[arg(last = true)]
    command: Vec<String>,
}

fn default_profiles() -> PathBuf {
    // Module-generated wrappers select their own catalog. A bare packaged nk
    // instead discovers this owner's profiles in the installed host registry.
    PathBuf::from(format!("/etc/sandbox/profiles/{}", nix::unistd::getuid()))
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

fn parse_entry_directory(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() && !path.components().any(|part| part == Component::ParentDir) {
        Ok(path)
    } else {
        Err("entry directory must be absolute and contain no parent traversal".into())
    }
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
        return Err(format!(
            "sandbox startup failed; inspect journalctl -u {}",
            profile.unit(instance)?
        )
        .into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EntryPoint {
    Command,
    Shell,
    Gui,
}

fn launch(
    profile: &Profile,
    instance: &str,
    command: &[String],
    start: bool,
    entry_point: EntryPoint,
) -> Result<i32> {
    let gui = matches!(entry_point, EntryPoint::Gui);
    if !is_running(profile, instance) {
        if !start {
            return Err("instance is not running; use launch to start it".into());
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
    // machine namespaces. Polkit authorizes this fixed helper; the helper
    // validates the installed profile and always enters as its owner.
    let mut enter = Command::new("/run/wrappers/bin/pkexec");
    enter
        .arg(env::current_exe()?)
        .arg("--enter")
        .arg(&profile.dir)
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
    // pkexec intentionally sanitizes its environment. Carry the small UI
    // allowlist as validated data, then set it only on the guest user service.
    for (key, value) in env::vars().filter(|(key, _)| allowed_entry_environment(key)) {
        enter
            .arg("--entry-environment")
            .arg(format!("{key}={value}"));
    }
    enter.arg("--").args(arguments);
    Ok(enter.status()?.code().unwrap_or(1))
}

fn enter_instance(
    profile: &Profile,
    instance: &str,
    command: &[String],
    environment: &[String],
    directory: Option<&Path>,
) -> Result<i32> {
    if !nix::unistd::geteuid().is_root() {
        return Err("entry helper requires polkit authorization".into());
    }
    let caller: u32 = env::var("PKEXEC_UID")?.parse()?;
    if caller != profile.uid()? {
        return Err("caller does not own this sandbox profile".into());
    }
    let registered = PathBuf::from(format!(
        "/etc/sandbox/profiles/{}/{}",
        caller,
        profile.get("name")?
    ));
    let declared = fs::canonicalize(registered)?;
    if !declared.starts_with("/nix/store") || declared != fs::canonicalize(&profile.dir)? {
        return Err("entry helper accepts only profiles installed by the host module".into());
    }
    // Authorization uses today's installed profile. Entry must use the guest's
    // pinned generation, which can intentionally outlive a host rebuild.
    let running = profile.running_profile(instance)?;
    let profile = &running;
    let home = Path::new(profile.get("home")?);
    let directory = directory.unwrap_or(home);
    if !directory.starts_with(home) {
        return Err("entry directory must be within the selected sandbox's home".into());
    }
    let default_shell = [profile.get("shell")?.to_owned()];
    let command = if command.is_empty() {
        &default_shell
    } else {
        command
    };
    let machine = format!(
        "{}@{}",
        profile.get("username")?,
        profile.machine(instance)?
    );
    let mut run = Command::new("systemd-run");
    run.args([
        "--user",
        "--machine",
        &machine,
        "--quiet",
        "--wait",
        "--collect",
        "--service-type=exec",
        "--expand-environment=no",
    ]);
    run.arg(if io::stdin().is_terminal() && io::stdout().is_terminal() {
        "--pty"
    } else {
        "--pipe"
    });
    run.arg("--working-directory").arg(directory);
    for value in environment {
        run.arg(format!("--setenv={value}"));
    }
    run.arg("--").arg(profile.get("entry")?).args(command);
    Ok(run.status()?.code().unwrap_or(1))
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
        let prefix = format!("sandbox-{}-{}@", profile.uid()?, profile.get("name")?);
        for line in units.lines() {
            let instance = line
                .split_whitespace()
                .next()
                .and_then(|unit| unit.strip_prefix(&prefix))
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

fn run() -> Result<i32> {
    let cli = Cli::parse();
    if let Some(arguments) = cli.enter {
        return enter_instance(
            &Profile::load(&PathBuf::from(&arguments[0]))?,
            &arguments[1],
            &cli.enter_command,
            &cli.entry_environment,
            cli.entry_directory.as_deref(),
        );
    }
    // Authorization to join is never authorization to run a root supervisor,
    // proxy, or another CLI command with caller-controlled configuration.
    if env::var_os("PKEXEC_UID").is_some() {
        return Err("polkit execution is restricted to the validated entry helper".into());
    }
    if let Some(arguments) = cli.serve {
        return supervise::serve(
            &Profile::load(&PathBuf::from(&arguments[0]))?,
            &arguments[1],
        );
    }
    if let Some(arguments) = cli.nix_proxy {
        return supervise::nix_proxy(&arguments[0], &arguments[1]);
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
    let profiles = catalog(&cli.profiles)?;
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
        Action::Stop(selection) => {
            let profile = find_profile(&profiles, &selection.profile)?;
            Ok(control_instance(profile, &selection.id(), "stop")?
                .code()
                .unwrap_or(1))
        }
        action => {
            let (request, start, entry_point) = match action {
                Action::Launch(request) => (request, true, EntryPoint::Command),
                Action::Join(request) => (request, false, EntryPoint::Command),
                Action::Gui(request) => (request, true, EntryPoint::Gui),
                Action::Shell(request) => (request, true, EntryPoint::Shell),
                _ => unreachable!(),
            };
            let profile = find_profile(&profiles, &request.selection.profile)?;
            let instance = if request.ephemeral {
                new_ephemeral_id()?
            } else {
                request.selection.id()
            };
            launch(profile, &instance, &request.command, start, entry_point)
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
        let launch = Cli::try_parse_from(["nk", "l", "dev"]).unwrap();
        assert!(matches!(launch.command, Some(Action::Launch(_))));
        let join = Cli::try_parse_from(["nk", "j", "dev"]).unwrap();
        assert!(matches!(join.command, Some(Action::Join(_))));
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
        let Some(Action::Stop(selection)) = cli.command else {
            panic!("expected stop command");
        };
        assert_eq!(selection.id(), "n-my-project");
    }
}
