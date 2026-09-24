"""Native GTK save-dialog input on the fixture-owned X display."""
import ctypes as C
import ctypes.util
import hashlib
import json
import time
from native_window import require_private_xvfb


class WindowAttributes(C.Structure):
    _fields_ = [('x',C.c_int),('y',C.c_int),('width',C.c_int),('height',C.c_int),('border_width',C.c_int),('depth',C.c_int),('visual',C.c_void_p),('root',C.c_ulong),('window_class',C.c_int),('bit_gravity',C.c_int),('win_gravity',C.c_int),('backing_store',C.c_int),('backing_planes',C.c_ulong),('backing_pixel',C.c_ulong),('save_under',C.c_int),('colormap',C.c_ulong),('map_installed',C.c_int),('map_state',C.c_int),('all_event_masks',C.c_long),('your_event_mask',C.c_long),('do_not_propagate_mask',C.c_long),('override_redirect',C.c_int),('screen',C.c_void_p)]


class NativeKeys:
    def __init__(self):
        require_private_xvfb()
        self.focus_window = None
        self.x = C.CDLL(ctypes.util.find_library('X11'))
        self.test = C.CDLL(ctypes.util.find_library('Xtst'))
        window, display = C.c_ulong, C.c_void_p
        signatures = {
            'XOpenDisplay': ([C.c_char_p], display),
            'XDefaultRootWindow': ([display], window),
            'XQueryTree': ([display, window, C.POINTER(window), C.POINTER(window), C.POINTER(C.POINTER(window)), C.POINTER(C.c_uint)], C.c_int),
            'XFetchName': ([display, window, C.POINTER(C.c_void_p)], C.c_int),
            'XGetWindowAttributes': ([display, window, C.POINTER(WindowAttributes)], C.c_int),
            'XSetInputFocus': ([display, window, C.c_int, C.c_ulong], C.c_int),
            'XGetInputFocus': ([display, C.POINTER(window), C.POINTER(C.c_int)], C.c_int),
            'XSync': ([display, C.c_int], C.c_int),
            'XRaiseWindow': ([display, window], C.c_int),
            'XKeysymToKeycode': ([display, C.c_ulong], C.c_uint),
            'XStringToKeysym': ([C.c_char_p], C.c_ulong),
            'XkbKeycodeToKeysym': ([display, C.c_uint, C.c_int, C.c_int], C.c_ulong),
            'XFlush': ([display], C.c_int),
            'XCloseDisplay': ([display], C.c_int),
            'XFree': ([C.c_void_p], C.c_int),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.x, name); fn.argtypes = args; fn.restype = result
        self.test.XTestFakeKeyEvent.argtypes = [display, C.c_uint, C.c_int, C.c_ulong]
        self.test.XTestFakeKeyEvent.restype = C.c_int
        self.display = self.x.XOpenDisplay(None)
        if not self.display: raise RuntimeError('The export fixture requires its private Xvfb display')

    def chooser(self):
        root, parent, children, count = C.c_ulong(), C.c_ulong(), C.POINTER(C.c_ulong)(), C.c_uint()
        if not self.x.XQueryTree(self.display, self.x.XDefaultRootWindow(self.display), C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
            raise RuntimeError('Cannot inspect fixture display')
        try:
            for index in range(min(count.value, 256)):
                name = C.c_void_p()
                if self.x.XFetchName(self.display, children[index], C.byref(name)) and name:
                    try:
                        attributes = WindowAttributes()
                        if C.string_at(name) == b'Export session' and self.x.XGetWindowAttributes(self.display, children[index], C.byref(attributes)) and attributes.map_state == 2:
                            self.x.XRaiseWindow(self.display, children[index]); self.x.XSetInputFocus(self.display, children[index], 1, 0); self.x.XFlush(self.display)
                            self.focus_window = int(children[index])
                            self.check_focus()
                            return int(children[index])
                    finally: self.x.XFree(name)
        finally:
            if children: self.x.XFree(children)
        return None

    def check_focus(self):
        window, revert = C.c_ulong(), C.c_int()
        self.x.XSync(self.display, 0)
        self.x.XGetInputFocus(self.display, C.byref(window), C.byref(revert))
        if self.focus_window is None or window.value != self.focus_window:
            raise RuntimeError('Native export chooser does not own input focus')

    def key(self, name, down):
        symbol = self.x.XStringToKeysym(name.encode())
        code = self.x.XKeysymToKeycode(self.display, symbol)
        if not code or not self.test.XTestFakeKeyEvent(self.display, code, int(down), 0):
            raise RuntimeError('Native fixture key was not delivered')

    def chord(self, *names):
        self.check_focus()
        for name in names: self.key(name, True)
        for name in reversed(names): self.key(name, False)
        self.x.XFlush(self.display)

    def type(self, text):
        self.check_focus()
        for char in text:
            symbol = ord(char)
            code = self.x.XKeysymToKeycode(self.display, symbol)
            assert code and symbol < 128, 'fixture save path must be ASCII'
            shift = self.x.XkbKeycodeToKeysym(self.display, code, 0, 0) != symbol
            if shift: self.key('Shift_L', True)
            self.test.XTestFakeKeyEvent(self.display, code, 1, 0); self.test.XTestFakeKeyEvent(self.display, code, 0, 0)
            if shift: self.key('Shift_L', False)
        self.x.XFlush(self.display)

    def close(self): self.x.XCloseDisplay(self.display)


def verify(script, button, fill, until, screenshot, report, requests):
    count = len(requests)
    keys = NativeKeys()
    destination = report / 'native-export.json'
    try:
        fill('textarea[aria-label="Main message"]', '/export "suggested.json" -f json -i h,m,lpr,last-provider-response')
        button('Send ↗')
        until(keys.chooser)
        keys.chord('Control_L', 'l'); keys.chord('Control_L','a'); keys.type(str(destination)); keys.chord('Return')
        until(destination.is_file)
        until(lambda: script('return [...document.querySelectorAll("button")].some(e=>e.textContent==="Export")'))
        data = destination.read_bytes(); artifact = json.loads(data)
        assert 'Desktop conversation verified' in data.decode()
        assert artifact['last_provider_request']['availability'] == 'available'
        assert artifact['last_provider_request']['effect_id'] == artifact['last_provider_response']['effect_id']
        assert not artifact['last_provider_response']['raw']
        button('Export'); until(keys.chooser); keys.chord('Escape')
        until(lambda: script('return [...document.querySelectorAll("button")].some(e=>e.textContent==="Export")'))
        assert destination.read_bytes() == data
        assert len(requests) == count
        # Cancellation is a visible local outcome; it never becomes model input.
        fill('textarea[aria-label="Main message"]', '')
        screenshot('export.png')
        (report / 'export.json').write_text(json.dumps({'status':'passed','bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'nativeSaveDialog':True,'cancelPreservedFile':True,'modelRequests':count},indent=2))
    finally: keys.close()
