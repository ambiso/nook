# Migrating existing sandbox state

Nook's default state directory is `${config.xdg.stateHome}/nook/<profile>`.
Older installations stored it under `~/.sandbox/<profile>`. The change does
not delete the old directories, but Nook will treat the new directory as a
fresh profile until the old data is copied or the legacy path is configured.

## Keep using the legacy directory

Add a default to your Home Manager Nook configuration:

```nix
programs.nook.defaults =
  { lib, name, ... }:
  {
    stateDirectory = lib.mkDefault ".sandbox/${name}";
  };
```

Or set `stateDirectory = ".sandbox/<profile>";` on individual profiles. Apply
the Home Manager configuration. Relative paths remain relative to your home.

## Move to the XDG state directory

For a clean migration, stop each sandbox before copying. This ensures saved
homes and instance metadata do not change during the final copy.

1. Stop every running instance with `nk stop --all`, or an individual one with
   `nk stop dev` or `nk stop dev --name project`.
2. Copy the old state tree into the new root. The example below assumes
   `config.xdg.stateHome` is `~/.local/state`; use its configured absolute path
   if you changed it:

   ```sh
   mkdir -p "$HOME/.local/state/nook"
   rsync -a -- "$HOME/.sandbox/" "$HOME/.local/state/nook/"
   ```

3. Remove any legacy `stateDirectory` override and apply the Home Manager
   configuration. Nook will use the XDG-based defaults.
4. Start the profiles and check them with `nk list`. Keep `~/.sandbox` until
   you have confirmed the homes and named instances are present.

If the configuration has already switched and a profile looks empty, do not
delete either directory. Restore its legacy `stateDirectory` override and apply
that configuration to reconnect to the existing data. You can migrate later
when you are ready to stop the sandboxes.

## Main instance directory layout

Nook now stores the main instance beside named instances at
`<state>/instances/main/home`. Older releases used `<state>/home`. Each `nk`
invocation checks every configured profile and atomically moves a stopped,
persistent main home into place. Running profiles are skipped until a later
invocation after they stop. Named instances already use `instances/n-NAME/home`
and need no layout change.

Nook leaves the source untouched and reports an error if both old and new homes
exist. Profiles that are no longer configured are not in Nook's catalog. First
check that no sandbox unit for the profile is running, then move its old home
manually:

```sh
set -e
state="$HOME/.local/state/nook/dev" # replace dev and the state root if needed
test -d "$state/home"
test ! -e "$state/instances/main/home"
test ! -L "$state/instances/main/home"
mkdir -p "$state/instances/main"
mv -T -- "$state/home" "$state/instances/main/home"
```

The move preserves the directory and its contents when both paths are on the
same filesystem. If both homes already exist, inspect them before moving
anything; Nook cannot safely choose which data to keep.
