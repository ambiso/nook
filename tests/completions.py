"""Exercise installed generated completions in all supported shells."""
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

package = Path(sys.argv[1])


def add_profile(directory, name, state):
    profile = directory / name
    profile.mkdir(parents=True)
    (profile / "profile").write_text(
        f"name\t{name}\nusername\tnook-test\nuid\t1234\ngid\t1234\n"
        f"state\t{state}\npersistent\t1\n"
    )
    for name in ["mounts", "environment", "session-rules", "system-rules", "pasta"]:
        (profile / name).touch()


with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    directory = root / "catalog with spaces"
    state = root / "state"
    (state / "instances/n-project/home").mkdir(parents=True)
    add_profile(directory, "dev", state)
    (root / "bin").mkdir()
    (root / "bin/sandbox").symlink_to(package / "bin/nk")
    env = dict(os.environ, NOOK_PROFILES=str(directory), NOOK_SMOKE_STATE="inactive")
    env["PATH"] = f"{root / 'bin'}:{package / 'bin'}:{env['PATH']}"

    def candidates(shell, words):
        name = words[0]
        quoted = shlex.join(words)
        if shell == "bash":
            script = f"""
source {package}/share/bash-completion/completions/{name}
COMP_WORDS=({quoted})
COMP_CWORD={len(words) - 1}
COMP_TYPE=9
_clap_complete_{name} {name} {shlex.quote(words[-1])} ''
printf '%s\\n' "${{COMPREPLY[@]}}"
"""
        elif shell == "zsh":
            script = f"""
compdef() {{ :; }}
source {package}/share/zsh/site-functions/_{name}
_describe() {{ local array=$argv[-1]; print -rl -- "${{(@P)array}}"; }}
words=({quoted})
CURRENT={len(words)}
_clap_dynamic_completer_{name}
"""
        else:
            script = (
                f"source {package}/share/fish/vendor_completions.d/{name}.fish; "
                f"complete --do-complete {shlex.quote(quoted)}"
            )
        result = subprocess.run([shell, "-c", script], env=env, check=True,
                                capture_output=True, text=True, timeout=15)
        return {line.split("\t", 1)[0].split(":", 1)[0] for line in result.stdout.splitlines()}

    for shell in ["bash", "zsh", "fish"]:
        for name in ["nk", "sandbox"]:
            assert "dev" in candidates(shell, [name, "launch", "d"]), (shell, name)
            assert "--ephemeral" in candidates(shell, [name, "launch", "dev", "--e"])
            assert "--all" in candidates(shell, [name, "stop", "dev", "--a"])
            assert "--yes" in candidates(shell, [name, "delete", "dev", "--y"])
            assert "--serve" not in candidates(shell, [name, "--"])
            assert "n-project" in candidates(shell, [name, "launch", "dev", "--instance", "n-"])
            assert "project" in candidates(shell, [name, "launch", "dev", "--name", "p"])
        print(f"{shell}: dynamic profiles, flags, aliases and saved instances passed")

    add_profile(directory, "dota-ai", state)
    for shell in ["bash", "zsh", "fish"]:
        assert "dota-ai" in candidates(shell, ["nk", "launch", "d"])

    # Completion input must never divert a pkexec invocation into another mode.
    result = subprocess.run([str(package / "bin/nk"), "list"],
                            env=dict(env, COMPLETE="fish", PKEXEC_UID="1234"),
                            capture_output=True, text=True, timeout=15)
    assert result.returncode != 0
    assert "polkit execution is restricted" in result.stderr
