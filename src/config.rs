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
            || path.contains(['\0', '\n', '\r', '\t', ':', '\\'])
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
    let contents = fs::read_to_string(path)?;
    let mut out = BTreeMap::new();
    let framed = contents.starts_with("nook-pairs-v1\n");
    let mut remaining = contents
        .strip_prefix("nook-pairs-v1\n")
        .unwrap_or(&contents);
    while !remaining.is_empty() {
        let (key, rest) = remaining
            .split_once('\t')
            .ok_or("expected tab-separated key and value")?;
        if key.is_empty() || key.contains(['\n', '\r', '\t']) {
            return Err(format!("invalid profile key {key:?}").into());
        }
        let value;
        if framed {
            let (length, rest) = rest.split_once('\t').ok_or("missing value length")?;
            let length: usize = length.parse()?;
            value = rest.get(..length).ok_or("invalid profile value length")?;
            remaining = rest[length..]
                .strip_prefix('\n')
                .ok_or("missing record terminator")?;
        } else {
            // Old generations use unframed, single-line values. The version
            // header keeps legacy values containing tabs unambiguous.
            (value, remaining) = rest.split_once('\n').unwrap_or((rest, ""));
        }
        if out.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("duplicate key {key}").into());
        }
    }
    Ok(out)
}
// This encoding is also used by nix/owner-key.nix. Escaping the marker itself
// keeps names such as a_b and a-5fb distinct in units and catalog directories.
pub fn owner_key(username: &str) -> String {
    use std::fmt::Write;
    let mut key = String::from("u");
    for byte in username.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            key.push(char::from(byte));
        } else {
            write!(key, "-{byte:02x}").expect("writing to a String cannot fail");
        }
    }
    key
}

fn resolve_owner(values: &mut BTreeMap<String, String>) -> Result<()> {
    if values.get("identityVersion").is_some_and(|v| v == "2") {
        let username = values.get("username").ok_or("missing owner username")?;
        let account = nix::unistd::User::from_name(username)?
            .ok_or_else(|| format!("sandbox owner account {username:?} does not exist"))?;
        if account.uid.is_root() || account.gid.as_raw() == 0 {
            return Err("sandbox owner must have non-root UID and primary GID".into());
        }
        // Never accept numeric host credentials supplied by a profile or its
        // caller. Resolve the configured account through the host account DB.
        values.insert("hostUid".into(), account.uid.to_string());
        values.insert("hostGid".into(), account.gid.to_string());
    } else if values.contains_key("identityVersion") {
        return Err("unsupported profile identity version".into());
    } else if values.contains_key("hostUid") || values.contains_key("hostGid") {
        return Err("legacy profiles cannot override host credentials".into());
    }
    Ok(())
}

impl Profile {
    pub fn running_profile(&self, instance: &str) -> Result<Self> {
        let runtime = self.supervisor_runtime(instance)?;
        if runtime.join("reload-failed").exists() {
            return Err(
                "the last live reload failed; retry it or restart the instance before entering"
                    .into(),
            );
        }
        self.running_profile_for_reload(instance)
    }

    pub fn running_profile_for_reload(&self, instance: &str) -> Result<Self> {
        // This GC root is published by the root supervisor in its root-owned
        // RuntimeDirectory, not in the owner's writable session directory.
        let runtime = self.supervisor_runtime(instance)?;
        let directory = fs::canonicalize(runtime.join("profile-root"))?;
        if !directory.starts_with("/nix/store/") {
            return Err("running profile must be an immutable Nix store path".into());
        }
        let running = Self::load(&directory)?;
        self.check_running_identity(&running)?;
        if running.values.contains_key("identityVersion") {
            // The store profile contains the account name, not the IDs that
            // were used to boot this lifetime. Those are pinned separately by
            // the root supervisor, outside every owner-writable directory.
            let recorded = fs::read_to_string(runtime.join("host-identity"))?;
            self.check_host_identity(&recorded)?;
        }
        Ok(running)
    }

    pub fn check_running_identity(&self, running: &Self) -> Result<()> {
        if running.host_uid()? != self.host_uid()?
            || running.host_gid()? != self.host_gid()?
            || running.values.get("username") != self.values.get("username")
            || running.get("name")? != self.get("name")?
        {
            return Err("running profile belongs to a different sandbox or owner".into());
        }
        Ok(())
    }

    fn check_host_identity(&self, recorded: &str) -> Result<()> {
        if recorded != self.host_identity()? {
            return Err(
                "host account IDs changed; stop and restart this instance before joining".into(),
            );
        }
        Ok(())
    }

    pub fn host_identity(&self) -> Result<String> {
        Ok(format!("{}\t{}\n", self.host_uid()?, self.host_gid()?))
    }

    pub fn immutable_store_path(&self, key: &str) -> Result<PathBuf> {
        let path = PathBuf::from(self.get(key)?);
        if !path.starts_with("/nix/store/")
            || !path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(format!("{key} must be an immutable Nix store path").into());
        }
        let canonical = fs::canonicalize(&path)?;
        if !canonical.starts_with("/nix/store/") {
            return Err(format!("{key} must resolve within the immutable Nix store").into());
        }
        Ok(canonical)
    }

    pub fn owner_key(&self) -> Result<String> {
        Ok(owner_key(self.get("username")?))
    }

    pub fn supervisor_runtime(&self, instance: &str) -> Result<PathBuf> {
        let legacy = Path::new("/run").join(self.machine(instance)?);
        let declared = self.declared_runtime(instance)?;
        if !declared.join("profile-root").exists() && legacy.join("profile-root").exists() {
            return Ok(legacy);
        }
        Ok(declared)
    }

    pub fn declared_runtime(&self, instance: &str) -> Result<PathBuf> {
        self.machine(instance)?;
        if !self.values.contains_key("identityVersion") {
            return Ok(Path::new("/run").join(self.machine(instance)?));
        }
        Ok(PathBuf::from(format!(
            "/run/nook-{}-{}-{instance}",
            self.owner_key()?,
            self.get("name")?
        )))
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
        let mut values = pairs(&dir.join("profile"))?;
        resolve_owner(&mut values)?;
        let mut mounts = Vec::new();
        for line in lines(&dir.join("mounts"))? {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != 3 || !["ro", "rw", "device", "device-pattern"].contains(&fields[0]) {
                return Err("invalid mount record".into());
            }
            let source = fields[1].replace(
                "@host-runtime@",
                &format!(
                    "/run/user/{}",
                    values
                        .get("hostUid")
                        .or_else(|| values.get("uid"))
                        .ok_or("missing host UID")?
                ),
            );
            safe_mount(&source, fields[2])?;
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
                source,
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
        if p.host_uid()? == 0
            || p.host_gid()? == 0
            || p.guest_uid()? == 0
            || p.guest_uid()? >= 100000
            || p.guest_gid()? == 0
            || p.guest_gid()? >= 100000
        {
            return Err(
                "owner IDs must be non-root; guest UID/GID must be between 1 and 99999".into(),
            );
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
    pub fn host_uid(&self) -> Result<u32> {
        Ok(self
            .values
            .get("hostUid")
            .map(String::as_str)
            .unwrap_or(self.get("uid")?)
            .parse()?)
    }
    pub fn host_gid(&self) -> Result<u32> {
        Ok(self
            .values
            .get("hostGid")
            .map(String::as_str)
            .unwrap_or(self.get("gid")?)
            .parse()?)
    }
    pub fn guest_uid(&self) -> Result<u32> {
        Ok(self.get("uid")?.parse()?)
    }
    pub fn guest_gid(&self) -> Result<u32> {
        Ok(self.get("gid")?.parse()?)
    }
    pub fn hostname(&self, instance: &str) -> Result<String> {
        if !valid_instance(instance) {
            return Err("invalid sandbox instance".into());
        }
        // Older manifests boot a system with a static "sandbox" hostname.
        // An empty value in new manifests requests a launch-time default.
        let configured = self
            .values
            .get("hostname")
            .map_or("sandbox", String::as_str);
        if !configured.is_empty() {
            return Ok(configured.into());
        }
        let name = self.get("name")?;
        let instance_name = instance.strip_prefix("n-").unwrap_or(instance);
        // Profile and instance names are each limited to 24 ASCII characters.
        // Trimming handles permitted edge hyphens without exceeding DNS's
        // 63-character label limit or changing stored instance identifiers.
        let combined = format!("{name}-{instance_name}");
        let hostname = combined.trim_matches('-');
        Ok(if hostname.is_empty() {
            "sandbox".into()
        } else {
            hostname.into()
        })
    }

    pub fn machine(&self, instance: &str) -> Result<String> {
        if !valid_instance(instance) {
            return Err(
                "invalid instance: use main, n-NAME, or e- followed by 16 hex digits".into(),
            );
        }
        Ok(format!(
            "sbx-{}-{}-{}",
            self.host_uid()?,
            self.get("name")?,
            instance
        ))
    }
    pub fn unit(&self, instance: &str) -> Result<String> {
        self.machine(instance)?;
        let legacy_runtime = Path::new("/run").join(self.machine(instance)?);
        let owner = if !self.values.contains_key("identityVersion")
            || legacy_runtime.join("profile-root").exists()
        {
            self.host_uid()?.to_string()
        } else {
            self.owner_key()?
        };
        Ok(format!(
            "sandbox-{owner}-{}@{instance}.service",
            self.get("name")?
        ))
    }

    pub fn runtime(&self, instance: &str) -> Result<PathBuf> {
        self.machine(instance)?;
        Ok(PathBuf::from(format!(
            "/run/user/{}/sandbox/{}/{instance}",
            self.host_uid()?,
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

    #[test]
    fn framed_pairs_preserve_control_characters_and_reject_bad_frames() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pairs");
        let value = "é\nother\t4\tfake\r\0";
        fs::write(
            &path,
            format!(
                "nook-pairs-v1\nkey\t{}\t{value}\nother\t4\treal\n",
                value.len()
            ),
        )
        .unwrap();
        let decoded = pairs(&path).unwrap();
        assert_eq!(decoded["key"], value);
        assert_eq!(decoded["other"], "real");
        assert_eq!(decoded.len(), 2);
        for malformed in [
            "nook-pairs-v1\nkey\t99\tshort\n",
            "nook-pairs-v1\nkey\t1\té\n",
            "nook-pairs-v1\nkey\t1\tx",
            "nook-pairs-v1\nkey\t1\tx\nkey\t1\ty\n",
        ] {
            fs::write(&path, malformed).unwrap();
            assert!(pairs(&path).is_err(), "{malformed:?}");
        }
        fs::write(&path, "key\tlegacy\twith tabs\n").unwrap();
        assert_eq!(pairs(&path).unwrap()["key"], "legacy\twith tabs");
    }

    fn profile(state: &Path, persistent: bool) -> Profile {
        Profile {
            dir: PathBuf::new(),
            values: BTreeMap::from([
                ("state".into(), state.to_string_lossy().into_owned()),
                ("home".into(), "/home/test".into()),
                ("name".into(), "dev".into()),
                ("uid".into(), "1000".into()),
                ("gid".into(), "100".into()),
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
    fn hostnames_follow_the_template_and_instance_unless_configured() {
        let mut profile = profile(Path::new("/home/test"), true);
        assert_eq!(profile.hostname("main").unwrap(), "sandbox");
        profile.values.insert("hostname".into(), String::new());
        for (instance, expected) in [
            ("main", "dev-main"),
            ("n-project", "dev-project"),
            ("e-0123456789abcdef", "dev-e-0123456789abcdef"),
        ] {
            assert_eq!(profile.hostname(instance).unwrap(), expected);
        }
        assert!(profile.hostname("../outside").is_err());
        profile
            .values
            .insert("hostname".into(), "workstation".into());
        assert_eq!(profile.hostname("n-project").unwrap(), "workstation");
    }

    #[test]
    fn generated_hostnames_are_valid_dns_labels_for_edge_case_names() {
        let mut profile = profile(Path::new("/home/test"), true);
        profile.values.insert("hostname".into(), String::new());
        for name in ["-dev-", "---", "a23456789012345678901234"] {
            profile.values.insert("name".into(), name.into());
            for instance in ["main", "n-project-", "n---", "n-a23456789012345678901234"] {
                let hostname = profile.hostname(instance).unwrap();
                assert!(!hostname.is_empty() && hostname.len() <= 63);
                assert!(hostname.as_bytes()[0].is_ascii_alphanumeric());
                assert!(hostname.as_bytes()[hostname.len() - 1].is_ascii_alphanumeric());
                assert!(
                    hostname
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                );
            }
        }
    }

    #[test]
    fn owner_keys_are_unambiguous_unit_components() {
        assert_eq!(owner_key("alice"), "ualice");
        assert_eq!(owner_key("a_b.c-d$"), "ua-5fb-2ec-2dd-24");
        assert_ne!(owner_key("a_b"), owner_key("a-5fb"));
        assert_ne!(owner_key("Alice"), owner_key("alice"));
    }

    #[test]
    fn host_and_guest_credentials_are_independent() {
        let mut profile = profile(Path::new("/home/test"), true);
        profile.values.insert("hostUid".into(), "123456".into());
        profile.values.insert("hostGid".into(), "234567".into());
        assert_eq!(profile.host_uid().unwrap(), 123456);
        assert_eq!(profile.host_gid().unwrap(), 234567);
        assert_eq!(profile.guest_uid().unwrap(), 1000);
        assert_eq!(profile.guest_gid().unwrap(), 100);
        assert!(profile.check_host_identity("123456\t234567\n").is_ok());
        assert!(profile.check_host_identity("1000\t100\n").is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "host account lookup uses NSS")]
    fn runtime_owner_lookup_ignores_supplied_numeric_credentials() {
        let user = nix::unistd::User::from_uid(nix::unistd::getuid())
            .unwrap()
            .unwrap();
        assert!(
            !user.uid.is_root(),
            "run credential tests as an ordinary user"
        );
        let mut values = BTreeMap::from([
            ("identityVersion".into(), "2".into()),
            ("username".into(), user.name),
            ("hostUid".into(), "0".into()),
            ("hostGid".into(), "0".into()),
        ]);
        resolve_owner(&mut values).unwrap();
        assert_eq!(values["hostUid"], user.uid.to_string());
        assert_eq!(values["hostGid"], user.gid.to_string());
        values.insert("username".into(), "root".into());
        assert!(resolve_owner(&mut values).is_err());
        values.insert("username".into(), "nook-test-account-does-not-exist".into());
        assert!(resolve_owner(&mut values).is_err());
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
    fn immutable_paths_reject_lexical_store_escapes_before_resolution() {
        let mut profile = profile(Path::new("/home/test"), true);
        for value in [
            "/nix/store/example/../../../tmp/payload",
            "/nix/store/example/./payload",
            "/tmp/payload",
            "relative",
        ] {
            profile.values.insert("system".into(), value.into());
            assert!(profile.immutable_store_path("system").is_err(), "{value}");
        }
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
