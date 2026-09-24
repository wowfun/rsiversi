"""Native WM_DELETE_WINDOW input on the fixture's private Xvfb display."""
import ctypes as C
import ctypes.util
import os
from pathlib import Path
import re
import socket
import struct


def require_private_xvfb():
    """Reject native input unless the X server belongs to this fixture's ancestry."""
    match = re.fullmatch(r':(\d+)(?:\.\d+)?', os.environ.get('DISPLAY', ''))
    if not match:
        raise RuntimeError('Native fixture input requires a private xvfb-run display')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        # Xlib can use Linux's abstract socket even when the filesystem listener
        # is unavailable (for example in a container's shared /tmp).
        address = f'/tmp/.X11-unix/X{match[1]}'
        try:
            connection.connect('\0' + address)
        except ConnectionRefusedError:
            connection.connect(address)
        pid, uid, _gid = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize('3i')))
    def parent(process):
        fields = dict(line.split(':', 1) for line in Path(f'/proc/{process}/status').read_text().splitlines())
        return int(fields['PPid'])
    ancestors = set()
    current = os.getpid()
    while current > 1 and current not in ancestors:
        ancestors.add(current)
        current = parent(current)
    if uid != os.getuid() or Path(f'/proc/{pid}/exe').resolve().name != 'Xvfb' or parent(pid) not in ancestors:
        raise RuntimeError('Refusing native input on an X display not owned by this fixture')
    return pid


def request_close():
    require_private_xvfb()
    x11 = C.CDLL(ctypes.util.find_library('X11'))
    window_id = C.c_ulong
    display_type = C.c_void_p
    class Data(C.Union):
        _fields_ = [('bytes', C.c_char * 20), ('shorts', C.c_short * 10), ('longs', C.c_long * 5)]
    class ClientMessage(C.Structure):
        _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int), ('display', display_type), ('window', window_id), ('message_type', C.c_ulong), ('format', C.c_int), ('data', Data)]
    class Event(C.Union):
        _fields_ = [('client', ClientMessage), ('padding', C.c_long * 24)]
    signatures = {
        'XOpenDisplay': ([C.c_char_p], display_type),
        'XDefaultRootWindow': ([display_type], window_id),
        'XQueryTree': ([display_type, window_id, C.POINTER(window_id), C.POINTER(window_id), C.POINTER(C.POINTER(window_id)), C.POINTER(C.c_uint)], C.c_int),
        'XFetchName': ([display_type, window_id, C.POINTER(C.c_void_p)], C.c_int),
        'XInternAtom': ([display_type, C.c_char_p, C.c_int], C.c_ulong),
        'XSendEvent': ([display_type, window_id, C.c_int, C.c_long, C.POINTER(Event)], C.c_int),
        'XFlush': ([display_type], C.c_int),
        'XCloseDisplay': ([display_type], C.c_int),
        'XFree': ([C.c_void_p], C.c_int),
    }
    for name, (arguments, result) in signatures.items():
        function = getattr(x11, name); function.argtypes = arguments; function.restype = result
    display = x11.XOpenDisplay(None)
    if not display:
        raise RuntimeError('Native close requires the fixture Xvfb display')
    try:
        root, parent, children, count = window_id(), window_id(), C.POINTER(window_id)(), C.c_uint()
        if not x11.XQueryTree(display, x11.XDefaultRootWindow(display), C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
            raise RuntimeError('Cannot inspect fixture display')
        matches = []
        try:
            if count.value > 256:
                raise RuntimeError('Unexpected fixture display window count')
            for index in range(count.value):
                name = C.c_void_p()
                if x11.XFetchName(display, children[index], C.byref(name)) and name:
                    try:
                        if C.string_at(name) == b'RSI': matches.append(children[index])
                    finally: x11.XFree(name)
        finally:
            if children: x11.XFree(children)
        if len(matches) != 1:
            raise RuntimeError(f'Expected one RSI fixture window, found {len(matches)}')
        event = Event()
        event.client.type = 33  # X11 ClientMessage, per Xlib.h.
        event.client.display = display
        event.client.window = matches[0]
        event.client.message_type = x11.XInternAtom(display, b'WM_PROTOCOLS', 0)
        event.client.format = 32
        event.client.data.longs[0] = x11.XInternAtom(display, b'WM_DELETE_WINDOW', 0)
        if not x11.XSendEvent(display, matches[0], 0, 0, C.byref(event)):
            raise RuntimeError('Native window close was not delivered')
        x11.XFlush(display)
    finally:
        x11.XCloseDisplay(display)
