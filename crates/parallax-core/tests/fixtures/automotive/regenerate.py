#!/usr/bin/env python3
"""Generate the small DCM, ODX and PDX comparison fixtures (stdlib only)."""

from copy import deepcopy
from pathlib import Path
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).parent
XSI = "http://www.w3.org/2001/XMLSchema-instance"
ET.register_namespace("xsi", XSI)

blocks = [
    '''FESTWERT K_IdleSpeed
  LANGNAME "Idle speed"
  EINHEIT_W "rpm"
  WERT 800
END''',
    '''TEXTSTRING T_Variant
  LANGNAME "Vehicle variant"
  TEXT "EU6 diesel"
END''',
    '''FESTWERTEBLOCK KA_Offsets 3
  EINHEIT_W "V"
  WERT 0.125 0.250 0.375
END''',
    '''KENNLINIE KL_Advance 3
  EINHEIT_X "rpm"
  EINHEIT_W "deg"
  ST/X 1000 2000 3000
  WERT 5 10.5 18
END''',
    '''KENNFELD KF_Fuel 3 2
  EINHEIT_X "rpm"
  EINHEIT_Y "%"
  EINHEIT_W "mg"
  ST/X 1000 2000 3000
  ST/Y 25
  WERT 1 2 3
  ST/Y 75
  WERT 4 5 6
END''',
    '''STUETZSTELLENVERTEILUNG AX_Speed 3
  EINHEIT_X "rpm"
  ST/X 1000 2000 3000
END''',
    '''GRUPPENKENNLINIE KL_Limit 3
  SSTX "AX_Speed"
  EINHEIT_W "Nm"
  WERT 100 150 125
END''',
]
header = 'KONSERVIERUNG_FORMAT 2.0\nMODULKOPF "Dataset" "Engine"\n\n'
base = header + "\n\n".join(blocks) + "\n"
(ROOT / "engine-v1.dcm").write_text(base)
reformatted = "* Exported with other spacing and order\n" + header
for block in reversed(blocks):
    lines = block.splitlines()
    metadata = [s for s in lines[1:-1] if s.strip().startswith(("LANGNAME", "EINHEIT_", "SSTX"))]
    data = [s for s in lines[1:-1] if s not in metadata]
    text = "\n".join([lines[0], *reversed(metadata), *data, "END"])
    text = text.replace("WERT 800", "WERT +8.000e2").replace("0.250", "2.5e-1")
    text = text.replace("ST/X 1000 2000 3000", "ST/X 1000.0\n  ST/X 2000 3000")
    reformatted += text + "\n\n"
(ROOT / "engine-v1-reformatted.dcm").write_text(reformatted)
(ROOT / "engine-v2.dcm").write_text(base.replace("WERT 800", "WERT 850").replace("WERT 4 5 6", "WERT 4 5 6.5"))


def child(parent, tag, text=None, **attrs):
    node = ET.SubElement(parent, tag, attrs)
    if text is not None:
        node.text = str(text)
    return node


def coded(params, name, byte, bits, value):
    p = child(params, "PARAM", **{f"{{{XSI}}}type": "CODED-CONST"})
    child(p, "SHORT-NAME", name)
    child(p, "BYTE-POSITION", byte)
    t = child(p, "DIAG-CODED-TYPE", **{"BASE-DATA-TYPE": "A_UINT32", f"{{{XSI}}}type": "STANDARD-LENGTH-TYPE"})
    child(t, "BIT-LENGTH", bits)
    child(p, "CODED-VALUE", value)


root = ET.Element("ODX", {"MODEL-VERSION": "2.2.0"})
container = child(root, "DIAG-LAYER-CONTAINER", ID="DLC.Engine")
child(container, "SHORT-NAME", "Engine")
child(container, "LONG-NAME", "Engine diagnostic definitions")
variant = child(child(container, "BASE-VARIANTS"), "BASE-VARIANT", ID="BV.Engine")
child(variant, "SHORT-NAME", "EngineBase")
dictionary = child(variant, "DIAG-DATA-DICTIONARY-SPEC")
dop = child(child(dictionary, "DATA-OBJECT-PROPS"), "DATA-OBJECT-PROP", ID="DOP.SerialNumber")
child(dop, "SHORT-NAME", "SerialNumber")
dt = child(dop, "DIAG-CODED-TYPE", **{"BASE-DATA-TYPE": "A_ASCIISTRING", f"{{{XSI}}}type": "STANDARD-LENGTH-TYPE"})
child(dt, "BIT-LENGTH", 128)
child(dop, "PHYSICAL-TYPE", **{"BASE-DATA-TYPE": "A_ASCIISTRING"})
child(child(dop, "COMPU-METHOD"), "CATEGORY", "IDENTICAL")
comms = child(variant, "DIAG-COMMS")
requests = child(variant, "REQUESTS")
responses = child(variant, "POS-RESPONSES")
for name, sid in [("ReadSerialNumber", 34), ("TesterPresent", 62)]:
    service = child(comms, "DIAG-SERVICE", ID=f"Service.{name}", SEMANTIC="IDENTIFICATION" if sid == 34 else "SESSION")
    child(service, "SHORT-NAME", name)
    child(service, "REQUEST-REF", **{"ID-REF": f"Request.{name}"})
    child(child(service, "POS-RESPONSE-REFS"), "POS-RESPONSE-REF", **{"ID-REF": f"Response.{name}"})
    request = child(requests, "REQUEST", ID=f"Request.{name}")
    child(request, "SHORT-NAME", name)
    params = child(request, "PARAMS")
    coded(params, "SID", 0, 8, sid)
    if sid == 34:
        coded(params, "DID", 1, 16, 61840)
    response = child(responses, "POS-RESPONSE", ID=f"Response.{name}")
    child(response, "SHORT-NAME", f"{name}Positive")
    params = child(response, "PARAMS")
    coded(params, "SID", 0, 8, sid + 64)
    if sid == 34:
        coded(params, "DID", 1, 16, 61840)
        value = child(params, "PARAM", **{f"{{{XSI}}}type": "VALUE"})
        child(value, "SHORT-NAME", "SerialNumber")
        child(value, "BYTE-POSITION", 3)
        child(value, "DOP-REF", **{"ID-REF": "DOP.SerialNumber"})


def xml(node, indent="  "):
    node = deepcopy(node)
    ET.indent(node, space=indent)
    return ET.tostring(node, encoding="utf-8", xml_declaration=True) + b"\n"


v1 = xml(root)
changed = deepcopy(root)
changed.find(".//DATA-OBJECT-PROP/DIAG-CODED-TYPE/BIT-LENGTH").text = "160"
changed.find(".//REQUEST/PARAMS/PARAM[2]/CODED-VALUE").text = "61841"
v2 = xml(changed)
reordered = deepcopy(root)
for tag in ["DIAG-COMMS", "REQUESTS", "POS-RESPONSES"]:
    node = reordered.find(f".//{tag}")
    node[:] = reversed(list(node))
for node in reordered.iter():
    node.attrib = dict(reversed(list(node.attrib.items())))
v1_reformatted = xml(reordered, "\t")
for name, data in [("engine-v1.odx-d", v1), ("engine-v1-reformatted.odx-d", v1_reformatted), ("engine-v2.odx-d", v2)]:
    (ROOT / name).write_bytes(data)

index = b'''<?xml version="1.0" encoding="utf-8"?>
<CATALOG xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" F-DTD-VERSION="ODX-2.2.0">
  <SHORT-NAME>Engine</SHORT-NAME>
  <ABLOCKS><ABLOCK><SHORT-NAME>Diagnostics</SHORT-NAME><FILES>
    <FILE MIME-TYPE="application/x-asam.odx.odx-d">diagnostics/engine.odx-d</FILE>
  </FILES></ABLOCK></ABLOCKS>
</CATALOG>
'''
for name, data, method in [("engine-v1.pdx", v1, zipfile.ZIP_DEFLATED), ("engine-v1-repacked.pdx", v1_reformatted, zipfile.ZIP_STORED), ("engine-v2.pdx", v2, zipfile.ZIP_DEFLATED)]:
    with zipfile.ZipFile(ROOT / name, "w", method) as archive:
        for path, payload in [("index.xml", index), ("diagnostics/engine.odx-d", data), ("flash/payload.bin", b"\x00\x01\x02\x03")]:
            info = zipfile.ZipInfo(path, (2026, 1, 1, 0, 0, 0))
            info.compress_type = method
            archive.writestr(info, payload)
