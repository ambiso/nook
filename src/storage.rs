//! Removal of saved instances runs as the owner, never through the root helper.
//! Directory descriptors and openat2 keep traversal away from symlinks and mounts.

use crate::config::{Profile, Result};
use nix::fcntl::{Flock, FlockArg};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, fstat, openat2, statat, unlinkat},
};
use std::{
    ffi::CStr,
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub fn lock_instance(profile: &Profile, instance: &str) -> Result<Flock<File>> {
    if nix::unistd::geteuid().as_raw() != profile.host_uid()? {
        return Err("instance storage must be managed as its host owner".into());
    }
    profile.machine(instance)?;
    // Keep this inode outside the deleted tree and do not unlink it afterward:
    // replacing a lock file would let a concurrent launcher lock a different inode.
    let directory = Path::new(profile.get("state")?).join(".locks");
    fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(directory.join(instance))?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
        format!("instance is busy starting, running, or being deleted: {error}").into()
    })
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
        verify_entry(&self.parent, self.name.as_str(), &self.root)?;
        // Discover existing mount boundaries before deleting any files.
        // The removal pass repeats the checks, since the tree can change.
        visit(&self.root, false, 0)?;
        visit(&self.root, true, 0)?;
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

fn visit(directory: impl AsFd, remove: bool, depth: usize) -> Result<()> {
    if depth > 256 {
        return Err("instance directory nesting exceeds the deletion limit".into());
    }
    for entry in Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        visit_entry(&directory, name, remove, depth)?;
    }
    Ok(())
}

fn visit_entry(directory: impl AsFd, name: &CStr, remove: bool, depth: usize) -> Result<()> {
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
    let is_directory = FileType::from_raw_mode(fstat(&entry)?.st_mode) == FileType::Directory;
    if is_directory {
        let child = open_directory(&entry, Path::new("."))?;
        visit(&child, remove, depth + 1)?;
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
            environment: BTreeMap::new(),
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
        assert!(visit_entry(&root, c"proc", false, 0).is_err());
    }

    #[test]
    fn lifecycle_lock_excludes_startup_and_deletion() {
        let temporary = tempfile::tempdir().unwrap();
        let profile = profile(temporary.path());
        let lock = lock_instance(&profile, "n-test").unwrap();
        assert!(lock_instance(&profile, "n-test").is_err());
        drop(lock);
        assert!(lock_instance(&profile, "n-test").is_ok());
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
