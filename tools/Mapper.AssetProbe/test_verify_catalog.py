import math
import struct
import unittest
from verify_catalog import equal, exact_differences


class Precision(unittest.TestCase):
    def test_integer_masks_and_booleans_are_never_float_approximated(self):
        self.assertFalse(equal(16777216, 16777217))
        self.assertFalse(equal(1, True))
        self.assertFalse(equal(False, 0))
        self.assertTrue(equal(-1, -1))

    def test_source_float_bits_match_widened_and_short_roundtrip_values(self):
        widened = struct.unpack('<f', struct.pack('<f', 0.3))[0]
        self.assertTrue(equal(0.3, widened))
        self.assertFalse(equal(0.3, 0.3001))
        self.assertFalse(equal(0.0, -0.0))
        self.assertFalse(equal(math.nan, math.nan))
        self.assertFalse(equal({'mask': 1}, {'mask': 1, 'extra': 2}))

    def test_complete_catalog_checks_geometry_without_float_tolerance_or_type_coercion(self):
        self.assertTrue(exact_differences({'center':1.0},{'center':math.nextafter(1.0,2.0)}))
        self.assertTrue(exact_differences({'count':1.0},{'count':1}))
        self.assertTrue(exact_differences({'flags':True},{'flags':1}))
        self.assertFalse(exact_differences({'center':[0.25,0.5]},{'center':[0.25,0.5]}))


if __name__ == '__main__':
    unittest.main()
