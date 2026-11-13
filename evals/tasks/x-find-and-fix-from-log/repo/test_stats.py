import unittest
from stats import percentile, summarize

class T(unittest.TestCase):
    def test_median_odd(self):
        self.assertEqual(percentile([3, 1, 2], 50), 2)
    def test_median_even_lower(self):
        self.assertEqual(percentile([1, 2, 3, 4], 50), 2)
    def test_max(self):
        self.assertEqual(percentile([5, 9, 7], 100), 9)
    def test_min(self):
        self.assertEqual(percentile([5, 9, 7], 0), 5)
    def test_single(self):
        self.assertEqual(percentile([4], 90), 4)
    def test_summarize(self):
        self.assertEqual(summarize([{"ms": 100}, {"ms": 200}])["p50"], 100)
