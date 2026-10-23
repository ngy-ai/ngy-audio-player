"""抓一帧播放器窗口，存成 PNG，并打印窗口状态。

用法：python scripts/shot.py <pid> <输出路径> [点击坐标 x,y ...]
"""
import ctypes
import ctypes.wintypes as w
import os
import sys
import time

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

user32.SetProcessDPIAware()


def find_window(pid):
    result = []

    @ctypes.WINFUNCTYPE(ctypes.c_bool, w.HWND, w.LPARAM)
    def callback(hwnd, _):
        owner = w.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd):
            length = user32.GetWindowTextLengthW(hwnd)
            if length:
                buffer = ctypes.create_unicode_buffer(length + 1)
                user32.GetWindowTextW(hwnd, buffer, length + 1)
                result.append((hwnd, buffer.value))
        return True

    user32.EnumWindows(callback, 0)
    return result


def client_offset(hwnd):
    rect = w.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    point = w.POINT(0, 0)
    user32.ClientToScreen(hwnd, ctypes.byref(point))
    return point.x - rect.left, point.y - rect.top


def click(hwnd, x, y):
    off_x, off_y = client_offset(hwnd)
    cx, cy = int(x - off_x), int(y - off_y)
    lparam = (cy << 16) | (cx & 0xFFFF)
    user32.PostMessageW(hwnd, 0x0200, 0, lparam)
    time.sleep(0.05)
    user32.PostMessageW(hwnd, 0x0201, 1, lparam)
    time.sleep(0.08)
    user32.PostMessageW(hwnd, 0x0202, 0, lparam)


def shot(hwnd, path):
    rect = w.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    width = rect.right - rect.left
    height = rect.bottom - rect.top
    hdc = user32.GetWindowDC(hwnd)
    mem = gdi32.CreateCompatibleDC(hdc)
    bitmap = gdi32.CreateCompatibleBitmap(hdc, width, height)
    gdi32.SelectObject(mem, bitmap)
    ok = user32.PrintWindow(hwnd, mem, 2)

    class BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [
            ("biSize", w.DWORD), ("biWidth", w.LONG), ("biHeight", w.LONG),
            ("biPlanes", w.WORD), ("biBitCount", w.WORD), ("biCompression", w.DWORD),
            ("biSizeImage", w.DWORD), ("biXPelsPerMeter", w.LONG),
            ("biYPelsPerMeter", w.LONG), ("biClrUsed", w.DWORD), ("biClrImportant", w.DWORD),
        ]

    header = BITMAPINFOHEADER()
    header.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    header.biWidth = width
    header.biHeight = -height  # 负值 = 自上而下
    header.biPlanes = 1
    header.biBitCount = 32
    header.biCompression = 0

    buffer = ctypes.create_string_buffer(width * height * 4)
    lines = gdi32.GetDIBits(mem, bitmap, 0, height, buffer,
                            ctypes.byref(header), 0)
    gdi32.DeleteObject(bitmap)
    gdi32.DeleteDC(mem)
    user32.ReleaseDC(hwnd, hdc)

    from PIL import Image
    image = Image.frombuffer("RGBA", (width, height), buffer, "raw", "BGRA", 0, 1)
    image.convert("RGB").save(path)
    return ok, lines, (width, height)


def main():
    pid = int(sys.argv[1])
    out = sys.argv[2]
    windows = find_window(pid)
    if not windows:
        print("没有找到可见窗口")
        return 1
    hwnd, title = windows[0]
    print(f"窗口: {title!r} hwnd={hwnd}")
    print(f"偏移: {client_offset(hwnd)}")

    for arg in sys.argv[3:]:
        x, y = (int(part) for part in arg.split(","))
        click(hwnd, x, y)
        print(f"点击 {x},{y}")

    # 戳几下让窗口重绘，否则 PrintWindow 拿到的是上一帧。
    for _ in range(3):
        user32.PostMessageW(hwnd, 0x0200, 0, (10 << 16) | 10)
        time.sleep(0.06)
    time.sleep(0.5)

    ok, lines, size = shot(hwnd, out)
    print(f"PrintWindow={ok} GetDIBits={lines} 尺寸={size} -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
