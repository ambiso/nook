//! Directory descriptors and openat2 keep saved-instance traversal away from
//! symlinks and mounts. Ordinary deletion runs as the owner; the authenticated
//! root helper uses the same traversal for subordinate-UID container data.

use crate::config::{Profile, Result};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{
        AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, fstat, mkdirat, open, openat2,
        renameat, statat, unlinkat,
    },
};

/// Open an existing path relative to an anchor without permitting `..`, an
/// absolute symlink, or a magic link to escape that anchor. Ordinary symlinks
/// and mount points which still resolve beneath the anchor remain usable.
pub fn open_beneath(root_path: &Path, relative: &Path) -> Result<File> {
    let root = File::from(open(
        root_path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    Ok(File::from(openat2(
        &root,
        relative,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
    )?))
}

/// Create and reopen a directory without following links or crossing mounts
/// below the configured anchor. Call this with the sandbox owner's credentials
/// when the resulting volume is owner-managed.
pub fn ensure_directory_beneath(root_path: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("managed directory must use a non-empty relative path".into());
    }
    fs::create_dir_all(root_path)?;
    // stateDirectory itself may intentionally be a host symlink. Resolve that
    // configured anchor once, then forbid links and mount transitions below it.
    let canonical = fs::canonicalize(root_path)?;
    let mut directory = File::open(&canonical)?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            unreachable!("validated above")
        };
        match mkdirat(&directory, name, Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(error) => return Err(error.into()),
        }
        directory = File::from(openat2(
            &directory,
            Path::new(name),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
        )?);
    }
    Ok(canonical.join(relative))
}
use std::{
    ffi::CStr,
    fs::{self, File},
    os::unix::{fs::PermissionsExt, io::AsRawFd},
    path::{Path, PathBuf},
};

/// Move the pre-instance-layout main home into the uniform instance tree.
///
/// This compatibility migration is deliberately a rename rather than a copy:
/// it is atomic, preserves all metadata, and cannot leave two diverging homes.
/// The caller must hold main's lifecycle lock so no second supervisor can race
/// the move. Both source and destination parents are opened beneath the state
/// directory before `renameat`, so path replacement cannot redirect the move.
pub fn migrate_legacy_main_home(profile: &Profile) -> Result<()> {
    if nix::unistd::geteuid().as_raw() != profile.host_uid()? {
        return Err("instance storage must be managed as its host owner".into());
    }

    let state_path = Path::new(profile.get("state")?);
    fs::create_dir_all(state_path)?;
    let state_path = fs::canonicalize(state_path)?;
    let state = File::open(&state_path)?;
    let legacy = match open_directory(&state, Path::new("home")) {
        Ok(home) => home,
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
        {
            return Ok(());
        }
        Err(error) => return Err(format!("cannot inspect legacy main home: {error}").into()),
    };

    ensure_directory_beneath(&state_path, Path::new("instances/main"))?;
    let destination = open_directory(&state, Path::new("instances/main"))?;
    match statat(&destination, "home", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {
            return Err(format!(
                "both legacy main home {} and new main home {} contain entries; refusing to merge or overwrite them",
                state_path.join("home").display(),
                state_path.join("instances/main/home").display()
            )
            .into());
        }
        Err(rustix::io::Errno::NOENT) => (),
        Err(error) => return Err(error.into()),
    }

    verify_entry(&state, "home", &legacy)?;
    renameat(&state, "home", &destination, "home")?;
    Ok(())
}

pub struct SavedInstance {
    pub path: PathBuf,
    parent: OwnedFd,
    root: OwnedFd,
    name: String,
}

impl SavedInstance {
    pub fn open(profile: &Profile, instance: &str) -> Result<Self> {
        if !instance.starts_with("n-") || !crate::config::valid_instance(instance) {
            return Err("only named persistent instances can be deleted; ephemeral homes disappear on stop, and main belongs to its profile".into());
        }
        if nix::unistd::geteuid().as_raw() != profile.host_uid()? {
            return Err("instance storage must be managed as its host owner".into());
        }
        // The configured state directory may itself be a deliberate host
        // symlink. Resolve that anchor once; never follow links underneath it.
        let state_path = fs::canonicalize(profile.get("state")?)?;
        let state = File::open(&state_path)?;
        let parent = open_directory(&state, Path::new("instances"))?;
        let root = open_directory(&parent, Path::new(instance))?;
        Ok(Self {
            path: state_path.join("instances").join(instance),
            parent,
            root,
            name: instance.into(),
        })
    }

    pub fn remove(&self) -> Result<()> {
        self.remove_with_privileges(false)
    }

    pub fn remove_privileged(&self, owner: u32) -> Result<()> {
        if !nix::unistd::geteuid().is_root() {
            return Err("privileged instance deletion requires root".into());
        }
        if fstat(&self.root)?.st_uid != owner {
            return Err("instance root does not belong to the authenticated owner".into());
        }
        self.remove_with_privileges(true)
    }

    fn remove_with_privileges(&self, privileged: bool) -> Result<()> {
        verify_entry(&self.parent, self.name.as_str(), &self.root)?;
        // Discover existing mount boundaries before deleting any files.
        // The removal pass repeats the checks, since the tree can change.
        visit(&self.root, false, 0, privileged)?;
        visit(&self.root, true, 0, privileged)?;
        verify_entry(&self.parent, self.name.as_str(), &self.root)?;
        unlinkat(&self.parent, self.name.as_str(), AtFlags::REMOVEDIR)?;
        Ok(())
    }
}

fn open_directory(parent: impl AsFd, path: &Path) -> Result<OwnedFd> {
    Ok(openat2(
        parent,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )?)
}

fn verify_entry(parent: impl AsFd, name: impl rustix::path::Arg, opened: impl AsFd) -> Result<()> {
    let current = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let expected = fstat(opened)?;
    if current.st_dev != expected.st_dev || current.st_ino != expected.st_ino {
        return Err("instance contents changed during deletion; refusing to continue".into());
    }
    Ok(())
}

fn visit(directory: impl AsFd, remove: bool, depth: usize, privileged: bool) -> Result<()> {
    if depth > 256 {
        return Err("instance directory nesting exceeds the deletion limit".into());
    }
    for entry in Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        visit_entry(&directory, name, remove, depth, privileged)?;
    }
    Ok(())
}

fn visit_entry(
    directory: impl AsFd,
    name: &CStr,
    remove: bool,
    depth: usize,
    privileged: bool,
) -> Result<()> {
    // O_PATH|O_NOFOLLOW opens a symlink itself. NO_XDEV rejects bind mounts too,
    // including mounts whose st_dev matches the enclosing filesystem.
    let entry = openat2(
        &directory,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )
    .map_err(|error| {
        format!("cannot safely traverse {name:?} (mount points are refused): {error}")
    })?;
    let metadata = fstat(&entry)?;
    let is_directory = FileType::from_raw_mode(metadata.st_mode) == FileType::Directory;
    if is_directory {
        if !privileged {
            make_directory_removable(&entry, metadata.st_uid, metadata.st_mode)?;
        }
        let child = open_directory(&entry, Path::new("."))?;
        visit(&child, remove, depth + 1, privileged)?;
    }
    if remove {
        verify_entry(&directory, name, &entry)?;
        unlinkat(
            &directory,
            name,
            if is_directory {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            },
        )?;
    }
    Ok(())
}

fn make_directory_removable(directory: &OwnedFd, owner: u32, mode: u32) -> Result<()> {
    let required = Mode::RWXU.bits();
    if mode & required == required {
        return Ok(());
    }
    if owner != nix::unistd::geteuid().as_raw() {
        return Err("cannot delete a directory not accessible to the instance owner".into());
    }

    // Rootless container storage deliberately creates some overlay work
    // directories with mode 000, and read-only image layers commonly contain
    // mode 0555 directories. Pinning the inode before changing its mode keeps
    // a concurrent path replacement from redirecting chmod outside the saved
    // instance. Linux does not expose fchmodat2(AT_EMPTY_PATH) through rustix,
    // so use the kernel's procfs reference to that already-open O_PATH fd.
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    fs::set_permissions(
        pinned,
        fs::Permissions::from_mode((mode & 0o7777) | required),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, os::unix::fs::symlink};

    fn profile(state: &Path) -> Profile {
        Profile {
            dir: PathBuf::new(),
            values: BTreeMap::from([
                ("name".into(), "test".into()),
                ("uid".into(), nix::unistd::geteuid().to_string()),
                ("state".into(), state.to_string_lossy().into_owned()),
            ]),
            mounts: vec![],
            host_environment: vec![],
            session_rules: vec![],
            system_rules: vec![],
            pasta: vec![],
        }
    }

    #[test]
    fn deletion_is_limited_to_named_instances() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        for instance in ["main", "e-0123456789abcdef", "../outside", "n-../outside"] {
            assert!(SavedInstance::open(&profile, instance).is_err());
        }
    }

    #[test]
    fn deleting_one_home_preserves_neighbors_and_symlink_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        let target = temporary.path().join("instances/n-test");
        fs::create_dir_all(target.join("home/cache")).unwrap();
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "outside data").unwrap();
        fs::create_dir_all(temporary.path().join("instances/n-other/home")).unwrap();
        fs::write(target.join("home/cache/output"), "disposable").unwrap();
        symlink(&outside, target.join("home/shared")).unwrap();
        SavedInstance::open(&profile, "n-test")
            .unwrap()
            .remove()
            .unwrap();
        assert!(!target.exists());
        assert_eq!(
            fs::read_to_string(outside.join("keep")).unwrap(),
            "outside data"
        );
        assert!(temporary.path().join("instances/n-other/home").exists());
    }

    #[test]
    fn deletion_opens_owner_locked_and_read_only_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        let target = temporary.path().join("instances/n-test");
        let locked = target.join("home/overlay/work/work");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("private"), "container data").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let read_only = target.join("home/image-layer");
        fs::create_dir(&read_only).unwrap();
        fs::write(read_only.join("file"), "image data").unwrap();
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();

        SavedInstance::open(&profile, "n-test")
            .unwrap()
            .remove()
            .unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn symlink_instance_roots_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        fs::create_dir(temporary.path().join("instances")).unwrap();
        symlink(temporary.path(), temporary.path().join("instances/n-test")).unwrap();
        assert!(SavedInstance::open(&profile, "n-test").is_err());
    }

    #[test]
    fn mounts_are_rejected_before_any_removal() {
        let root = File::open("/").unwrap();
        assert!(visit_entry(&root, c"proc", false, 0, false).is_err());
    }

    #[test]
    fn legacy_main_home_migration_is_atomic_and_refuses_conflicts() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        fs::create_dir(temporary.path().join("home")).unwrap();
        fs::write(temporary.path().join("home/keep"), "main data").unwrap();

        migrate_legacy_main_home(&profile).unwrap();
        assert!(!temporary.path().join("home").exists());
        assert_eq!(
            fs::read_to_string(temporary.path().join("instances/main/home/keep")).unwrap(),
            "main data"
        );
        migrate_legacy_main_home(&profile).unwrap();

        fs::create_dir(temporary.path().join("home")).unwrap();
        fs::write(temporary.path().join("home/legacy"), "legacy").unwrap();
        assert!(migrate_legacy_main_home(&profile).is_err());
        assert_eq!(
            fs::read_to_string(temporary.path().join("home/legacy")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("instances/main/home/keep")).unwrap(),
            "main data"
        );
    }

    #[test]
    fn managed_directories_stay_beneath_their_anchor() {
        let temporary = tempfile::tempdir().unwrap();
        let anchor = temporary.path().join("state");
        assert_eq!(
            ensure_directory_beneath(&anchor, Path::new("instances/n-test/volumes/cache")).unwrap(),
            fs::canonicalize(&anchor)
                .unwrap()
                .join("instances/n-test/volumes/cache")
        );
        assert!(anchor.join("instances/n-test/volumes/cache").is_dir());

        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, anchor.join("escape")).unwrap();
        assert!(ensure_directory_beneath(&anchor, Path::new("escape/new")).is_err());
        assert!(!outside.join("new").exists());
        assert!(ensure_directory_beneath(&anchor, Path::new("../outside/new")).is_err());
    }

    #[test]
    fn anchored_paths_allow_internal_symlinks_but_reject_escapes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("home");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(root.join("state/projects")).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink("state/projects", root.join("Projects")).unwrap();
        symlink(&outside, root.join("escape")).unwrap();

        assert!(open_beneath(&root, Path::new("Projects")).is_ok());
        assert!(open_beneath(&root, Path::new("escape")).is_err());
        assert!(open_beneath(&root, Path::new("../outside")).is_err());
    }

    #[test]
    fn replacing_a_confirmed_directory_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        let target = temporary.path().join("instances/n-test");
        fs::create_dir_all(target.join("home")).unwrap();
        let saved = SavedInstance::open(&profile, "n-test").unwrap();
        let moved = temporary.path().join("moved");
        fs::rename(&target, &moved).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), "replacement").unwrap();
        assert!(saved.remove().is_err());
        assert!(moved.join("home").is_dir());
        assert_eq!(
            fs::read_to_string(target.join("keep")).unwrap(),
            "replacement"
        );
    }
}
