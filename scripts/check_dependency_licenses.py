#!/usr/bin/env python3
"""Targeted LEGAL-001 regression gate; shared by Protocol and its consumers.

The pinned registry checksum binds the reviewed license files to one artifact.
This deliberately does not claim a full transitive or distribution-license audit.
"""

import argparse
import json
import sys
import tomllib
from pathlib import Path
from urllib.parse import unquote, urlsplit

DECISIONS = Path(__file__).resolve().parents[1] / "dependency-license-decisions.json"


class LicenseContractError(ValueError):
    pass


def repository_identity(source):
    if not isinstance(source, str):
        raise LicenseContractError("Git dependency source must be a string")
    parsed = urlsplit(source.removeprefix("git+"))
    path = unquote(parsed.path).rstrip("/").lower().removesuffix(".git")
    return f"{(parsed.hostname or '').lower()}{path}"


def check_contract(manifest, lock, decisions):
    if decisions.get("schema_version") != 1:
        raise LicenseContractError("Unsupported dependency-license decision schema")
    blocked_names = set(decisions["blocked_packages"])
    blocked_repositories = set(decisions["blocked_git_repositories"])

    def check_source(source):
        if repository_identity(source) in blocked_repositories:
            raise LicenseContractError("An unlicensed Git repository remains in the product dependency graph")

    def inspect_manifest(value):
        if isinstance(value, dict):
            if "git" in value:
                check_source(value["git"])
            for child in value.values():
                inspect_manifest(child)
        elif isinstance(value, list):
            for child in value:
                inspect_manifest(child)

    inspect_manifest(manifest)
    packages = lock.get("package")
    if not isinstance(packages, list):
        raise LicenseContractError("Cargo lockfile must contain a package inventory")
    for package in packages:
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            raise LicenseContractError("Cargo package inventory contains an invalid entry")
        if package.get("name") in blocked_names:
            raise LicenseContractError("An unlicensed package remains in the product lockfile")
        source = package.get("source", "")
        if not isinstance(source, str):
            raise LicenseContractError("Cargo package source must be a string")
        if source.startswith("git+"):
            check_source(source)
    for name, decision in decisions["replacements"].items():
        matches = [package for package in packages if package.get("name") == name]
        if len(matches) != 1 or any(
            matches[0].get(field) != decision[field] for field in ("version", "source", "checksum")
        ):
            raise LicenseContractError("Replacement artifact differs from the reviewed license decision")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path("Cargo.toml"))
    parser.add_argument("--lockfile", type=Path, default=Path("Cargo.lock"))
    args = parser.parse_args()
    try:
        check_contract(
            tomllib.loads(args.manifest.read_text(encoding="utf-8")),
            tomllib.loads(args.lockfile.read_text(encoding="utf-8")),
            json.loads(DECISIONS.read_text(encoding="utf-8")),
        )
    except (OSError, ValueError, KeyError, TypeError):
        # Never echo dependency URLs, parse excerpts or arbitrary file contents.
        print("LEGAL-001 dependency license contract failed", file=sys.stderr)
        return 1
    print("LEGAL-001: retired sources absent; replacement matches the reviewed artifact")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
