#!/usr/bin/env python3
"""Rebuilds the DBC fixtures from powertrain-v1.dbc (hand-written, checked with cantools).

- powertrain-v1-reordered.dbc: v1 loaded and written again by cantools, which
  reorders sections and reformats numbers: the same database.
- powertrain-v2.dbc: v1 with a changed factor, the CruiseControl message moved to
  the top, and a new signal FuelRate.

Needs `pip install cantools`.
"""
import pathlib

import cantools

here = pathlib.Path(__file__).parent
v1 = (here / "powertrain-v1.dbc").read_text()

db = cantools.database.load_string(v1, database_format="dbc", strict=True)
(here / "powertrain-v1-reordered.dbc").write_text(db.as_dbc_string())

v2 = v1.replace('SG_ EngineSpeed : 0|16@1+ (0.25,0)', 'SG_ EngineSpeed : 0|16@1+ (0.125,0)')
cruise = 'BO_ 2566844672 CruiseControl: 8 Dashboard\n SG_ SetSpeed : 0|8@1+ (1,0) [0|250] "km/h"  Engine\n\n'
v2 = v2.replace(cruise, "").replace("BO_ 200 EngineData", cruise + "BO_ 200 EngineData")
v2 = v2.replace(
    ' SG_ EngineRunning : 48|1@1+ (1,0) [0|1] ""  Dashboard,Transmission\n',
    ' SG_ EngineRunning : 48|1@1+ (1,0) [0|1] ""  Dashboard,Transmission\n'
    ' SG_ FuelRate : 56|8@1+ (0.2,0) [0|51] "l/h"  Dashboard\n',
)
v2 = v2.replace('CM_ SG_ 1024 DiagMux', 'CM_ SG_ 200 FuelRate "Injected fuel per hour";\nCM_ SG_ 1024 DiagMux')
assert v2 != v1
cantools.database.load_string(v2, database_format="dbc", strict=True)
(here / "powertrain-v2.dbc").write_text(v2)
