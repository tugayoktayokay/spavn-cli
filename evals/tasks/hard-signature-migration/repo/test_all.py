import unittest
import notify, billing, alerts, signup
from jobs import cleanup

class T(unittest.TestCase):
    def setUp(self):
        notify.SENT.clear()
    def test_send_signature(self):
        self.assertEqual(notify.send("hi", to="bob"), 1)
        self.assertEqual(notify.SENT, [("bob", "hi")])
        with self.assertRaises(TypeError):
            notify.send("bob", "hi")
    def test_billing(self):
        billing.charge("amy", 5)
        self.assertEqual(notify.SENT, [("amy", "charged 5")])
    def test_alerts(self):
        self.assertEqual(alerts.alert_all(["a", "b"], "x"), [1, 2])
        self.assertEqual(notify.SENT, [("a", "x"), ("b", "x")])
    def test_signup(self):
        signup.welcome("cy")
        self.assertEqual(notify.SENT, [("cy", "welcome"), ("admin", "new user cy")])
    def test_cleanup(self):
        cleanup.run("ops")
        self.assertEqual(notify.SENT, [("ops", "cleanup done")])
