# AWK Pattern Test: Space-Separated Attributes

This file tests rust code blocks with space-separated attributes (```rust ignore).

```rust ignore
fn space_separated_attributes() {
    this_wont_compile
}
```

Expected: Should match and extract this block (note: space-separated is less common).
