import unittest

from terminal_style import has_foreground


class ForegroundTest(unittest.TestCase):
    def test_equivalent_palette_encodings_and_combined_runs(self):
        for prefix in [b"32", b"1;32", b"38;5;2", b"38:5:2"]:
            with self.subTest(prefix=prefix):
                self.assertTrue(has_foreground(
                    b"\x1b[" + prefix + b"mSYN\x1b[22mTAX",
                    b"SYNTAX", ("ansi", 2)))

    def test_resets_background_and_partial_coloring_do_not_pass(self):
        for data in [
            b"\x1b[42mSYNTAX", b"\x1b[32m\x1b[0mSYNTAX",
            b"\x1b[32mSYN\x1b[39mTAX", b"\x1b[32mSYN\x1b[31mTAX",
        ]:
            with self.subTest(data=data):
                self.assertFalse(has_foreground(data, b"SYNTAX", ("ansi", 2)))

    def test_rgb_encodings_are_compared_by_components(self):
        for code in [b"38;2;1;2;3", b"38:2::1:2:3"]:
            self.assertTrue(has_foreground(b"\x1b[" + code + b"mvalue", b"value", ("rgb", 1, 2, 3)))


if __name__ == "__main__":
    unittest.main()
