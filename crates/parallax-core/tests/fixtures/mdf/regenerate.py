#!/usr/bin/env python3
"""Rebuilds the MDF fixtures with asammdf (`pip install asammdf`).

drive-v1.mf4: two channel groups (engine and gearbox), a linear conversion, a
value-to-text table, an attachment and header metadata. drive-v2.mf4 adds an
oil pressure channel and changes the coolant unit to K. drive-v1.mdf is v1
saved as MDF 3.30.
"""
import datetime
import pathlib

import numpy as np
from asammdf import MDF, Signal

here = pathlib.Path(__file__).parent


def build(version, v2=False):
    t = np.arange(100, dtype=np.float64) * 0.01
    mdf = MDF(version=version)
    mdf.header.start_time = datetime.datetime(2026, 10, 1, 8, 30, tzinfo=datetime.timezone.utc)
    mdf.header.comment = "Test drive on the proving ground"
    mdf.header.author = "Test Engineer"
    mdf.header.project = "EDC42"
    engine = [
        Signal(np.arange(100, dtype=np.uint16) * 40, t, name="EngineSpeed", unit="rpm",
               conversion={"a": 0.25, "b": 0}, comment="Crankshaft speed"),
        Signal(np.linspace(20, 90, 100).astype(np.float32), t, name="CoolantTemp",
               unit="K" if v2 else "degC", comment="Coolant temperature"),
        Signal(np.linspace(0, 100, 100).astype(np.float32), t, name="Throttle", unit="%"),
    ]
    if v2:
        engine.append(Signal(np.linspace(1, 4, 100).astype(np.float32), t, name="OilPressure", unit="bar"))
    mdf.append(engine, acq_name="Engine", comment="Engine ECU signals")
    gear = Signal(
        (np.arange(100) // 40).astype(np.uint8), t, name="Gear",
        conversion={"val_0": 0, "text_0": b"Neutral", "val_1": 1, "text_1": b"First", "val_2": 2, "text_2": b"Second"},
    )
    mdf.append([gear], acq_name="Gearbox")
    if version >= "4.00":
        mdf.attach(b"[calibration]\nversion=1.4\n", file_name="calibration.ini", comment="Calibration set used")
    return mdf


build("4.10").save(here / "drive-v1.mf4", overwrite=True)
build("4.10", v2=True).save(here / "drive-v2.mf4", overwrite=True)
build("3.30").save(here / "drive-v1.mdf", overwrite=True)
