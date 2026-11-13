import unittest
from cart import Cart

class T(unittest.TestCase):
    def test_add(self):
        c = Cart()
        c.add("a")
        self.assertEqual(c.count(), 1)
    def test_fresh_cart_is_empty(self):
        self.assertEqual(Cart().count(), 0)
    def test_given_items_are_copied(self):
        src = ["x"]
        c = Cart(src)
        c.add("y")
        self.assertEqual(src, ["x"])
