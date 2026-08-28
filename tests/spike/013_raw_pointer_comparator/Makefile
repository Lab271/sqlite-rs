# Spike 013: Raw-Pointer Comparator Trick Investigation
# Usage: make test | make bench

.PHONY: test bench clean

# Confirm the three comparators agree before trusting any timing.
test:
	cargo test

# Head-to-head: regular (general decode) vs safe_fast (#631's shipped
# fast path) vs unsafe_trick (literal sqlite3 vdbesort.c port).
bench:
	cargo bench

clean:
	cargo clean
