"""Passes only when the tests accept stack.py and reject every mutant."""
import glob, os, shutil, subprocess, sys, tempfile

def tests_pass(stack_source):
    with tempfile.TemporaryDirectory() as d:
        shutil.copy("test_stack.py", d)
        shutil.copy(stack_source, os.path.join(d, "stack.py"))
        r = subprocess.run([sys.executable, "-m", "unittest", "-q"], cwd=d,
                           capture_output=True, text=True, timeout=60)
        ran = "Ran 0 tests" not in r.stderr
        return r.returncode == 0 and ran

if not tests_pass("stack.py"):
    sys.exit("tests fail (or none exist) on the real implementation")
survivors = [m for m in sorted(glob.glob("mutants/*.py")) if tests_pass(m)]
if survivors:
    sys.exit("mutants not caught: " + ", ".join(survivors))
