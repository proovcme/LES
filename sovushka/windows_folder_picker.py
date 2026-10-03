"""Windows Common Item Dialog, without PowerShell or additional dependencies.

The COM apartment and all interfaces belong to the calling worker thread.
IFileDialog ABI: https://learn.microsoft.com/windows/win32/api/shobjidl_core/nn-shobjidl_core-ifiledialog
"""
from __future__ import annotations

import ctypes as ct
from contextlib import contextmanager
from threading import Lock
from uuid import UUID

_picker_lock = Lock()

class GUID(ct.Structure):
    _fields_ = [("data", ct.c_ubyte * 16)]

    @classmethod
    def parse(cls, value):
        return cls.from_buffer_copy(UUID(value).bytes_le)


def _check(result):
    if result < 0:
        raise OSError(f"Windows folder dialog failed (0x{result & 0xffffffff:08X})")


def _method(pointer, index, *types):
    table = ct.cast(pointer, ct.POINTER(ct.POINTER(ct.c_void_p))).contents
    return ct.WINFUNCTYPE(ct.c_long, ct.c_void_p, *types)(table[index])


def _release(pointer):
    if pointer:
        _method(pointer, 2)(pointer)


@contextmanager
def _folder_dialog(initial: str, title: str):
    """Create/configure the real dialog; always balance COM and native references."""
    ole = ct.OleDLL("ole32")
    ole.CoInitializeEx.argtypes = [ct.c_void_p, ct.c_ulong]
    ole.CoInitializeEx.restype = ct.c_long
    ole.CoCreateInstance.argtypes = [ct.POINTER(GUID), ct.c_void_p, ct.c_ulong,
                                    ct.POINTER(GUID), ct.POINTER(ct.c_void_p)]
    ole.CoCreateInstance.restype = ct.c_long
    ole.CoTaskMemFree.argtypes = [ct.c_void_p]
    ole.CoTaskMemFree.restype = None
    ole.CoUninitialize.argtypes = []
    ole.CoUninitialize.restype = None
    _check(ole.CoInitializeEx(None, 2))  # COINIT_APARTMENTTHREADED
    dialog, folder = ct.c_void_p(), ct.c_void_p()
    try:
        clsid = GUID.parse("DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7")
        iid = GUID.parse("D57C7288-D4AD-4768-BE02-9D969532D960")
        _check(ole.CoCreateInstance(ct.byref(clsid), None, 1, ct.byref(iid), ct.byref(dialog)))
        options = ct.c_ulong()
        _check(_method(dialog, 10, ct.POINTER(ct.c_ulong))(dialog, ct.byref(options)))
        # PICKFOLDERS | FORCEFILESYSTEM | PATHMUSTEXIST | NOCHANGEDIR
        _check(_method(dialog, 9, ct.c_ulong)(dialog, options.value | 0x20 | 0x40 | 0x800 | 0x8))
        _check(_method(dialog, 17, ct.c_wchar_p)(dialog, title))
        if initial:
            shell = ct.WinDLL("shell32")
            shell.SHCreateItemFromParsingName.argtypes = [ct.c_wchar_p, ct.c_void_p,
                                                         ct.POINTER(GUID), ct.POINTER(ct.c_void_p)]
            shell.SHCreateItemFromParsingName.restype = ct.c_long
            item_iid = GUID.parse("43826D1E-E718-42EE-BC55-A1E261C37BFE")
            result = shell.SHCreateItemFromParsingName(initial, None, ct.byref(item_iid), ct.byref(folder))
            if result >= 0:
                _check(_method(dialog, 12, ct.c_void_p)(dialog, folder))
        yield dialog, ole
    finally:
        _release(folder)
        _release(dialog)
        ole.CoUninitialize()


def _pick_folder(*, initial: str, title: str) -> dict[str, str]:
    with _folder_dialog(initial, title) as (dialog, ole):
        # The UI server has no native window. Do not borrow the foreground HWND:
        # it may belong to another app, and cross-process ownership can block Show.
        result = _method(dialog, 3, ct.c_void_p)(dialog, None)
        if result & 0xffffffff == 0x800704C7:  # ERROR_CANCELLED, including Escape
            return {"status": "cancelled", "path": ""}
        _check(result)
        item, path = ct.c_void_p(), ct.c_void_p()
        try:
            _check(_method(dialog, 20, ct.POINTER(ct.c_void_p))(dialog, ct.byref(item)))
            _check(_method(item, 5, ct.c_ulong, ct.POINTER(ct.c_void_p))(
                item, 0x80058000, ct.byref(path)))  # SIGDN_FILESYSPATH
            return {"status": "selected", "path": ct.wstring_at(path)}
        finally:
            if path:
                ole.CoTaskMemFree(path)
            _release(item)


def pick_folder(*, initial: str, title: str) -> dict[str, str]:
    if not _picker_lock.acquire(blocking=False):
        return {"status": "busy", "detail": "Выбор папки уже открыт."}
    try:
        return _pick_folder(initial=initial, title=title)
    finally:
        _picker_lock.release()
