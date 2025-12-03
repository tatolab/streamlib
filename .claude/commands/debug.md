# Debug StreamLib Runtime State

Help the user debug their StreamLib runtime by examining the current graph state.

## What to do:

1. **Find the runtime** - Locate where `StreamRuntime` is created or accessible in the user's code (usually in `main.rs` or a test file)

2. **Add debug output** - Help the user add temporary code to inspect state:
   ```rust
   // Add this where you want to inspect runtime state:
   {
       let pg = runtime.property_graph().read();
       eprintln!("=== Runtime State ===");
       eprintln!("{}", serde_json::to_string_pretty(&pg.to_json()).unwrap());
   }
   ```

3. **Analyze the output** - Look for common issues:

   | Issue | What to look for |
   |-------|------------------|
   | Processor not running | `"state": "Idle"` when should be `"Running"` |
   | Buffer backpressure | `"buffer": { "fill_level": N }` where N equals capacity |
   | Empty buffers | `"fill_level": 0` with `"is_empty": true` - producer too slow |
   | Frame drops | `"metrics": { "frames_dropped": N }` where N > 0 |
   | High latency | `"latency_p99_ms"` significantly higher than `"latency_p50_ms"` |
   | Missing connections | Links missing from expected processors |
   | Wrong link state | `"state": "Pending"` instead of `"Wired"` |

4. **Generate DOT visualization** if the graph is complex:
   ```rust
   eprintln!("=== DOT Graph ===\n{}", pg.to_dot());
   ```
   User can paste this into https://dreampuf.github.io/GraphvizOnline/ to visualize.

5. **Report findings** - Summarize what's wrong and suggest fixes.
