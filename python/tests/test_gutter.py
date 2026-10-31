import base64
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from zenteru.gutter import encode_matrix  # noqa: E402


class EncodeMatrixTest(unittest.TestCase):
    def test_cell_forms_and_padding(self):
        m = [[(255, 0, 0), "#00ff0080", None], [(1, 2, 3, 4)]]
        raw = base64.b64decode(encode_matrix(m, 3, 2))
        self.assertEqual(len(raw), 3 * 2 * 4)
        self.assertEqual(raw[0:4], bytes([255, 0, 0, 255]))
        self.assertEqual(raw[4:8], bytes([0, 255, 0, 128]))
        self.assertEqual(raw[8:12], bytes(4))
        self.assertEqual(raw[12:16], bytes([1, 2, 3, 4]))
        self.assertEqual(raw[16:24], bytes(8))  # padded

    def test_clamps_and_crops(self):
        raw = base64.b64decode(encode_matrix([[(300, -5, 10)] * 5], 2, 1))
        self.assertEqual(raw, bytes([255, 0, 10, 255]) * 2)


if __name__ == "__main__":
    unittest.main()
