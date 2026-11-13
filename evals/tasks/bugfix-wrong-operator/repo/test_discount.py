import unittest
from discount import final_price

class T(unittest.TestCase):
    def test_ten_percent(self):
        self.assertEqual(final_price(200.0, 10), 180.0)
    def test_zero(self):
        self.assertEqual(final_price(99.99, 0), 99.99)
    def test_full(self):
        self.assertEqual(final_price(50.0, 100), 0.0)
