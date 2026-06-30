# Fallback: if the chunk loop still throws E0502 on `rows`

The current code binds `let rows = &self.rows[..self.n_rows];` before the
control-chunk loop. If the borrow checker rejects it (because it can't
prove `rows` is disjoint from the `&mut self.gmod` / `&mut self.scratch_*`
/ `&mut self.vm` accesses inside the loop), sever the borrow by copying the
active rows onto the stack.

Replace:

```rust
        const CTRL_CHUNK: usize = 64;
        let rows = &self.rows[..self.n_rows];
        let mut off = 0usize;
```

with:

```rust
        const CTRL_CHUNK: usize = 64;
        // Stack copy severs the borrow on self.rows so the loop can take
        // &mut self.gmod / self.scratch_* / self.vm freely. DecodedRow is
        // Copy and the array is 32 * a few words — trivially cheap.
        let rows_buf = self.rows;
        let rows = &rows_buf[..self.n_rows];
        let mut off = 0usize;
```

`DecodedRow` already derives `Copy` (it's a small POD struct of indices and
amounts), and `[DecodedRow; 32]` is `Copy`, so `let rows_buf = self.rows;`
is a plain stack copy — no heap, no allocation, audio-thread safe. This is
guaranteed to compile.
