import copy
import hashlib
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

from check_dependency_licenses import DECISIONS, LicenseContractError, check_contract, main


class DependencyLicenseContractTests(unittest.TestCase):
    def setUp(self):
        self.decisions = json.loads(DECISIONS.read_text(encoding="utf-8"))
        replacement = self.decisions["replacements"]["mac_address"]
        self.lock = {
            "package": [{"name": "mac_address", **{key: replacement[key] for key in ("version", "source", "checksum")}}]
        }

    def test_distinct_registry_default_net_is_not_mistaken_for_the_retired_git_package(self):
        self.lock["package"].append({"name": "default-net", "version": "0.14.1", "source": "registry+example"})
        check_contract({}, self.lock, self.decisions)

    def test_blocked_sources_are_rejected_in_target_dependencies_and_patch_tables(self):
        sources = [
            "https://github.com/rustdesk-org/default_net",
            "https://GITHUB.com/RUSTDESK-ORG/DEFAULT_NET.GIT",
            "git+https://github.com/rustdesk-org/default_net.git?rev=123#123",
            "ssh://git@GITHUB.COM/rustdesk-org/impersonate-system.git",
            "https://github.com/rustdesk-org/%64efault_net/",
        ]
        for source in sources:
            with self.subTest(source=source):
                for manifest in [
                    {"target": {"cfg(windows)": {"dependencies": {"renamed": {"git": source}}}}},
                    {"patch": {"crates-io": {"renamed": {"git": source}}}},
                ]:
                    with self.assertRaises(LicenseContractError):
                        check_contract(manifest, self.lock, self.decisions)

    def test_lockfile_reintroduction_is_rejected_even_with_a_renamed_dependency_key(self):
        for package in [
            {"name": "default_net"},
            {"name": "impersonate_system"},
            {"name": "renamed", "source": "git+https://github.com/rustdesk-org/default_net#123"},
        ]:
            with self.subTest(package=package), self.assertRaises(LicenseContractError):
                check_contract({}, {"package": [*self.lock["package"], package]}, self.decisions)

    def test_replacement_version_source_checksum_and_presence_are_bound(self):
        for field in ["version", "source", "checksum"]:
            changed = copy.deepcopy(self.lock)
            changed["package"][0][field] = "changed"
            with self.subTest(field=field), self.assertRaises(LicenseContractError):
                check_contract({}, changed, self.decisions)
        for packages in [[], self.lock["package"] * 2]:
            with self.assertRaises(LicenseContractError):
                check_contract({}, {"package": packages}, self.decisions)

    def test_reviewed_license_digests_are_explicit_sha256_values(self):
        replacement = self.decisions["replacements"]["mac_address"]
        self.assertEqual(replacement["license"], "MIT OR Apache-2.0")
        self.assertEqual(set(replacement["license_sha256"]), {"LICENSE-MIT", "LICENSE-APACHE2.0"})
        for digest in replacement["license_sha256"].values():
            self.assertEqual(len(bytes.fromhex(digest)), hashlib.sha256().digest_size)

    def test_malformed_inventory_is_rejected_explicitly(self):
        for packages in [None, ["invalid"], [{"name": "example", "source": False}]]:
            with self.subTest(packages=packages), self.assertRaises(LicenseContractError):
                check_contract({}, {"package": packages}, self.decisions)

    def test_cli_does_not_echo_parse_excerpts_or_credentials(self):
        with tempfile.TemporaryDirectory(prefix="camellia-license-contract-") as directory:
            manifest = Path(directory) / "Cargo.toml"
            lock = Path(directory) / "Cargo.lock"
            manifest.write_text('credential-canary = "unterminated', encoding="utf-8")
            lock.write_text("version = 4", encoding="utf-8")
            output, errors = io.StringIO(), io.StringIO()
            with (
                patch.object(
                    sys, "argv", ["check-dependency-licenses", "--manifest", str(manifest), "--lockfile", str(lock)]
                ),
                redirect_stdout(output),
                redirect_stderr(errors),
            ):
                status = main()
            self.assertEqual(status, 1)
            self.assertEqual(output.getvalue(), "")
            self.assertEqual(errors.getvalue(), "LEGAL-001 dependency license contract failed\n")
            self.assertNotIn("credential-canary", errors.getvalue())


if __name__ == "__main__":
    unittest.main()
