import unittest
from handlers import handle_refund, handle_action_03, handle_post_07

ORDER = {"id": 1, "total": 50}

class T(unittest.TestCase):
    def test_partial(self):
        self.assertEqual(handle_refund(ORDER, 20)["refunded"], 20)
    def test_full(self):
        self.assertEqual(handle_refund(ORDER, 50)["refunded"], 50)
    def test_too_much(self):
        with self.assertRaises(ValueError):
            handle_refund(ORDER, 51)
    def test_zero(self):
        with self.assertRaises(ValueError):
            handle_refund(ORDER, 0)
    def test_neighbours_untouched(self):
        self.assertEqual(handle_action_03(ORDER)["step"], 3)
        self.assertEqual(handle_post_07(ORDER)["handler"], "post_07")
