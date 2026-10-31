#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Capture only the isolated Xvfb test display through system GDK.
Never use this utility against the user's desktop or an account login page.
"""
import ctypes as c
import sys

def capture(path):
    gdk = c.CDLL('libgdk-3.so.0')
    pixbuf = c.CDLL('libgdk_pixbuf-2.0.so.0')
    gobject = c.CDLL('libgobject-2.0.so.0')
    gdk.gdk_init.argtypes = [c.c_void_p, c.c_void_p]
    gdk.gdk_init(None, None)
    gdk.gdk_get_default_root_window.restype = c.c_void_p
    window = gdk.gdk_get_default_root_window()
    assert window, 'isolated Xvfb root window is unavailable'
    gdk.gdk_window_get_width.argtypes = [c.c_void_p]
    gdk.gdk_window_get_height.argtypes = [c.c_void_p]
    gdk.gdk_pixbuf_get_from_window.argtypes = [c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_int]
    gdk.gdk_pixbuf_get_from_window.restype = c.c_void_p
    image = gdk.gdk_pixbuf_get_from_window(window, 0, 0, gdk.gdk_window_get_width(window), gdk.gdk_window_get_height(window))
    assert image, 'cannot capture isolated application'
    pixbuf.gdk_pixbuf_savev.argtypes = [c.c_void_p, c.c_char_p, c.c_char_p, c.c_void_p, c.c_void_p, c.c_void_p]
    assert pixbuf.gdk_pixbuf_savev(image, path.encode(), b'png', None, None, None)
    gobject.g_object_unref.argtypes = [c.c_void_p]
    gobject.g_object_unref(image)

if __name__ == '__main__':
    capture(sys.argv[1])
