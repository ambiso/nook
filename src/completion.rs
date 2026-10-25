//! Clap generates shell integration and flag candidates; profile data is read at Tab time.
use crate::config::catalog;
use clap::CommandFactory;
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub fn complete() {
    let bin = if std::env::var("NOOK_COMPLETE_BIN").as_deref() == Ok("sandbox") {
        "sandbox"
    } else {
        "nk"
    };
    clap_complete::CompleteEnv::with_factory(|| {
        // CompleteEnv separates its protocol from the user's command line with `--`.
        let words: Vec<_> = std::env::args_os()
            .skip_while(|s| s != "--")
            .skip(1)
            .collect();
        let index = std::env::var("_CLAP_COMPLETE_INDEX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| words.len().saturating_sub(1));
        command(&words[..index.min(words.len())], crate::default_profiles()).name(bin)
    })
    .bin(bin)
    .completer(bin)
    .complete();
}

fn command(preceding: &[OsString], default_catalog: PathBuf) -> clap::Command {
    // Use the actual parser for options, aliases and positional values, allowing
    // missing arguments because the user is still typing the command.
    let matches = crate::Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(preceding)
        .ok();
    let directory = matches
        .as_ref()
        .and_then(|m| m.get_one::<PathBuf>("profiles"))
        .cloned()
        .unwrap_or(default_catalog);
    let profile = matches
        .as_ref()
        .and_then(|m| m.subcommand())
        .and_then(|(_, m)| m.try_get_one::<String>("profile").ok().flatten())
        .cloned();
    let mut command = crate::Cli::command();
    // Hide shorthand candidates while still parsing aliases already typed.
    command = command
        .mut_subcommands(|sub| sub.visible_alias(None).visible_aliases(Vec::<&str>::new()))
        .mut_subcommand("launch", |sub| sub.aliases(["l", "start", "s"]));
    for action in ["launch", "gui", "shell", "stop", "delete", "path"] {
        command = command.mut_subcommand(action, |sub| {
            // Preserve argument order: mut_arg removes and re-appends positionals.
            sub.mut_args(|arg| {
                let kind = match arg.get_id().as_str() {
                    "profile" => "profile",
                    "instance" => "instance",
                    "name" => "name",
                    _ => return arg,
                };
                let directory = directory.clone();
                let profile = profile.clone();
                arg.add(ArgValueCompleter::new(move |current: &OsStr| {
                    candidates(current, &directory, profile.as_deref(), kind)
                }))
            })
        });
    }
    command
}

fn candidates(
    current: &OsStr,
    directory: &Path,
    selected: Option<&str>,
    kind: &str,
) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let Ok(profiles) = catalog(directory) else {
        return Vec::new();
    };
    let mut values = BTreeSet::new();
    if kind == "profile" {
        values.extend(
            profiles
                .iter()
                .filter_map(|p| p.values.get("name").cloned()),
        );
    } else if let Some(selected) = selected {
        let profiles: Vec<_> = profiles
            .into_iter()
            .filter(|p| p.values.get("name").is_some_and(|n| n == selected))
            .collect();
        if let Some(profile) = profiles.first() {
            // Saved homes remain completable even when the system bus is unavailable.
            values.insert("main".to_owned());
            values.extend(profile.saved_instances().unwrap_or_default());
            if let Ok(choices) = crate::choices(&profiles) {
                values.extend(choices.into_iter().map(|c| c.instance));
            }
        }
        if kind == "name" {
            values = values
                .into_iter()
                .filter_map(|s| s.strip_prefix("n-").map(String::from))
                .collect();
        }
    }
    values
        .into_iter()
        .filter(|s| s.starts_with(current))
        .map(CompletionCandidate::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn add_profile(catalog: &Path, name: &str, state: &Path) {
        let dir = catalog.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("profile"), format!("name\t{name}\nusername\tnook-test\nuid\t1234\ngid\t1234\nstate\t{}\npersistent\t1\n", state.display())).unwrap();
        for file in [
            "mounts",
            "environment",
            "session-rules",
            "system-rules",
            "pasta",
        ] {
            fs::write(dir.join(file), "").unwrap();
        }
    }

    fn suggestions(words: &[&str], directory: &Path) -> Vec<OsString> {
        let words: Vec<_> = words.iter().map(OsString::from).collect();
        let index = words.len() - 1;
        let mut cmd = command(&words[..index], directory.into());
        clap_complete::engine::complete(&mut cmd, words, index, None)
            .unwrap()
            .into_iter()
            .map(|c| c.get_value().to_owned())
            .collect()
    }

    #[test]
    fn profiles_are_dynamic_and_flags_come_from_clap() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("profiles");
        add_profile(&directory, "dev", temp.path());
        let results = suggestions(&["nk", "launch", "d"], &directory);
        assert!(
            results.contains(&"dev".into()),
            "{results:?}; catalog={:?}",
            catalog(&directory)
        );
        add_profile(&directory, "dota-ai", temp.path());
        assert!(suggestions(&["nk", "l", "d"], &directory).contains(&"dota-ai".into()));
        assert!(
            suggestions(&["nk", "launch", "dev", "--e"], &directory)
                .contains(&"--ephemeral".into())
        );
        assert!(suggestions(&["nk", "delete", "dev", "--y"], &directory).contains(&"--yes".into()));
        assert!(
            !suggestions(&["nk", "stop", "dev", "--e"], &directory).contains(&"--ephemeral".into())
        );
    }

    #[test]
    fn command_completions_prefer_full_names_over_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("profiles");
        add_profile(&directory, "dev", temp.path());
        assert_eq!(
            suggestions(&["nk", ""], &directory)
                .into_iter()
                .filter(|command| ["l", "start", "s"].contains(&command.to_str().unwrap_or("")))
                .count(),
            0
        );
        assert!(suggestions(&["nk", ""], &directory).contains(&"launch".into()));
        assert!(suggestions(&["nk", "la"], &directory).contains(&"launch".into()));
    }

    #[test]
    fn custom_catalog_and_selected_profile_control_instance_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("catalog with spaces");
        let state = temp.path().join("dev-state");
        fs::create_dir_all(state.join("instances/n-project/home")).unwrap();
        add_profile(&directory, "dev", &state);
        add_profile(&directory, "other", &temp.path().join("other-state"));
        let dir = directory.to_str().unwrap();
        assert!(
            suggestions(
                &["nk", "--profiles", dir, "launch", "dev", "--instance", "n-"],
                Path::new("/missing")
            )
            .contains(&"n-project".into())
        );
        assert!(
            suggestions(
                &["nk", "--profiles", dir, "launch", "dev", "--name", "p"],
                Path::new("/missing")
            )
            .contains(&"project".into())
        );
        assert!(
            !suggestions(
                &[
                    "nk",
                    "--profiles",
                    dir,
                    "launch",
                    "other",
                    "--instance",
                    "n-"
                ],
                Path::new("/missing")
            )
            .contains(&"n-project".into())
        );
    }
}
