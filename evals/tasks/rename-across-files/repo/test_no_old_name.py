import glob, unittest

class T(unittest.TestCase):
    def test_old_name_gone(self):
        for path in glob.glob("*.py"):
            if path == "test_no_old_name.py":
                continue
            with open(path) as f:
                self.assertNotIn("calc_total", f.read(), path)
    def test_new_name_works(self):
        from billing import compute_total
        self.assertEqual(compute_total([4, 5]), 9)
