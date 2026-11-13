import unittest
from billing import calc_total
from report import summary

class T(unittest.TestCase):
    def test_total(self):
        self.assertEqual(calc_total([1, 2, 3]), 6)
    def test_summary(self):
        self.assertEqual(summary([1, 2]), "Total: 3")
