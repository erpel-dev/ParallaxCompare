from filter_ui import *
from contextlib import contextmanager
import fcntl
@contextmanager
def manual():
 (ROOT/'pause').touch()
 with (ROOT/'gui.lock').open('w')as f:
  fcntl.flock(f,fcntl.LOCK_EX)
  try:yield
  finally:(ROOT/'pause').unlink(missing_ok=True)
