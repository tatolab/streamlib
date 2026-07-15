# Naming

Names must be understood with zero context — someone reading the name 200 lines from its
declaration knows exactly what it is and where its data comes from / goes to. **Never suggest a
shorter name; suggest a more explicit one.** Explicit beats short, always.

- Encode relationship + role + direction: `FromUpstream`, `ToDownstream`, `Input`, `Output`.
- No bare generic words: never just `Writer`, `Reader`, `Handle`, `Manager`, `State`, `ctx`, `buf`.

Validated examples (do not shorten): `LinkOutputDataWriter`, `LinkInputDataReader`,
`LinkInputFromUpstreamProcessor`, `LinkOutputToDownstreamProcessor`,
`add_link_output_data_writer()`, `set_link_output_to_processor_message_writer()`. A 43-character
name is fine.
