"""Save and restore only the Linux sandbox policy owned by a CI step."""
import argparse
import json
import os
from pathlib import Path
import subprocess

SETTINGS = {"kernel.unprivileged_userns_clone": "1", "kernel.apparmor_restrict_unprivileged_userns": "0"}


def owner():
    return os.getuid()


def command(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True)


def prepare(path, profile, proc_root=Path("/proc/sys")):
    settings = SETTINGS if profile == "full" else {"kernel.apparmor_restrict_unprivileged_userns": "0"}
    previous = {}
    for key in settings:
        if profile == "full" and not (proc_root / key.replace(".", "/")).exists():
            continue
        value = command("sysctl", "-n", key).stdout.strip()
        if value not in ("0", "1"):
            raise ValueError(f"unexpected boolean sysctl value for {key}")
        previous[key] = value
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise RuntimeError("sandbox policy was not restored by an earlier step; retained ownership record must be restored first") from error
    with os.fdopen(descriptor, "w", encoding="utf-8") as target:
        os.chmod(path, 0o600)
        json.dump({"uid": owner(), "values": previous}, target)
        target.flush()
        os.fsync(target.fileno())
    for key in previous:
        command("sudo", "sysctl", "-w", f"{key}={settings[key]}")


def restore(path):
    if not path.exists():
        return
    with path.open("rb") as source:
        encoded = source.read(4097)
    if len(encoded) > 4096:
        raise ValueError("sandbox policy state exceeds its bound")
    state = json.loads(encoded)
    if (not isinstance(state, dict) or set(state) != {"uid", "values"}
            or type(state["uid"]) is not int or state["uid"] != owner()
            or not isinstance(state["values"], dict)
            or any(key not in SETTINGS or value not in ("0", "1") for key, value in state["values"].items())):
        raise ValueError("invalid sandbox policy restore ownership")
    errors = []
    for key, value in state["values"].items():
        try:
            command("sudo", "sysctl", "-w", f"{key}={value}")
        except (OSError, subprocess.CalledProcessError) as error:
            errors.append(error)
    if errors:
        raise RuntimeError("sandbox restoration failed; retaining ownership record") from errors[0]
    path.unlink()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "restore"))
    parser.add_argument("state", type=Path)
    parser.add_argument("profile", nargs="?", choices=("full", "apparmor"))
    args = parser.parse_args()
    if (args.action == "prepare") != (args.profile is not None):
        parser.error("prepare requires a profile; restore does not accept one")
    if args.action == "prepare":
        prepare(args.state, args.profile)
    else:
        restore(args.state)
