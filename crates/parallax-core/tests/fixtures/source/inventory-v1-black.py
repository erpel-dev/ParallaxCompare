"""Stock keeping for the shop."""

import json
from dataclasses import dataclass, field
from pathlib import Path

LOW_STOCK = 5  # warn below this
CURRENCIES = (
    "EUR",
    "USD",
)


@dataclass
class Item:
    sku: str
    name: str
    price: float = 0.0
    tags: list = field(default_factory=list)

    def label(self):
        return "%s (%s)" % (self.name, self.sku)


class Inventory(object):
    """All items, by SKU."""

    def __init__(self, path):
        self.path = Path(path)
        self.items = {}

    def load(self):
        data = json.loads(self.path.read_text())
        for row in data:
            self.add(Item(**row))

    def add(self, item):
        if item.sku in self.items:
            raise KeyError(item.sku)
        self.items[item.sku] = item

    def low_stock(self, counts, limit=LOW_STOCK):
        return [
            self.items[sku]
            for sku, n in counts.items()
            if n < limit and sku in self.items
        ]

    def total(self, counts):
        return sum(self.items[sku].price * n for sku, n in counts.items())


def main(argv=None):
    inv = Inventory(argv[1] if argv else "stock.json")
    inv.load()
    print(len(inv.items), "items")


if __name__ == "__main__":
    import sys

    main(sys.argv)
