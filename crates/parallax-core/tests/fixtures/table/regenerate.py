#!/usr/bin/env python3
"""Writes the CSV fixtures with Python's csv module.

products.csv            40 products, ids 1000-1039, names with commas, quotes and a line break
products-shuffled.csv   the same rows in another order (as a database export might return them)
products-semicolon.csv  the same rows with ';', every field quoted, CRLF (Excel in a German locale)
products-edited.csv     shuffled, with the price of product 1017 changed
"""
import csv
import random

rows = []
for i in range(40):
    name = ["Bolt M%d" % (i + 3), 'Washer, "heavy"', "Nut\nself-locking", "Spring"][i % 4]
    rows.append([1000 + i, name, "%.2f" % (0.05 * (i + 1)), ["steel", "brass", "", "nylon"][i % 4]])
header = ["id", "name", "price", "material"]


def write(path, data, **kw):
    with open(path, "w", newline="") as f:
        w = csv.writer(f, **kw)
        w.writerow(header)
        w.writerows(data)


write("products.csv", rows, lineterminator="\n")
shuffled = rows[:]
random.Random(7).shuffle(shuffled)
write("products-shuffled.csv", shuffled, lineterminator="\n")
write("products-semicolon.csv", rows, delimiter=";", quoting=csv.QUOTE_ALL, lineterminator="\r\n")
edited = [r[:] for r in shuffled]
for r in edited:
    if r[0] == 1017:
        r[2] = "9.99"
write("products-edited.csv", edited, lineterminator="\n")
