import unittest
from slug import slugify

class T(unittest.TestCase):
    def test_spaces(self):
        self.assertEqual(slugify("Hello World"), "hello-world")
    def test_punctuation(self):
        self.assertEqual(slugify("  Hello, World!  "), "hello-world")
    def test_collapse(self):
        self.assertEqual(slugify("a --- b"), "a-b")
    def test_digits(self):
        self.assertEqual(slugify("Top 10 Tips"), "top-10-tips")
    def test_empty(self):
        self.assertEqual(slugify("!!!"), "")
