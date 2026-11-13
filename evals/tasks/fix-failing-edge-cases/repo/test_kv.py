import unittest
from kv import parse_kv

class T(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(parse_kv("a=1;b=2"), {"a": "1", "b": "2"})
    def test_trailing_separator(self):
        self.assertEqual(parse_kv("a=1;"), {"a": "1"})
    def test_value_with_equals(self):
        self.assertEqual(parse_kv("u=x=y"), {"u": "x=y"})
    def test_whitespace(self):
        self.assertEqual(parse_kv(" a = 1 ; b = 2 "), {"a": "1", "b": "2"})
    def test_empty(self):
        self.assertEqual(parse_kv(""), {})
