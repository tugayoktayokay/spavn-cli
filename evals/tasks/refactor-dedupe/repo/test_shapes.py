import unittest
from shapes import area_square, area_rect

class T(unittest.TestCase):
    def test_square(self):
        self.assertEqual(area_square(3), 9)
    def test_rect(self):
        self.assertEqual(area_rect(2, 5), 10)
    def test_negative(self):
        with self.assertRaises(ValueError):
            area_square(-1)
        with self.assertRaises(ValueError):
            area_rect(1, -1)
    def test_single_validation_site(self):
        with open("shapes.py") as f:
            self.assertEqual(f.read().count('raise ValueError'), 1)
