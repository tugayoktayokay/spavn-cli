import unittest
from report import render

class T(unittest.TestCase):
    def test_width(self):
        out = render([{"name": "Zoë", "paid": 1}, {"name": "Bob", "paid": 2}]).split("\n")
        self.assertEqual(len(out[0]), len(out[1]))
        self.assertTrue(out[0].startswith("Zoë"))
