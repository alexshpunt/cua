"""Assemble private-distribution MCP runtimes and validate a complete source set.

This does not publish packages or install a driver. The Windows package also
installs on Linux x64 so WSL can select its Windows host without a second install.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil

TARGETS = {
    "win32-x64": {
        "os": ["win32", "linux"],
        "cpu": "x64",
        "binaries": ["cua-driver.exe", "cua-cursor-theme.exe", "cua-driver-uia.exe"],
    },
    "linux-x64": {"os": ["linux"], "cpu": "x64", "binaries": ["cua-driver", "cua-cursor-theme"]},
    "darwin-x64": {"os": ["darwin"], "cpu": "x64", "binaries": ["cua-driver", "cua-cursor-theme"]},
    "darwin-arm64": {
        "os": ["darwin"],
        "cpu": "arm64",
        "binaries": ["cua-driver", "cua-cursor-theme"],
    },
}
NOTICES = ["LICENSE.md", "THIRD_PARTY_NOTICES.md"]


def sha256(path):
    """Hash a native payload without loading the binary into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def assemble(release, licenses, output, source, target, driver_version, probe):
    """Copy one complete runtime with exact source, startup evidence and hashes."""
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("source must be an exact commit SHA")
    spec = TARGETS[target]
    output.mkdir(parents=True, exist_ok=False)
    runtime = output / "runtime"
    runtime.mkdir()
    for name in spec["binaries"]:
        shutil.copy2(release / name, runtime / name)
        if not name.endswith(".exe"):
            (runtime / name).chmod(0o755)
    for name in NOTICES:
        shutil.copy2(licenses / name, runtime / name)
    version = f"{driver_version}-dev.{source[:12]}"
    files = {name: sha256(runtime / name) for name in spec["binaries"] + NOTICES}
    write_json(
        output / "runtime.json",
        {
            "format": 1,
            "source": source,
            "target": target,
            "version": version,
            "driver_version": driver_version,
            "command": f"runtime/{spec['binaries'][0]}",
            "files": files,
            "signing": "development artifact; not notarized or UIAccess-qualified",
            "gui_qualification": "not established by this build",
        },
    )
    write_json(output / "startup.json", probe)
    write_json(
        output / "package.json",
        {
            "name": f"@alexshp/cua-runtime-{target}",
            "version": version,
            "description": f"Pinned CUA MCP runtime for {target}",
            "license": "MIT",
            "os": spec["os"],
            "cpu": [spec["cpu"]],
            "files": ["runtime/", "runtime.json", "startup.json"],
            "repository": {"type": "git", "url": "https://github.com/alexshpunt/cua.git"},
        },
    )
    return output


def verify_set(packages, source):
    """Reject missing, duplicate, mixed-source or changed platform packages."""
    entries = []
    seen = set()
    for package in packages:
        manifest = json.loads((package / "runtime.json").read_text(encoding="utf-8"))
        target = manifest["target"]
        if manifest["source"] != source:
            raise ValueError("runtime source mismatch")
        if target not in TARGETS or target in seen:
            raise ValueError("expected a complete four-target set without duplicates")
        seen.add(target)
        names = TARGETS[target]["binaries"] + NOTICES
        if set(manifest["files"]) != set(names):
            raise ValueError("required runtime companion or notice missing")
        for name in names:
            if sha256(package / "runtime" / name) != manifest["files"][name]:
                raise ValueError(f"runtime hash mismatch: {target}/{name}")
        metadata = json.loads((package / "package.json").read_text(encoding="utf-8"))
        if (
            metadata["name"] != f"@alexshp/cua-runtime-{target}"
            or metadata["version"] != manifest["version"]
        ):
            raise ValueError("npm package identity mismatch")
        probe = json.loads((package / "startup.json").read_text(encoding="utf-8"))
        if probe.get("source") != source or probe.get("target") != target:
            raise ValueError("startup evidence source or target mismatch")
        if (
            probe.get("input_calls") != 0
            or probe.get("capture_calls") != 0
            or not probe.get("tools")
        ):
            raise ValueError("missing read-only startup evidence")
        entries.append(manifest)
    if seen != set(TARGETS):
        raise ValueError("expected a complete four-target set")
    if len({entry["version"] for entry in entries}) != 1:
        raise ValueError("runtime versions differ")
    return {
        "source": source,
        "packages": entries,
        "gate": "build and API only; no GUI certification",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    one = commands.add_parser("assemble")
    for name in ("release", "licenses", "output", "probe"):
        one.add_argument(f"--{name}", type=Path, required=True)
    one.add_argument("--source", required=True)
    one.add_argument("--target", choices=TARGETS, required=True)
    one.add_argument("--driver-version", required=True)
    complete = commands.add_parser("verify-set")
    complete.add_argument("--root", type=Path, required=True)
    complete.add_argument("--source", required=True)
    complete.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "assemble":
        assemble(
            args.release,
            args.licenses,
            args.output,
            args.source,
            args.target,
            args.driver_version,
            json.loads(args.probe.read_text(encoding="utf-8")),
        )
    else:
        write_json(args.output, verify_set(sorted(args.root.glob("*/package")), args.source))


if __name__ == "__main__":
    main()
