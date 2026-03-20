# Agent Guidelines

## Coding Conventions

### Prefer scoping over explicit `drop`

When a mutex guard (or other RAII resource) needs to be released before the end
of a function, prefer a bare scope block with implicit drop over calling
`drop()` explicitly. Extract needed values from the scope via a tuple or let
binding.

```rust
// Preferred: implicit drop via scope
let (value_a, value_b) = {
    let guard = mutex.lock().await;
    let a = guard.some_field.clone();
    let b = guard.other_field;
    (a, b)
};

// Avoid: explicit drop
let guard = mutex.lock().await;
let value_a = guard.some_field.clone();
let value_b = guard.other_field;
drop(guard);
```
