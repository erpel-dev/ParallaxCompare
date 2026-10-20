# Automotive comparison fixtures

`regenerate.py` creates these small fixtures using only the Python standard
library. They contain invented engine calibration and diagnostic definitions.

- DCM 2.0: scalar, text, value block, curve, map, axis and grouped curve, with
  units and metadata. The reformatted copy changes object/metadata order, line
  wrapping, comments and decimal spelling. Version 2 changes a scalar and a map
  cell.
- ODX 2.2.0: one base variant, two UDS services, request/positive-response
  parameters and a referenced ASCII data object. The reformatted copy changes
  definition and attribute order and indentation. Version 2 changes a DID and a
  data object's bit length.
- PDX: the corresponding ODX file, a catalog and a small binary flash payload.
  The repacked version also changes ZIP compression. ZIP timestamps are fixed
  for reproducibility.

The ODX and PDX fixtures can also be loaded independently with `odxtools`:

```python
from odxtools.loadfile import load_file
db = load_file("engine-v1.pdx")
print([service.short_name for layer in db.diag_layers for service in layer.services])
```

The diagnostics view preserves XML data rather than validating the complete ODX
schema or resolving cross-document references. Unknown/rich-text whitespace and
ordered parameter lists are retained conservatively. DCM files over 64 MiB and
objects over two million values are rejected by the structured view; ODX is
limited to 64 MiB, 250,000 elements and 128 nesting levels. These limits report
an error, allowing the normal raw-text fallback, rather than hiding data.
