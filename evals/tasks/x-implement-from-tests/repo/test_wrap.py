import unittest
from wrap import wrap

class T(unittest.TestCase):
    def test_short(self):
        self.assertEqual(wrap("hi there", 20), ["hi there"])
    def test_breaks_on_spaces(self):
        self.assertEqual(wrap("the quick brown fox", 10), ["the quick", "brown fox"])
    def test_exact_fit(self):
        self.assertEqual(wrap("abcde fghij", 5), ["abcde", "fghij"])
    def test_long_word_is_split(self):
        self.assertEqual(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"])
    def test_long_word_after_text(self):
        self.assertEqual(wrap("ab abcdefgh", 4), ["ab", "abcd", "efgh"])
    def test_collapses_whitespace(self):
        self.assertEqual(wrap("a   b \n c", 10), ["a b c"])
    def test_empty(self):
        self.assertEqual(wrap("", 5), [])
        self.assertEqual(wrap("   ", 5), [])
    def test_bad_width(self):
        with self.assertRaises(ValueError):
            wrap("x", 0)
