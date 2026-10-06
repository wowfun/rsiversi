"""Own only the Linux user manager state created by one CI job."""
import json
import os
from pathlib import Path
import pwd
import subprocess
import sys


def command(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True)


def prepare(path, linger_root=Path("/var/lib/systemd/linger")):
    uid = os.getuid()
    user = pwd.getpwuid(uid).pw_name
    active = subprocess.run(["systemctl", "is-active", "--quiet", f"user@{uid}.service"], check=False)
    if active.returncode not in (0, 3, 4):
        raise RuntimeError("cannot observe the existing user manager")
    state = {"uid": uid, "user": user, "linger": (linger_root / user).exists(), "active": active.returncode == 0}
    with path.open("x", encoding="utf-8") as target:
        os.chmod(path, 0o600)
        json.dump(state, target)
        target.flush()
        os.fsync(target.fileno())
    if not state["linger"]:
        command("sudo", "loginctl", "enable-linger", user)
    if not state["active"]:
        command("sudo", "systemctl", "start", f"user@{uid}.service")


def restore(path):
    if not path.exists():
        return
    with path.open(encoding="utf-8") as source:
        text = source.read(4097)
    if len(text) > 4096:
        raise RuntimeError("user manager state exceeds its bound")
    state = json.loads(text)
    uid = os.getuid()
    if (not isinstance(state, dict) or set(state) != {"uid", "user", "linger", "active"}
            or type(state["uid"]) is not int or state["uid"] != uid
            or state["user"] != pwd.getpwuid(uid).pw_name
            or type(state["linger"]) is not bool or type(state["active"]) is not bool):
        raise RuntimeError("invalid user manager restore ownership")
    errors = []
    actions = []
    if not state["active"]:
        actions.append(("sudo", "systemctl", "stop", f"user@{uid}.service"))
    if not state["linger"]:
        actions.append(("sudo", "loginctl", "disable-linger", state["user"]))
    for action in actions:
        try:
            command(*action)
        except subprocess.CalledProcessError as error:
            errors.append(error)
    if errors:
        raise RuntimeError("user manager restoration failed; retaining ownership record") from errors[0]
    path.unlink()


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("prepare", "restore"):
        raise SystemExit("usage: user_manager.py prepare|restore STATE")
    {"prepare": prepare, "restore": restore}[sys.argv[1]](Path(sys.argv[2]))
