import unittest

import versions
from versions import Shown

D = "2026-11-19"


class ParseTests(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(versions.parse("1.2.3"), (1, 2, 3, 2, 0))
        self.assertEqual(versions.parse("0.1.0-beta.5"), (0, 1, 0, 1, 5))
        self.assertEqual(versions.parse("0.1.0-alpha.4"), (0, 1, 0, 0, 4))

    def test_not_a_release(self):
        for t in ["1.2", "v1.2.3", "0.1.0-rc.1", "0.1.0-beta", "0.1.0-beta.x",
                  "next", "", "1.2.3 ", "1.2.3-beta.1.2"]:
            self.assertIsNone(versions.parse(t), t)

    def test_numbers_compare_as_numbers(self):
        self.assertGreater(versions.parse("0.1.0-beta.10"), versions.parse("0.1.0-beta.9"))
        self.assertGreater(versions.parse("0.10.0"), versions.parse("0.9.0"))
        self.assertGreater(versions.parse("0.1.0"), versions.parse("0.1.0-beta.99"))
        self.assertGreater(versions.parse("0.1.0-beta.1"), versions.parse("0.1.0-alpha.99"))

    def test_channel(self):
        self.assertEqual(versions.channel("1.2.3"), "stable")
        self.assertEqual(versions.channel("0.1.0-beta.5"), "beta")
        self.assertEqual(versions.channel("0.1.0-alpha.4"), "alpha")


class PickTests(unittest.TestCase):
    def shown(self, *vs):
        return [(s.channel, s.version) for s in versions.pick({v: D for v in vs})]

    def test_spec_table(self):
        self.assertEqual(self.shown("0.1.0-beta.5", "0.1.0-alpha.4"), [("beta", "0.1.0-beta.5")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.1.0-alpha.4"), [("stable", "0.1.0")])
        self.assertEqual(self.shown("0.1.0", "0.2.0-beta.1", "0.2.0-alpha.3"),
                         [("stable", "0.1.0"), ("beta", "0.2.0-beta.1")])
        self.assertEqual(self.shown("0.1.2", "0.2.0-beta.1", "0.3.0-alpha.1"),
                         [("stable", "0.1.2"), ("beta", "0.2.0-beta.1"), ("alpha", "0.3.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0", "0.1.0-beta.7", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("alpha", "0.2.0-alpha.1")])
        self.assertEqual(self.shown("0.1.0-alpha.4"), [("alpha", "0.1.0-alpha.4")])
        self.assertEqual(self.shown(), [])

    def test_alpha_is_held_against_a_hidden_beta(self):
        # The beta is hidden by stable 0.2.0, and still hides the alpha.
        self.assertEqual(self.shown("0.2.0", "0.2.0-beta.1", "0.2.0-alpha.9"), [("stable", "0.2.0")])
        # An alpha between a hidden beta and the stable: newer than the stable
        # is not enough, it must beat the beta too.
        self.assertEqual(self.shown("0.1.0", "0.3.0-beta.1", "0.2.0-alpha.1"),
                         [("stable", "0.1.0"), ("beta", "0.3.0-beta.1")])

    def test_newest_of_each_channel_and_junk_ignored(self):
        self.assertEqual(self.shown("0.1.0-beta.9", "0.1.0-beta.10", "0.1.0-rc.1", "next"),
                         [("beta", "0.1.0-beta.10")])

    def test_keeps_the_date(self):
        self.assertEqual(versions.pick({"1.0.0": "2027-03-03"}), [Shown("stable", "1.0.0", "2027-03-03")])


if __name__ == "__main__":
    unittest.main()
