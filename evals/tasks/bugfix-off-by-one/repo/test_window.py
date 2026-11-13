import unittest
from window import moving_sum

class T(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(moving_sum([1, 2, 3, 4], 2), [3, 5, 7])
    def test_full(self):
        self.assertEqual(moving_sum([1, 2, 3], 3), [6])
    def test_too_short(self):
        self.assertEqual(moving_sum([1], 2), [])
