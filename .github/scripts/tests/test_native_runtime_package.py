"""Runtime packages must form one exact, complete four-target set."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "native_runtime_package.py"


class NativeRuntimePackageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("native_runtime_package", SCRIPT)
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = "a" * 40

    def assemble(self, target):
        release = self.root / target / "release"
        release.mkdir(parents=True)
        for name in self.module.TARGETS[target]["binaries"]:
            (release / name).write_bytes((target + name).encode())
        licenses = self.root / "licenses"
        licenses.mkdir(exist_ok=True)
        for name in ("LICENSE.md", "THIRD_PARTY_NOTICES.md"):
            (licenses / name).write_text("notice", encoding="utf-8")
        probe = {
            "source": self.source,
            "target": target,
            "input_calls": 0,
            "capture_calls": 0,
            "tools": ["list_windows", "get_window_state"],
        }
        return self.module.assemble(
            release, licenses, self.root / target / "package", self.source, target, "0.34.0", probe
        )

    def test_complete_set_preserves_source_hashes_companions_and_notices(self):
        packages = [self.assemble(target) for target in self.module.TARGETS]
        result = self.module.verify_set(packages, self.source)
        self.assertEqual(len(result["packages"]), 4)
        windows = next(p for p in packages if "win32" in str(p))
        manifest = json.loads((windows / "runtime.json").read_text())
        self.assertIn("cua-driver-uia.exe", manifest["files"])
        self.assertIn("THIRD_PARTY_NOTICES.md", manifest["files"])
        self.assertEqual(
            json.loads((windows / "package.json").read_text())["os"], ["win32", "linux"]
        )

    def test_missing_target_wrong_source_and_changed_bytes_reject_promotion(self):
        packages = [self.assemble(target) for target in self.module.TARGETS]
        with self.assertRaisesRegex(ValueError, "complete four-target"):
            self.module.verify_set(packages[:-1], self.source)
        manifest_path = packages[0] / "runtime.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["source"] = "b" * 40
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "source"):
            self.module.verify_set(packages, self.source)
        manifest["source"] = self.source
        manifest_path.write_text(json.dumps(manifest))
        (packages[0] / "runtime" / "cua-driver.exe").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "hash"):
            self.module.verify_set(packages, self.source)

    def test_missing_required_sidecar_refuses_assembly(self):
        package = self.assemble("win32-x64")
        release = package.parent / "release"
        (release / "cua-driver-uia.exe").unlink()
        with self.assertRaises(FileNotFoundError):
            self.module.assemble(
                release,
                self.root / "licenses",
                package.parent / "other",
                self.source,
                "win32-x64",
                "0.34.0",
                {},
            )


if __name__ == "__main__":
    unittest.main()
