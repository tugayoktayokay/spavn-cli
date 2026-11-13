import unittest, settings

class T(unittest.TestCase):
    def setUp(self):
        settings.reset()
    def test_default_retries(self):
        self.assertEqual(settings.get("retries"), 3)
    def test_tags_are_isolated(self):
        settings.get("tags").append("x")
        settings.reset()
        self.assertEqual(settings.get("tags"), [])
