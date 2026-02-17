# AWK Pattern Test: Comma-Separated Attributes

This file tests rust code blocks with comma-separated attributes (```rust,ignore).

```rust,ignore
fn comma_separated_attributes() {
    this_wont_compile
}
```

```rust,no_run
fn another_comma_attribute() {
    std::process::exit(1);
}
```

Expected: Should match and extract both blocks with their attributes.
