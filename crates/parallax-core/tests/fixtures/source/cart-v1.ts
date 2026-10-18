import { formatPrice } from './format'
import type { Item } from "./types"

export const TAX_RATE = 0.19

export interface CartLine { item: Item, quantity: number }

export class Cart {
  private lines: CartLine[] = []

  constructor(private readonly currency: string = 'EUR') {}

  add(item: Item, quantity = 1): this {
    const line = this.lines.find(l => l.item.id === item.id)
    if (line) line.quantity += quantity
    else this.lines.push({ item, quantity })
    return this
  }

  get total(): number {
    return this.lines.reduce((sum, l) => sum + (l.item.price * l.quantity), 0) * (1 + TAX_RATE)
  }

  toString() { return `${this.lines.length} items, ${formatPrice(this.total, this.currency)}` }
}

export function isEmpty(cart: Cart) { return cart.total === 0 }
