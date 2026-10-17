use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Debug)]
pub struct Profile {
    pub dir: PathBuf,
    pub values: BTreeMap<String, String>,
    pub mounts: Vec<Mount>,
    pub environment: BTreeMap<String, String>,
    pub session_rules: Vec<String>,
    pub system_rules: Vec<String>,
    pub pasta: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Mount {
    pub mode: String,
    pub source: String,
    pub target: String,
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 24
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn valid_instance(s: &str) -> bool {
    s == "main"
        || s.strip_prefix("e-").is_some_and(|x| {
            x.len() == 16
                && x.bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        || s.strip_prefix("n-").is_some_and(valid_name)
}
pub fn safe_mount(source: &str, target: &str) -> Result<()> {
    for path in [source, target] {
        let p = Path::new(path);
        if !p.is_absolute()
            || path.contains(['\n', '\r', '\t', ':', '\\'])
            || p.components()
                .any(|x| matches!(x, std::path::Component::ParentDir))
        {
            return Err(format!("invalid bind path: {path}").into());
        }
        if ["/", "/tmp", "/run", "/run/user", "/dev", "/proc", "/sys"].contains(&path)
            || path.contains(".X11-unix")
            || path.contains(".Xauthority")
        {
            return Err(format!("unsafe broad or X11 bind: {path}").into());
        }
        if path.starts_with("/run/user/") && p.components().count() == 4 {
            return Err("binding a complete host runtime directory is forbidden".into());
        }
    }
    Ok(())
}
fn lines(path: &Path) -> Result<Vec<String>> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}
fn pairs(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for line in lines(path)? {
        let (k, v) = line
            .split_once('\t')
            .ok_or("expected tab-separated key and value")?;
        if out.insert(k.to_string(), v.to_string()).is_some() {
            return Err(format!("duplicate key {k}").into());
        }
    }
    Ok(out)
}
impl Profile {
    pub fn running_profile(&self, instance: &str) -> Result<Self> {
        // This GC root is published by the root supervisor in its root-owned
        // RuntimeDirectory, not in the owner's writable session directory.
        let directory = fs::canonicalize(
            Path::new("/run")
                .join(self.machine(instance)?)
                .join("profile-root"),
        )?;
        if !directory.starts_with("/nix/store") {
            return Err("running profile must be an immutable Nix store path".into());
        }
        let running = Self::load(&directory)?;
        self.check_running_identity(&running)?;
        Ok(running)
    }

    fn check_running_identity(&self, running: &Self) -> Result<()> {
        if running.uid()? != self.uid()? || running.get("name")? != self.get("name")? {
            return Err("running profile belongs to a different sandbox or owner".into());
        }
        Ok(())
    }

    pub fn guest_working_directory(&self, instance: &str, directory: &Path) -> Result<PathBuf> {
        let home = PathBuf::from(self.get("home")?);
        let Some(host_home) = self.persistent_home(instance)? else {
            return Ok(home);
        };
        // Resolve aliases such as ~/Projects before comparing paths. Only map
        // this instance's own backing home; matching another instance's home
        // would imply access to files that are deliberately not shared.
        if let (Ok(host_home), Ok(directory)) =
            (fs::canonicalize(host_home), fs::canonicalize(directory))
            && let Ok(relative) = directory.strip_prefix(host_home)
        {
            return Ok(home.join(relative));
        }
        Ok(home)
    }

    pub fn persistent_home(&self, instance: &str) -> Result<Option<PathBuf>> {
        if !valid_instance(instance) {
            return Err("invalid sandbox instance".into());
        }
        if instance.starts_with("e-") || (instance == "main" && !self.flag("persistent")) {
            return Ok(None);
        }
        let state = PathBuf::from(self.get("state")?);
        // A named instance explicitly requests persistence, even when the
        // template's main instance normally has a temporary home.
        Ok(Some(if instance == "main" {
            state.join("home")
        } else {
            state.join("instances").join(instance).join("home")
        }))
    }

    pub fn saved_instances(&self) -> Result<Vec<String>> {
        let directory = Path::new(self.get("state")?).join("instances");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut instances = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("n-")
                && valid_instance(&name)
                && entry.file_type()?.is_dir()
                && fs::symlink_metadata(entry.path().join("home"))
                    .is_ok_and(|metadata| metadata.is_dir())
            {
                instances.push(name);
            }
        }
        instances.sort();
        Ok(instances)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let values = pairs(&dir.join("profile"))?;
        let mut mounts = Vec::new();
        for line in lines(&dir.join("mounts"))? {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != 3 || !["ro", "rw", "device", "device-pattern"].contains(&fields[0]) {
                return Err("invalid mount record".into());
            }
            safe_mount(fields[1], fields[2])?;
            if fields[0] == "device-pattern" {
                let name = fields[1]
                    .strip_prefix("/dev/")
                    .ok_or("device patterns must be under /dev")?;
                if name.contains('/') || name == "*" || name.is_empty() {
                    return Err(
                        "device patterns must select named top-level character devices".into(),
                    );
                }
                glob::Pattern::new(fields[1])?;
            }
            mounts.push(Mount {
                mode: fields[0].into(),
                source: fields[1].into(),
                target: fields[2].into(),
            });
        }
        let p = Self {
            dir: dir.into(),
            values,
            mounts,
            environment: pairs(&dir.join("environment"))?,
            session_rules: lines(&dir.join("session-rules"))?,
            system_rules: lines(&dir.join("system-rules"))?,
            pasta: lines(&dir.join("pasta"))?,
        };
        if !valid_name(p.get("name")?) {
            return Err("invalid profile name".into());
        }
        for name in ["uid", "gid"] {
            let _: u32 = p.get(name)?.parse()?;
        }
        if p.uid()? == 0 || p.uid()? >= 100000 || p.gid()? == 0 || p.gid()? >= 100000 {
            return Err("sandbox owner UID/GID must be between 1 and 99999".into());
        }
        for key in p.environment.keys() {
            if !key
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                || [
                    "DISPLAY",
                    "XAUTHORITY",
                    "DBUS_SESSION_BUS_ADDRESS",
                    "DBUS_SYSTEM_BUS_ADDRESS",
                    "WAYLAND_DISPLAY",
                    "XDG_RUNTIME_DIR",
                    "HOME",
                ]
                .contains(&key.as_str())
            {
                return Err(format!("reserved or invalid environment variable {key}").into());
            }
        }
        Ok(p)
    }
    pub fn get(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("missing profile key {key}").into())
    }
    pub fn flag(&self, key: &str) -> bool {
        self.values.get(key).is_some_and(|x| x == "1")
    }
    pub fn uid(&self) -> Result<u32> {
        Ok(self.get("uid")?.parse()?)
    }
    pub fn gid(&self) -> Result<u32> {
        Ok(self.get("gid")?.parse()?)
    }
    pub fn machine(&self, instance: &str) -> Result<String> {
        if !valid_instance(instance) {
            return Err(
                "invalid instance: use main, n-NAME, or e- followed by 16 hex digits".into(),
            );
        }
        Ok(format!(
            "sbx-{}-{}-{}",
            self.uid()?,
            self.get("name")?,
            instance
        ))
    }
    pub fn unit(&self, instance: &str) -> Result<String> {
        self.machine(instance)?;
        Ok(format!(
            "sandbox-{}-{}@{instance}.service",
            self.uid()?,
            self.get("name")?
        ))
    }
    pub fn runtime(&self, instance: &str) -> Result<PathBuf> {
        self.machine(instance)?;
        Ok(PathBuf::from(format!(
            "/run/user/{}/sandbox/{}/{instance}",
            self.uid()?,
            self.get("name")?
        )))
    }
}
pub fn catalog(dir: &Path) -> Result<Vec<Profile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        out.push(Profile::load(&entry?.path())?);
    }
    out.sort_by_key(|p| p.values["name"].clone());
    Ok(out)
}
pub fn error(message: &str) -> io::Error {
    io::Error::other(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(state: &Path, persistent: bool) -> Profile {
        Profile {
            dir: PathBuf::new(),
            values: BTreeMap::from([
                ("state".into(), state.to_string_lossy().into_owned()),
                ("home".into(), "/home/test".into()),
                ("name".into(), "dev".into()),
                ("uid".into(), "1000".into()),
                (
                    "persistent".into(),
                    if persistent { "1" } else { "0" }.into(),
                ),
            ]),
            mounts: Vec::new(),
            environment: BTreeMap::new(),
            session_rules: Vec::new(),
            system_rules: Vec::new(),
            pasta: Vec::new(),
        }
    }

    #[test]
    fn running_generation_may_differ_but_owner_and_profile_must_match() {
        let installed = profile(Path::new("/new-home"), true);
        let mut running = profile(Path::new("/old-home"), true);
        assert!(installed.check_running_identity(&running).is_ok());
        running.values.insert("uid".into(), "1001".into());
        assert!(installed.check_running_identity(&running).is_err());
        running.values.insert("uid".into(), "1000".into());
        running.values.insert("name".into(), "vr".into());
        assert!(installed.check_running_identity(&running).is_err());
    }

    #[test]
    fn working_directory_maps_physical_paths_and_symlink_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("dev");
        let profile = profile(&state, true);
        let project = state.join("home/Projects/project's %h/subdir");
        fs::create_dir_all(&project).unwrap();
        let alias = directory.path().join("Projects");
        std::os::unix::fs::symlink(state.join("home/Projects"), &alias).unwrap();
        for path in [project, alias.join("project's %h/subdir")] {
            assert_eq!(
                profile.guest_working_directory("main", &path).unwrap(),
                Path::new("/home/test/Projects/project's %h/subdir"),
            );
        }
        assert_eq!(
            profile
                .guest_working_directory("main", &state.join("home"))
                .unwrap(),
            Path::new("/home/test"),
        );
    }

    #[test]
    fn working_directory_does_not_cross_instance_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("dev");
        let profile = profile(&state, true);
        let main_project = state.join("home/Projects/main");
        let named_project = state.join("instances/n-project/home/Projects/named");
        let outside = state.join("home-other/Projects");
        for path in [&main_project, &named_project, &outside] {
            fs::create_dir_all(path).unwrap();
        }
        let escape = state.join("home/outside");
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        for (instance, path) in [
            ("main", &named_project),
            ("n-project", &main_project),
            ("e-0123456789abcdef", &main_project),
            ("main", &outside),
            ("main", &escape),
        ] {
            assert_eq!(
                profile.guest_working_directory(instance, path).unwrap(),
                Path::new("/home/test"),
            );
        }
        assert_eq!(
            profile
                .guest_working_directory("n-project", &named_project)
                .unwrap(),
            Path::new("/home/test/Projects/named"),
        );
    }

    #[test]
    fn named_homes_persist_even_for_temporary_templates() {
        let state = Path::new("/home/test/.sandbox/dev");
        for persistent in [false, true] {
            let profile = profile(state, persistent);
            assert_eq!(
                profile.persistent_home("n-project").unwrap(),
                Some(state.join("instances/n-project/home")),
            );
            assert_eq!(
                profile.persistent_home("main").unwrap(),
                persistent.then(|| state.join("home")),
            );
            assert_eq!(profile.persistent_home("e-0123456789abcdef").unwrap(), None);
            assert!(profile.persistent_home("../escape").is_err());
        }
    }

    #[test]
    fn saved_instance_discovery_ignores_unrelated_entries() {
        let directory = tempfile::tempdir().unwrap();
        let profile = profile(directory.path(), false);
        assert!(profile.saved_instances().unwrap().is_empty());
        for instance in ["n-zeta", "n-alpha", "n-", "e-0123456789abcdef", "main"] {
            fs::create_dir_all(
                directory
                    .path()
                    .join("instances")
                    .join(instance)
                    .join("home"),
            )
            .unwrap();
        }
        fs::create_dir_all(directory.path().join("instances/n-no-home")).unwrap();
        fs::write(directory.path().join("instances/n-file"), "not an instance").unwrap();
        assert_eq!(profile.saved_instances().unwrap(), ["n-alpha", "n-zeta"]);
    }

    #[test]
    fn instance_names_cannot_inject_options_or_paths() {
        for bad in [
            "",
            "../dev",
            "e-123",
            "--host",
            "n-../../",
            "n-",
            "main.service",
            "e-AAAAAAAAAAAAAAAA",
        ] {
            assert!(!valid_instance(bad), "{bad}");
        }
        for good in ["main", "n-review", "e-0123456789abcdef"] {
            assert!(valid_instance(good));
        }
    }
    #[test]
    fn host_x11_and_broad_mounts_are_rejected() {
        for bad in [
            "/tmp",
            "/tmp/.X11-unix/X0",
            "/run/user/1000",
            "/home/me/.Xauthority",
            "/dev",
            "/a/../tmp",
            "/path:options",
        ] {
            assert!(safe_mount(bad, "/allowed").is_err(), "{bad}");
        }
        assert!(safe_mount("/home/me/Projects", "/home/me/Projects").is_ok());
        assert!(safe_mount("/run/user/1000/wayland-0", "/run/host/wayland-0").is_ok());
    }
}
