"""Inspect effective SGR foreground runs, not terminal geometry or screen pixels."""
import re

CSI = re.compile(rb"\x1b\[([0-?]*)(?:[ -/]*)([@-~])")


def foreground_after(parameters, foreground):
    parts = parameters.split(b";") if parameters else [b"0"]
    index = 0
    while index < len(parts):
        part = parts[index]
        index += 1
        if b":" in part:
            values = part.split(b":")
            if values[0] == b"38":
                if len(values) == 3 and values[1] == b"5":
                    foreground = ("ansi", int(values[2]))
                elif len(values) >= 5 and values[1] == b"2":
                    foreground = ("rgb", *(int(value) for value in values[-3:]))
            continue
        if part and not part.isdigit():
            continue
        code = int(part or b"0")
        if code in (0, 39):
            foreground = None
        elif 30 <= code <= 37:
            foreground = ("ansi", code - 30)
        elif 90 <= code <= 97:
            foreground = ("ansi", code - 90 + 8)
        elif code == 38 and index < len(parts):
            kind = parts[index]
            count = 1 if kind == b"5" else 3 if kind == b"2" else 0
            if count and index + count < len(parts):
                values = tuple(int(value) for value in parts[index + 1:index + count + 1])
                foreground = (("ansi",) if count == 1 else ("rgb",)) + values
                index += count + 1
    return foreground


def has_foreground(data, needle, expected):
    """Find the entire literal under one effective foreground across SGR runs."""
    text = bytearray()
    colors = []
    foreground = None
    start = 0
    for escape in CSI.finditer(data):
        plain = data[start:escape.start()]
        text.extend(plain)
        colors.extend([foreground] * len(plain))
        if escape[2] == b"m":
            foreground = foreground_after(escape[1], foreground)
        start = escape.end()
    text.extend(data[start:])
    colors.extend([foreground] * (len(data) - start))
    offset = text.find(needle)
    while offset >= 0:
        if all(color == expected for color in colors[offset:offset + len(needle)]):
            return True
        offset = text.find(needle, offset + 1)
    return False
