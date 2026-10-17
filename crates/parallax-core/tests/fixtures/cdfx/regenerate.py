#!/usr/bin/env python3
"""Rebuilds the CDFX fixtures from engine-v1.cdfx (hand-written, ASAM CDF 2.0).

- engine-v1-reformatted.cdfx: the same instances in reverse order, written on
  one line each and with numbers formatted differently (1.0 → 1.000).
- engine-v2.cdfx: v1 with two changed values (idle speed, one map cell).
"""
import pathlib
import re

here = pathlib.Path(__file__).parent
v1 = (here / "engine-v1.cdfx").read_text()

head, rest = v1.split("          <SW-INSTANCE>", 1)
body, tail = ("          <SW-INSTANCE>" + rest).rsplit("        </SW-INSTANCE-TREE>", 1)
instances = re.findall(r"          <SW-INSTANCE>.*?\n          </SW-INSTANCE>\n", body, re.S)
assert len(instances) == 6
flat = [re.sub(r">\s+<", "><", i.strip()) + "\n" for i in reversed(instances)]
flat = [i.replace("<V>1.0</V>", "<V>1.000</V>").replace("<V>2000</V>", "<V>2.0E3</V>") for i in flat]
(here / "engine-v1-reformatted.cdfx").write_text(head + "".join(flat) + "        </SW-INSTANCE-TREE>" + tail)

v2 = v1.replace("<V>800</V>", "<V>850</V>").replace("<V>3.25</V>", "<V>3.5</V>")
(here / "engine-v2.cdfx").write_text(v2)
