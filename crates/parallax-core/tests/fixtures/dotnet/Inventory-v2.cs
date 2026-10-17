using System;
using System.Collections.Generic;
using System.Reflection;

[assembly: AssemblyVersion("1.3.0.0")]
[assembly: AssemblyTitle("Inventory")]

namespace Shop.Stock
{
    public interface IPriced
    {
        decimal Price { get; }
    }

    public enum Unit : byte { Piece, Kilogram, Litre = 10 }

    public struct Quantity
    {
        public readonly double Amount;
        public readonly Unit Unit;
        public Quantity(double amount, Unit unit) { Amount = amount; Unit = unit; }
        public override string ToString() => Amount + " " + Unit;
    }

    public sealed class Item : IPriced, IComparable<Item>
    {
        public const int MaxNameLength = 40;
        private static int created;

        public string Name { get; }
        public decimal Price { get; private set; }
        public Quantity Stock;

        public Item(string name, decimal price)
        {
            if (name.Length > MaxNameLength) throw new ArgumentException("name too long", nameof(name));
            Name = name;
            Price = price;
            created++;
        }

        public int CompareTo(Item other) => string.CompareOrdinal(Name, other?.Name);

        public void Discount(int percent)
        {
            if (percent < 0 || percent > 100) throw new ArgumentOutOfRangeException(nameof(percent));
            Price = Price * (100 - percent) / 100;
        }

        public static int Created => created;

        public bool IsExpensive(decimal limit) => Price > limit;
    }

    public class Warehouse<T> where T : class, IPriced
    {
        private readonly List<T> items = new List<T>();
        public event EventHandler Changed;

        public void Add(T item)
        {
            items.Add(item);
            Changed?.Invoke(this, EventArgs.Empty);
        }

        public decimal Total()
        {
            decimal sum = 0;
            foreach (var i in items) sum += i.Price;
            return sum;
        }

        protected internal virtual int Count(Func<T, bool> filter)
        {
            int n = 0;
            for (int i = 0; i < items.Count; i++)
            {
                if (filter(items[i])) n++;
            }
            return n;
        }

        internal static string Describe(object o, ref int counter, out bool ok)
        {
            counter++;
            ok = o != null;
            return o switch { Item it => it.Name, string s => s, _ => "?" };
        }
    }
}
