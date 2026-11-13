import unittest
from bank import Bank, InsufficientFunds

def make():
    b = Bank()
    b.open("a", 100)
    b.open("b", 10)
    return b

class T(unittest.TestCase):
    def test_ok(self):
        b = make()
        b.transfer("a", "b", 30)
        self.assertEqual(b.balances, {"a": 70, "b": 40})
        self.assertEqual(b.ledger, [("a", "b", 30)])
    def test_insufficient_changes_nothing(self):
        b = make()
        with self.assertRaises(InsufficientFunds):
            b.transfer("a", "b", 500)
        self.assertEqual(b.balances, {"a": 100, "b": 10})
        self.assertEqual(b.ledger, [])
    def test_unknown_destination_changes_nothing(self):
        b = make()
        with self.assertRaises(KeyError):
            b.transfer("a", "ghost", 5)
        self.assertEqual(b.balances, {"a": 100, "b": 10})
        self.assertEqual(b.ledger, [])
    def test_unknown_source_changes_nothing(self):
        b = make()
        with self.assertRaises(KeyError):
            b.transfer("ghost", "b", 5)
        self.assertEqual(b.balances, {"a": 100, "b": 10})
    def test_transfer_to_self(self):
        b = make()
        b.transfer("a", "a", 20)
        self.assertEqual(b.balances["a"], 100)
    def test_non_positive(self):
        b = make()
        for amt in (0, -5):
            with self.assertRaises(ValueError):
                b.transfer("a", "b", amt)
        self.assertEqual(b.balances, {"a": 100, "b": 10})
