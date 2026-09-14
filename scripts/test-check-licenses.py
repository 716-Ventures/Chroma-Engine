"""Offline regression tests for the license inventory gate."""

import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("license_gate", Path(__file__).with_name("check-licenses.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class LicenseGateTests(unittest.TestCase):
    def setUp(self):
        self.review = {"name": "example", "version": "1.0.0", "license": "MIT",
                       "reason": "No upstream license file", "upstream": "https://example.com/source"}
        self.inventory = {"licenses": [{"id": "MIT", "source_path": None,
                            "used_by": [{"crate": {"name": "example", "version": "1.0.0"}}]}]}

    def test_reviewed_fallback(self):
        gate.check_fallbacks(self.inventory, [self.review])

    def test_new_fallback_rejected(self):
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [])

    def test_changed_version_rejected(self):
        self.review["version"] = "2.0.0"
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [self.review])

    def test_changed_license_rejected(self):
        self.review["license"] = "BSD-2-Clause"
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [self.review])

    def test_recovered_text_requires_removing_exception(self):
        self.inventory["licenses"][0]["source_path"] = "LICENSE"
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [self.review])

    def test_duplicate_review_rejected(self):
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [self.review, self.review])

    def test_unexplained_review_rejected(self):
        self.review["reason"] = ""
        with self.assertRaises(ValueError):
            gate.check_fallbacks(self.inventory, [self.review])

    def test_recovered_text_needs_no_exception(self):
        self.inventory["licenses"][0]["source_path"] = "LICENSE"
        gate.check_fallbacks(self.inventory, [])


if __name__ == "__main__":
    unittest.main()
