import random, unittest
from pairs import count_pairs

class T(unittest.TestCase):
    def test_small(self):
        self.assertEqual(count_pairs([1, 2, 3, 4], 5), 2)
        self.assertEqual(count_pairs([], 1), 0)
        self.assertEqual(count_pairs([5], 10), 0)
    def test_duplicates(self):
        self.assertEqual(count_pairs([2, 2, 2, 2], 4), 6)
        self.assertEqual(count_pairs([1, 1, 3, 3], 4), 4)
    def test_negative_and_zero(self):
        self.assertEqual(count_pairs([-1, 1, 0, 0, 2, -2], 0), 3)
    def test_large(self):
        rnd = random.Random(7)
        nums = [rnd.randint(-10**6, 10**6) for _ in range(200_000)]
        got = count_pairs(nums, 1234)
        self.assertGreaterEqual(got, 0)
        self.assertEqual(got, count_pairs(nums[:1] + nums[1:], 1234))
