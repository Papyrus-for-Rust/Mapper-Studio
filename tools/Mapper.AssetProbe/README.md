# Native catalog reader and parity probe

Read-only Rust catalog parser used by Mapper for missing, unpinned installation
catalogs. It uses
the MIT-licensed [`unity-rs-core` 0.6.2](https://github.com/seiunx-dev/unity-rs)
through its low-level source, bundle, serialized-file and type-tree APIs.
The exact dependency and resolved versions are pinned in Cargo files. This
isolated package builds the packaged `tools/mapper-asset-reader.exe` helper.
The parser stays outside the editor process and its dependency tree; its numeric
JSON settings do not alter legacy recipe parsing.

The probe reads the selected installation's `AssetSceneManifest.json`, discovers
all autospawn prefab candidates, and inspects their assigned scene roots. The
reference catalog is used for comparison, **not** as the discovery roster.
It captures bounds/minimum world size, priority/count, rotation ranges, terrain
modifier flags/parameters, texture GUIDs and legacy object references. It also
walks nested hierarchies and captures water bodies, contributors, collider
geometry and both stored/conditionally-active states. Unknown modifier boolean layouts
are reported per root; missing data is not replaced with guessed defaults.

It opens only required scene nodes, processes one scene at a time, and omits
the generic viewer's global reference/texture/sprite indexes. A synchronized
64 KiB pages with a bounded 8 MiB LRU cache amortize small metadata reads. Large stored blocks
stay file-backed; compressed blocks and object materialization have separate
bounds. No game binaries are loaded or executed.

```powershell
cargo build --manifest-path tools/Mapper.AssetProbe/Cargo.toml --release --locked -j1
& tools/Mapper.AssetProbe/target/release/mapper-asset-reader.exe `
    'C:/path/to/Rust' 'C:/path/to/catalog.json' 'C:/new/report.json'
```

Add `--unbuffered` for the same operation without the page adapter. Reports must
be new. `verify_catalog.py REFERENCE PROBE NEW_REPORT` independently compares
captured root fields, using exact integer masks and source-f32 bit comparisons.
Python is used for this development oracle only, not native extraction.

## Owned-installation evidence

On the tested client, manifest-driven native discovery inspects 829 candidates
in 26 scenes and finds all 98 expected roots in about 1.02 seconds. The 1,470
compared field groups match the current catalog, including the 84 supported
terrain modifiers' texture GUIDs and parameters. Eleven previously unsupported
modifier layouts remain explicitly reported.
An additional hidden-console run took 0.92 seconds with a sampled peak working
set of 131,371,008 bytes (about 125 MiB, sampled every 25 ms).

An earlier controlled, reference-targeted workload used 25 scenes and 98 roots:
buffered/unbuffered reads took 0.64/24.68 seconds. An expanded-field version of
that workload took 0.61/24.41 seconds with equal root data. These comparisons
measure the same native parser with different I/O, **not** a complete native
replacement versus the complete Python extractor.

The probe now emits the complete format-5 catalog **metadata payload**, including
nested water geometry and legacy object IDs. The independent validator compares
every field and requires exact f64 geometry results and JSON scalar types; it
does not loosen comparisons for small transform differences. The current
owned-installation payload has zero differences from the existing catalog.

The first hierarchy pass took 16.34 seconds. An 8 MiB LRU cache reduced application
file-read traffic from about 79 GB of repeated cached-page reads to 1.36 GB and
the pass to 10.67 seconds. Direct conversion from typed native fields to values
removed text serialization/parsing inside the traversal; the complete metadata
pass then took 8.92 seconds. All measurements exclude full source hashing and
texture/model decoding. The roughly one-second measurements above describe
the earlier root-only workload and are not full metadata import timings.

The final compact hierarchy pass took 4.53 seconds, with a sampled peak working
set of 141,938,688 bytes (about 135 MiB). Transform/GameObject field payloads
are released after their matrices and IDs are retained; script inspection
keeps only the fields needed for water metadata. Exact complete metadata parity
still passes, including the existing incomplete reports. These are individual
owned-installation runs, not a controlled cross-machine benchmark.

The geometry comparison covers 351,367 hierarchy nodes, 17 water bodies and
14 contributors across 98 roots. It preserves the four existing incomplete
geometry reports. Twenty-five independent synthetic fixtures cover arbitrary
rotation, tilt, shear, inactive roots, disabled components, external/cyclic
references and malformed states. Mesh buffers are not read. The native value
bridge preserves source f32/f64 widths and large integer IDs; integral source
counts are normalized to the catalog contract, while fractional counts require
an adapter instead of being silently truncated.

## Mapper integration, November 10

`--catalog-metadata RUST_ROOT NEW_JSON` discovers metadata without a reference
catalog. Mapper holds its normal verified, read-only source leases throughout
execution, validates the helper's source-code fingerprint and returned source
identity, and validates the complete catalog before atomic publication. Native
metadata is copied into the existing format-5 catalog contract as raw JSON,
preserving exact f64 values, integer masks and boolean types. Existing pinned
catalogs and valid cached catalog/texture pairs remain unchanged. Missing or
unsupported readers use the existing compatibility parser. Cancellation kills
the helper and leaves no accepted partial catalog.

A controlled owned-installation check used the same held source receipts and
fresh private output directories for each reader, with initial hashing excluded
and the OS cache left intact. Native catalog preparation, validation and
publication took **4.102 s**; compatibility preparation took **29.843 s**.
Native ran first. Both found 98 entries. An independent Python comparison found
zero complete metadata differences and identical source receipts and layout
contracts, including names and nested water geometry. This is one ordered
catalog-only comparison; it excludes textures, generation-stage preparation and
complete application startup. Evidence is under `native-catalog-*20261110` in
ignored verification artifacts.

The helper does not hash source files, decode texture/model pixels, execute
generation stages or publish shared caches. Those responsibilities remain in
Mapper. Only the owned current-client sample has been verified; unfamiliar
Unity/game layouts still need tested adapters or compatibility fallback.

## Next steps

1. Verify more owned historical/staging installations and malformed layouts.
2. Port GUID resolution and texture decoding against every existing decoded
   checksum, retaining orientation, channel precision and lossless storage.
3. Port generation-stage metadata, then optional model decoding.
4. Measure the remaining first-time verification and extraction costs separately
   from populated-cache startup; retain update invalidation and version pins.

Run tests and lint without opening Mapper or the game:

```powershell
cargo test --manifest-path tools/Mapper.AssetProbe/Cargo.toml -j 1
cargo clippy --manifest-path tools/Mapper.AssetProbe/Cargo.toml --all-targets -j 1 -- -D warnings
```
