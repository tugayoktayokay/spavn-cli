import unittest
from cart import total

ITEMS = [{"price": 20.0, "qty": 2}, {"price": 10.0, "qty": 1}]

class T(unittest.TestCase):
    def test_no_coupon(self):
        self.assertEqual(total(ITEMS), 50.0)
    def test_pct(self):
        self.assertEqual(total(ITEMS, "TEN"), 45.0)
    def test_flat(self):
        self.assertEqual(total(ITEMS, "FIVE"), 45.0)
    def test_flat_floor(self):
        self.assertEqual(total([{"price": 3.0, "qty": 1}], "FIVE"), 0)
