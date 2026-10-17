package com.example.shop;

import java.io.IOException;
import java.math.BigDecimal;
import java.util.*;
import java.util.function.Function;

@Deprecated(since = "2.0")
public final class OrderService<T extends Comparable<T>> implements Runnable, AutoCloseable {
    public static final long VERSION = 3L;
    public static final String NAME = "orders";
    public static final char SEP = ';';
    private final List<Item> items = new ArrayList<>();
    private volatile boolean open = true;

    public record Item(String name, BigDecimal price, int quantity) {}

    public enum Status { NEW, PAID, SHIPPED }

    public OrderService() {}

    public BigDecimal total(List<Item> items) throws IOException {
        if (items == null) {
            throw new IOException("no items");
        }
        BigDecimal sum = BigDecimal.ZERO;
        for (Item item : items) {
            sum = sum.add(item.price().multiply(BigDecimal.valueOf(item.quantity())));
        }
        return sum;
    }

    public String describe(Status status, int count) {
        String label = switch (status) {
            case NEW -> "new";
            case PAID -> "paid";
            case SHIPPED -> "shipped";
        };
        return "Order " + label + " with " + count + " items";
    }

    public int bucket(int code) {
        switch (code) {
            case 1: return 10;
            case 2: return 20;
            case 3: return 30;
            case 1000: return 99;
            default: return -1;
        }
    }

    public List<String> names() {
        return items.stream().map(i -> i.name().toUpperCase()).sorted().toList();
    }

    @SafeVarargs
    public final <R> List<R> convert(Function<Item, R> f, Item... extra) {
        List<R> out = new ArrayList<>();
        Runnable log = () -> System.out.println("convert " + extra.length);
        log.run();
        for (Item i : extra) out.add(f.apply(i));
        return out;
    }

    public synchronized void run() {
        try {
            close();
        } catch (Exception e) {
            open = false;
        }
    }

    @Override
    public void close() throws Exception {
        open = false;
    }
}
