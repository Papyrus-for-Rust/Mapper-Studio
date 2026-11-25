# Mapper plugins

The beta supports local executable plugins. Open **Window → Plugins**, add a plugin's JSON manifest, choose an action, then run it. Only install plugins you trust. They run as separate processes with your account's permissions.

Start with the [Rust SDK example](../src/mapper-plugin-sdk/examples/) and build its package with `tools/Build-PluginSdkExample.ps1`. The example includes a manifest and an executable hash. This is also a reference for implementing the same protocol in another language.

API 1 supports two capabilities:

- `recipe.transform` returns a replacement recipe. Mapper validates it before applying the change as one Undo step.
- `recipe.inspect` returns a summary and findings without editing the recipe.

Mapper starts the executable for one call. Read a JSON request from stdin through EOF, write one JSON response to stdout, and exit. Write logs to stderr. Echo the incoming protocol and request ID. See the [SDK types](../src/mapper-plugin-sdk/src/lib.rs) for the exact request and response fields.
