import unittest, settings

class T(unittest.TestCase):
    def setUp(self):
        settings.reset()
    def test_override(self):
        settings.override(retries=9)
        self.assertEqual(settings.get("retries"), 9)
    def test_tags_default(self):
        self.assertEqual(settings.get("tags"), [])
