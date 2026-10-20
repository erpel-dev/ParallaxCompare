# Configuration and definition fixtures

These are invented examples for comparison tests, shaped after the public Eclipse S-CORE configuration families. They contain no production configuration or third-party source code.

- Communication, lifecycle, logging and parameter JSON have equivalent reordered and changed variants. Unknown arrays, arguments and matrices remain ordered.
- TRLC covers object identities, reordered attributes, Unicode and versioned traceability references. RSL, Franca and Protobuf examples cover layout changes and changed types, IDs or field numbers.
- FlatBuffers covers included schemas, tables, required strings, exact 64-bit integers, floating-point values, enums, structs, fixed arrays, scalar/struct vectors, unions, union vectors, optional scalars and defaults.
- `data-repacked.bin` explicitly stores default values; it differs in bytes from `data-v1.bin` but decodes equally. `data-size-prefixed.bin` is also equivalent. `data-v2.bin` changes values. `data-unknown.bin` adds a populated field that the original schema must reject.

Regenerate with `python3 regenerate.py` with `flatc` 24.3.25 on PATH. Committed `.bfbs` and `.bin` fixtures let decoding tests run without an installed compiler; the text-schema compilation test also checks the actionable missing-tool error when `flatc` is unavailable.
