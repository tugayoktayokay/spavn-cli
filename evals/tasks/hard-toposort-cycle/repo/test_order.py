import unittest
from order import toposort, CycleError

class T(unittest.TestCase):
    def test_simple(self):
        self.assertEqual(toposort({"b": ["a"], "a": []}), ["a", "b"])
    def test_implicit_nodes(self):
        self.assertEqual(toposort({"app": ["lib", "log"]}), ["lib", "log", "app"])
    def test_alphabetical_among_ready(self):
        self.assertEqual(toposort({"d": ["a"], "c": ["a"], "b": ["a"], "a": []}), ["a", "b", "c", "d"])
    def test_diamond(self):
        deps = {"top": ["l", "r"], "l": ["base"], "r": ["base"], "base": []}
        self.assertEqual(toposort(deps), ["base", "l", "r", "top"])
    def test_empty(self):
        self.assertEqual(toposort({}), [])
    def test_cycle_message(self):
        with self.assertRaises(CycleError) as cm:
            toposort({"b": ["c"], "c": ["a"], "a": ["b"]})
        self.assertEqual(str(cm.exception), "cycle: a -> b -> c -> a")
    def test_self_cycle(self):
        with self.assertRaises(CycleError) as cm:
            toposort({"x": ["x"]})
        self.assertEqual(str(cm.exception), "cycle: x -> x")
    def test_cycle_behind_valid_nodes(self):
        with self.assertRaises(CycleError) as cm:
            toposort({"z": ["a"], "a": [], "m": ["n"], "n": ["m"]})
        self.assertEqual(str(cm.exception), "cycle: m -> n -> m")
    def test_input_not_mutated(self):
        deps = {"b": ["a"]}
        toposort(deps)
        self.assertEqual(deps, {"b": ["a"]})
