#![forbid(unsafe_code)]

//! Install standalone Home Manager profiles into a systemd host.
//! The unprivileged `current` check makes ordinary HM switches silent. The
//! privileged `install` operation deliberately requires pkexec authorization.

use clap::{Parser, Subcommand};
use nook::{
    config::{Profile, Result, owner_key, valid_name},
    host_paths,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env, fs,
    fs::OpenOptions,
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Parser)]
#[command(about = "Install Nook's host units for standalone Home Manager")]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Succeed if the host already has this exact profile catalog installed.
    Current { username: String, catalog: PathBuf },
    /// Show the immutable catalog and its requested host access.
    Plan { username: String, catalog: PathBuf },
    /// Install profiles after authorization by an administrator.
    Install {
        username: String,
        catalog: PathBuf,
        /// Stage in another root for offline testing; unavailable via pkexec.
        #[arg(long, hide = true)]
        root: Option<PathBuf>,
    },
}

#[derive(Default, Deserialize, Serialize)]
struct Installed {
    catalog: PathBuf,
    profiles: Vec<String>,
    reload_pending: bool,
}

struct DesiredProfile {
    name: String,
    path: PathBuf,
    helper: PathBuf,
    unit: String,
}

fn store_path(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.starts_with("/nix/store/") {
        return Err(format!("expected immutable Nix store path: {}", path.display()).into());
    }
    Ok(canonical)
}

fn safe_exec_path(path: &Path) -> Result<()> {
    let value = path.to_str().ok_or("non-UTF-8 executable path")?;
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b'+')
    }) {
        Ok(())
    } else {
        Err(format!("executable path cannot be represented safely in a unit: {value}").into())
    }
}

fn limits(profile: &Profile) -> Result<String> {
    const NAMES: &[&str] = &[
        "CPU",
        "FSIZE",
        "DATA",
        "STACK",
        "CORE",
        "RSS",
        "NOFILE",
        "AS",
        "NPROC",
        "MEMLOCK",
        "LOCKS",
        "SIGPENDING",
        "MSGQUEUE",
        "NICE",
        "RTPRIO",
        "RTTIME",
    ];
    let mut lines = String::new();
    for line in profile.get("resourceLimits")?.lines() {
        let (name, value) = line.split_once('=').ok_or("invalid resource limit")?;
        if !NAMES.contains(&name)
            || value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'+' | b'-')
            })
        {
            return Err(format!("invalid resource limit {line:?}").into());
        }
        lines.push_str(&format!("Limit{name}={value}\n"));
    }
    Ok(lines)
}

fn unit(profile: &Profile, path: &Path, helper: &Path) -> Result<String> {
    let owner = profile.owner_key()?;
    let name = profile.get("name")?;
    let wrapper = helper.with_file_name("nook-root");
    safe_exec_path(path)?;
    safe_exec_path(&wrapper)?;
    if !wrapper.is_file() {
        return Err(format!("missing root helper wrapper: {}", wrapper.display()).into());
    }
    Ok(format!(
        "[Unit]\nDescription=Nook sandbox {name} (%i)\nWants=systemd-machined.socket\nAfter=systemd-machined.socket\n\
         [Service]\nType=notify\nNotifyAccess=main\nEnvironment=NIX_REMOTE=daemon\n\
         ExecStart={} serve {} %i\nExecReload={} reload {} %i\n\
         RuntimeDirectory=nook-{owner}-{name}-%i\nRuntimeDirectoryMode=0711\n\
         StateDirectory=sandbox/uid-ranges\nPrivateMounts=yes\nDelegate=yes\n\
         DelegateSubgroup=supervisor\nKillMode=mixed\nTimeoutStartSec=120\n\
         TimeoutStopSec=30\nRestart=no\n{}",
        wrapper.display(),
        path.display(),
        wrapper.display(),
        path.display(),
        limits(profile)?,
    ))
}

fn catalog(username: &str, path: &Path) -> Result<(PathBuf, Vec<DesiredProfile>)> {
    let canonical = store_path(path)?;
    let mut profiles = Vec::new();
    let mut names = BTreeSet::new();
    let mut common_helper = None;
    for entry in fs::read_dir(&canonical)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "invalid profile name")?;
        if !valid_name(&name) || !names.insert(name.clone()) {
            return Err(format!("invalid or duplicate profile name {name:?}").into());
        }
        let profile_path = store_path(&entry.path())?;
        // Nix store directories can contain symlinks to arbitrary host files.
        // Do not follow one into a root-only file while parsing as root.
        for file in [
            "profile",
            "mounts",
            "host-environment",
            "session-rules",
            "system-rules",
            "pasta",
        ] {
            let source = store_path(&profile_path.join(file))?;
            if !source.is_file() {
                return Err(format!("profile {name:?} has no regular {file} file").into());
            }
        }
        let profile = Profile::load(&profile_path)?;
        if profile.get("username")? != username || profile.get("name")? != name {
            return Err(format!("profile {name:?} belongs to another owner or name").into());
        }
        let helper = profile.immutable_store_path("supervisorExecutable")?;
        if helper
            .file_name()
            .is_none_or(|name| name != ".nook-root-wrapped")
        {
            return Err("profile names an unexpected supervisor executable".into());
        }
        if common_helper
            .as_ref()
            .is_some_and(|previous| previous != &helper)
        {
            return Err("all profiles of an owner must use one Nook helper version".into());
        }
        common_helper = Some(helper.clone());
        let rendered = unit(&profile, &profile_path, &helper)?;
        profiles.push(DesiredProfile {
            name,
            path: profile_path,
            helper,
            unit: rendered,
        });
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    Ok((canonical, profiles))
}

fn under(root: &Path, absolute: &str) -> PathBuf {
    root.join(absolute.trim_start_matches('/'))
}

fn marker(root: &Path, owner: &str) -> PathBuf {
    under(root, "/etc/sandbox/arch-managed").join(format!("{owner}.json"))
}

fn installed(root: &Path, owner: &str) -> Result<Option<Installed>> {
    match fs::read(marker(root, owner)) {
        Ok(contents) => {
            let record: Installed = serde_json::from_slice(&contents)?;
            if record.profiles.iter().any(|name| !valid_name(name)) {
                return Err("invalid installed Nook profile record".into());
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn plan(username: &str, path: &Path) -> Result<()> {
    let owner = owner_key(username);
    let (catalog, profiles) = catalog(username, path)?;
    let old = installed(Path::new("/"), &owner)?.unwrap_or_default();
    let new_names: BTreeSet<_> = profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    println!("Nook host update for {username:?}");
    println!("Catalog: {}", catalog.display());
    for name in &old.profiles {
        if !new_names.contains(name.as_str()) {
            println!("  - {name}: remove host template and profile registration");
        }
    }
    for desired in &profiles {
        let registered = Path::new("/etc/sandbox/profiles")
            .join(&owner)
            .join(&desired.name);
        if fs::canonicalize(&registered).is_ok_and(|path| path == desired.path) {
            continue;
        }
        let action = if old.profiles.contains(&desired.name) {
            '~'
        } else {
            '+'
        };
        let profile = Profile::load(&desired.path)?;
        println!("  {action} {}: {}", desired.name, desired.path.display());
        for permission in [
            "network",
            "portals",
            "inheritAppearance",
            "hostSessionBus",
            "nix",
            "podman",
        ] {
            if profile.flag(permission) {
                println!("      {permission}=true");
            }
        }
        for mount in &profile.mounts {
            println!(
                "      {} {:?} -> {:?}",
                mount.mode,
                mount.source.to_string(),
                mount.target
            );
        }
        if !profile.system_rules.is_empty() {
            println!("      host system D-Bus rules: {:?}", profile.system_rules);
        }
        if !profile.session_rules.is_empty() {
            println!(
                "      host session D-Bus rules: {:?}",
                profile.session_rules
            );
        }
        if !profile.get("extraSystemCalls")?.is_empty() {
            println!(
                "      extra syscalls: {:?}",
                profile.get("extraSystemCalls")?
            );
        }
        if !profile.get("resourceLimits")?.is_empty() {
            println!(
                "      resource limits: {}",
                profile.get("resourceLimits")?.replace('\n', ", ")
            );
        }
    }
    Ok(())
}

fn trusted_directory(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err("invalid host directory component".into());
            }
            current.push(component);
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o755))?;
                fs::symlink_metadata(&current)?
            }
            Err(error) => return Err(error.into()),
        };
        // Every parent is trusted before descending. Unlike a leaf-only check,
        // this rejects an /etc symlink through a user-writable ancestor.
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "host directory must be root-owned, real, and not writable: {}",
                current.display()
            )
            .into());
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("missing destination parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

fn symlink_atomic(path: &Path, target: &Path) -> Result<()> {
    let parent = path.parent().ok_or("missing destination parent")?;
    let temporary = tempfile::Builder::new()
        .prefix(".nook-")
        .tempfile_in(parent)?;
    let temporary_path = temporary.path().to_path_buf();
    drop(temporary);
    symlink(target, &temporary_path)?;
    fs::rename(temporary_path, path)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn polkit_rule(username: &str, owner: &str, profiles: &[DesiredProfile]) -> Result<String> {
    let user = serde_json::to_string(username)?;
    let helper = profiles.first().map(|profile| &profile.helper);
    let mut rule = String::from(
        "// Generated by nook-host-install.\npolkit.addRule(function(action, subject) {\n",
    );
    rule.push_str(&format!("  if (subject.user !== {user}) return;\n"));
    if let Some(helper) = helper {
        rule.push_str("  if (action.id === 'org.freedesktop.policykit.exec' &&\n");
        rule.push_str(&format!(
            "      (action.lookup('program') === {} || action.lookup('program') === {})) return polkit.Result.YES;\n",
            serde_json::to_string(&format!("/etc/sandbox/helpers/{owner}"))?,
            serde_json::to_string(&helper.to_string_lossy())?,
        ));
    }
    rule.push_str("  if (action.id !== 'org.freedesktop.systemd1.manage-units') return;\n");
    rule.push_str("  var unit = action.lookup('unit');\n  var verb = action.lookup('verb');\n");
    rule.push_str(
        "  if (!unit || (verb !== 'start' && verb !== 'stop' && verb !== 'restart')) return;\n",
    );
    for profile in profiles {
        let expression = format!(
            "^sandbox-{owner}-{}@(main|n-[a-z0-9-]{{1,24}}|e-[a-f0-9]{{16}})\\.service$",
            profile.name
        );
        rule.push_str(&format!(
            "  if (new RegExp({}).test(unit)) return polkit.Result.YES;\n",
            serde_json::to_string(&expression)?
        ));
    }
    rule.push_str("});\n");
    Ok(rule)
}

fn install(username: &str, catalog_path: &Path, root: &Path) -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return Err("host installation requires administrator authentication".into());
    }
    if root != Path::new("/") && env::var_os("PKEXEC_UID").is_some() {
        return Err("staging another root is unavailable through pkexec".into());
    }
    let account = nix::unistd::User::from_name(username)?.ok_or("host account does not exist")?;
    if account.uid.is_root() || account.gid.as_raw() == 0 {
        return Err("Nook owner must be a non-root account".into());
    }
    if let Some(caller) = env::var_os("PKEXEC_UID") {
        if caller.to_string_lossy().parse::<u32>()? != account.uid.as_raw() {
            return Err("authenticated caller does not own these profiles".into());
        }
    }
    let owner = owner_key(username);
    let (catalog, profiles) = catalog(username, catalog_path)?;
    let unit_dir = under(root, "/etc/systemd/system");
    let profile_dir = under(root, "/etc/sandbox/profiles").join(&owner);
    let helper_dir = under(root, "/etc/sandbox/helpers");
    let policy_dir = under(root, "/etc/polkit-1/rules.d");
    let marker_dir = under(root, "/etc/sandbox/arch-managed");
    for directory in [
        &unit_dir,
        &profile_dir,
        &helper_dir,
        &policy_dir,
        &marker_dir,
    ] {
        trusted_directory(root, directory)?;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .open(marker_dir.join(format!("{owner}.lock")))?;
    lock.lock()?;
    // Read the previous generation only after taking the per-owner lock;
    // concurrent Home Manager switches must not remove each other's units.
    let old = installed(root, &owner)?.unwrap_or_default();
    let old_names: BTreeSet<_> = old.profiles.iter().cloned().collect();
    let new_names: BTreeSet<_> = profiles.iter().map(|p| p.name.clone()).collect();
    let unit_path = |name: &str| unit_dir.join(format!("sandbox-{owner}-{name}@.service"));
    let policy_path = policy_dir.join(format!("49-nook-{owner}.rules"));
    let helper_path = helper_dir.join(&owner);
    // A fresh installation must never replace an unrelated system service,
    // policy, or profile. Subsequent switches may replace only our own record.
    for profile in &profiles {
        if !old_names.contains(&profile.name)
            && (entry_exists(&unit_path(&profile.name))?
                || entry_exists(&profile_dir.join(&profile.name))?)
        {
            return Err(format!("host files already exist for profile {}", profile.name).into());
        }
    }
    if old.catalog.as_os_str().is_empty()
        && (entry_exists(&helper_path)? || entry_exists(&policy_path)?)
    {
        return Err("host helper or Polkit rule already exists".into());
    }
    let save = |record: &Installed| -> Result<()> {
        write_atomic(&marker(root, &owner), &serde_json::to_vec(record)?)
    };
    // Once collision checks pass, record ownership of every path we may touch.
    // A failed write or interrupted switch can then be retried without treating
    // our partially installed files as somebody else's host configuration.
    save(&Installed {
        catalog: catalog.clone(),
        profiles: old_names.union(&new_names).cloned().collect(),
        reload_pending: true,
    })?;
    for profile in &profiles {
        write_atomic(&unit_path(&profile.name), profile.unit.as_bytes())?;
        symlink_atomic(&profile_dir.join(&profile.name), &profile.path)?;
    }
    for name in old_names.difference(&new_names) {
        remove_if_present(&unit_path(name))?;
        remove_if_present(&profile_dir.join(name))?;
    }
    if let Some(profile) = profiles.first() {
        symlink_atomic(&helper_path, &profile.helper)?;
        write_atomic(
            &policy_path,
            polkit_rule(username, &owner, &profiles)?.as_bytes(),
        )?;
    } else if !old.catalog.as_os_str().is_empty() {
        remove_if_present(&helper_path)?;
        remove_if_present(&policy_path)?;
    }
    let mut record = Installed {
        catalog,
        profiles: new_names.into_iter().collect(),
        reload_pending: root == Path::new("/"),
    };
    save(&record)?;
    if root == Path::new("/") {
        let status = Command::new(host_paths::systemctl())
            .arg("daemon-reload")
            .status()?;
        if !status.success() {
            return Err("systemd daemon-reload failed; rerun Home Manager activation".into());
        }
        record.reload_pending = false;
        save(&record)?;
    }
    Ok(())
}

fn run() -> Result<i32> {
    match Cli::parse().action {
        Action::Current {
            username,
            catalog: path,
        } => {
            let desired = store_path(&path)?;
            let owner = owner_key(&username);
            let old = installed(Path::new("/"), &owner)?;
            // Polkit's rules directory can be root-only on Arch. The
            // root-owned record attests that the rule was installed; this
            // unprivileged fast path checks only files it may read.
            Ok(
                if old.is_some_and(|record| {
                    !record.reload_pending
                        && record.catalog == desired
                        && (record.profiles.is_empty()
                            || Path::new("/etc/sandbox/helpers").join(&owner).is_file())
                        && record.profiles.iter().all(|name| {
                            Path::new("/etc/systemd/system")
                                .join(format!("sandbox-{owner}-{name}@.service"))
                                .is_file()
                                && Path::new("/etc/sandbox/profiles")
                                    .join(&owner)
                                    .join(name)
                                    .exists()
                        })
                }) {
                    0
                } else {
                    1
                },
            )
        }
        Action::Plan { username, catalog } => {
            plan(&username, &catalog)?;
            Ok(0)
        }
        Action::Install {
            username,
            catalog,
            root,
        } => {
            install(
                &username,
                &catalog,
                root.as_deref().unwrap_or(Path::new("/")),
            )?;
            Ok(0)
        }
    }
}

fn main() {
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("nook-host-install: {error}");
            std::process::exit(2);
        }
    }
}
