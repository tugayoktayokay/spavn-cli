import unittest
from semver import compare

class T(unittest.TestCase):
    def test_core(self):
        self.assertEqual(compare("1.2.3", "1.2.4"), -1)
        self.assertEqual(compare("2.0.0", "1.9.9"), 1)
        self.assertEqual(compare("1.10.0", "1.9.0"), 1)
        self.assertEqual(compare("1.0.0", "1.0.0"), 0)
    def test_build_ignored(self):
        self.assertEqual(compare("1.0.0+a", "1.0.0+b"), 0)
        self.assertEqual(compare("1.0.0-rc.1+x", "1.0.0-rc.1"), 0)
    def test_prerelease_lower(self):
        self.assertEqual(compare("1.0.0-alpha", "1.0.0"), -1)
        self.assertEqual(compare("1.0.0", "1.0.0-alpha"), 1)
    def test_prerelease_order(self):
        order = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
                 "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"]
        for i in range(len(order)):
            for j in range(len(order)):
                want = (i > j) - (i < j)
                self.assertEqual(compare(order[i], order[j]), want, (order[i], order[j]))
    def test_numeric_identifier_not_string(self):
        self.assertEqual(compare("1.0.0-2", "1.0.0-10"), -1)
    def test_invalid(self):
        for bad in ["1.2", "1.2.3.4", "01.2.3", "1.2.3-", "1.0.0-a..b", "a.b.c", "", "1.2.-3"]:
            with self.assertRaises(ValueError, msg=bad):
                compare(bad, "1.0.0")
            with self.assertRaises(ValueError, msg=bad):
                compare("1.0.0", bad)
