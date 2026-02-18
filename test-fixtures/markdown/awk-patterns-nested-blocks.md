# AWK Pattern Test: Nested Code Blocks

This file tests nested code blocks and recovery.

```rust
fn outer_block() {
    println!("This is the outer block");
    // Imagine this demonstrates code with embedded markdown examples
}
```

Here's some text between blocks.

```rust
fn recovery_block() {
    println!("This block should still be extracted");
}
```

Expected: Both blocks should be extracted separately.
