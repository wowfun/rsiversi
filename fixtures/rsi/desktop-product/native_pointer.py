"""XTest input on the fixture's private display, through GTK's real event path."""
import ctypes as C
import ctypes.util
import time
from native_window import require_private_xvfb


def drag(x, y, dx, dy, button=1):
    require_private_xvfb()
    assert button in (1, 3) and all(abs(v) < 32768 for v in (x, y, x+dx, y+dy))
    x11 = C.CDLL(ctypes.util.find_library('X11'))
    xtest = C.CDLL(ctypes.util.find_library('Xtst'))
    x11.XOpenDisplay.argtypes = [C.c_char_p]; x11.XOpenDisplay.restype = C.c_void_p
    x11.XFlush.argtypes = [C.c_void_p]
    x11.XCloseDisplay.argtypes = [C.c_void_p]
    xtest.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]
    xtest.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
    display = x11.XOpenDisplay(None)
    if not display: raise RuntimeError('Cannot connect to private fixture display')
    def flush():
        x11.XFlush(display); time.sleep(.025)
    try:
        assert xtest.XTestFakeMotionEvent(display, -1, round(x), round(y), 0); flush()
        assert xtest.XTestFakeButtonEvent(display, button, 1, 0); flush()
        for step in range(1, 9):
            assert xtest.XTestFakeMotionEvent(display, -1, round(x+dx*step/8), round(y+dy*step/8), 0); flush()
    finally:
        xtest.XTestFakeButtonEvent(display, button, 0, 0); flush()
        x11.XCloseDisplay(display)
