#!/bin/sh
# Compiles the class files the tests read (checked in, so tests need no JDK).
#   OrderService-v1.class          javac -g of v1
#   OrderService-v1-shifted.class  the same code three lines further down (line numbers differ)
#   OrderService-v1-nodebug.class  javac -g:none of v1
#   OrderService-v1-java17.class   javac --release 17 of v1
#   OrderService-v2.class          v2: a changed constant, method body and lambda, a new method
#   OrderService-Item.class, OrderService-Status.class: the nested record and enum of v1
set -e
cd "$(dirname "$0")"
out=$(mktemp -d)
compile() { # <flags> <source> <name>
    mkdir -p "$out/$3/src/com/example/shop" "$out/$3/classes"
    cp "$2" "$out/$3/src/com/example/shop/OrderService.java"
    javac $1 -d "$out/$3/classes" "$out/$3/src/com/example/shop/OrderService.java"
    cp "$out/$3/classes/com/example/shop/OrderService.class" "OrderService-$3.class"
}
compile -g OrderService-v1.java v1
cp "$out/v1/classes/com/example/shop/OrderService\$Item.class" OrderService-Item.class
cp "$out/v1/classes/com/example/shop/OrderService\$Status.class" OrderService-Status.class
{ sed -n 1p OrderService-v1.java; printf '// A comment added above the class,\n// which moves every line of code down\n// and changes the line number tables.\n'; sed 1d OrderService-v1.java; } > "$out/shifted.java"
compile -g "$out/shifted.java" v1-shifted
compile -g:none OrderService-v1.java v1-nodebug
compile "-g --release 17" OrderService-v1.java v1-java17
compile -g OrderService-v2.java v2
rm -rf "$out"
