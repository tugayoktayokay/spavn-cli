import unittest
from lru import LRUCache

class T(unittest.TestCase):
    def test_miss(self):
        self.assertIsNone(LRUCache(2).get("a"))
    def test_evicts_oldest(self):
        c = LRUCache(2)
        c.put("a", 1); c.put("b", 2); c.put("c", 3)
        self.assertIsNone(c.get("a"))
        self.assertEqual(c.get("c"), 3)
    def test_get_refreshes(self):
        c = LRUCache(2)
        c.put("a", 1); c.put("b", 2)
        c.get("a")
        c.put("c", 3)
        self.assertEqual(c.get("a"), 1)
        self.assertIsNone(c.get("b"))
    def test_update_existing(self):
        c = LRUCache(1)
        c.put("a", 1); c.put("a", 2)
        self.assertEqual(c.get("a"), 2)
