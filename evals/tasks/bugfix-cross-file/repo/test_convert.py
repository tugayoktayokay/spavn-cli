import unittest
from convert import convert

class T(unittest.TestCase):
    def test_km_to_m(self):
        self.assertAlmostEqual(convert(2, "km", "m"), 2000)
    def test_m_to_cm(self):
        self.assertAlmostEqual(convert(1, "m", "cm"), 100)
    def test_mm_to_cm(self):
        self.assertAlmostEqual(convert(50, "mm", "cm"), 5)
