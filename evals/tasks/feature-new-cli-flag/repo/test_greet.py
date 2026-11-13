import io, unittest
from contextlib import redirect_stdout
from greet import main

def run(argv):
    buf = io.StringIO()
    with redirect_stdout(buf):
        main(argv)
    return buf.getvalue().strip()

class T(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(run(["Ada"]), "Hello, Ada!")
    def test_upper(self):
        self.assertEqual(run(["Ada", "--upper"]), "HELLO, ADA!")
