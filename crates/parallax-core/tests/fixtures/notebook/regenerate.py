#!/usr/bin/env python3
"""Builds and executes the notebook fixtures (needs nbformat, nbclient, ipykernel).

analysis-v1.ipynb        executed notebook: markdown, code with stream/result/image/HTML outputs, an error
analysis-v1-rerun.ipynb  the same cells executed again with different data (outputs and counts differ)
analysis-v2.ipynb        one code line edited, one markdown line edited, one cell added
"""
import os
import nbformat
from nbclient import NotebookClient

PNG = '''
import struct, zlib
from IPython.display import Image, HTML, display
def png(w, h, color):
    raw = b''.join(b'\\x00' + bytes(color) * w for _ in range(h))
    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d))
    return b'\\x89PNG\\r\\n\\x1a\\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')
display(Image(png(8, 4, COLOR)))
'''


def build(variant, seed):
    os.environ["NB_SEED"] = str(seed)
    nb = nbformat.v4.new_notebook()
    nb.metadata["kernelspec"] = {"name": "python3", "display_name": "Python 3 (ipykernel)", "language": "python"}
    title = "# Sales analysis" if variant != 2 else "# Sales analysis, second quarter"
    cells = [
        nbformat.v4.new_markdown_cell(title + "\n\nTotals per *region*."),
        nbformat.v4.new_code_cell("import os, random\nrandom.seed(int(os.environ.get('NB_SEED', '1')))\nsales = {r: random.randint(10, 99) for r in ['north', 'south', 'east']}\nprint('loaded', len(sales), 'regions')"),
        nbformat.v4.new_code_cell("sorted(sales.items())"),
        nbformat.v4.new_code_cell("COLOR = (200, 30, 30) if os.environ.get('NB_SEED', '1') == '1' else (30, 30, 200)" + PNG),
        nbformat.v4.new_code_cell("HTML('<b>total</b>: %d' % sum(sales.values()))"),
        nbformat.v4.new_code_cell("1 / 0" if variant != 2 else "total = sum(sales.values()) / 3\ntotal"),
        nbformat.v4.new_raw_cell("raw text, not executed"),
    ]
    if variant == 2:
        cells.insert(3, nbformat.v4.new_code_cell("for r in sales:\n    print(r, sales[r] * 2)"))
    nb.cells = cells
    NotebookClient(nb, timeout=60, kernel_name="python3", allow_errors=True).execute()
    return nb


for name, variant, seed in [("analysis-v1", 1, 1), ("analysis-v1-rerun", 1, 2), ("analysis-v2", 2, 1)]:
    nb = build(variant, seed)
    nbformat.write(nb, f"{name}.ipynb")
    print(name, len(open(f"{name}.ipynb").read()), "bytes")
