# Third-party sources for Doom

This directory contains vendored third-party code required by ArrOSt Doom integration.

## Expected layout

- `user/doom/third_party/doomgeneric/`

## Setup

Use the helper script to fetch/populate DoomGeneric sources:

```bash
scripts/vendor_doomgeneric.sh
```

## Notes

- Third-party code is kept separate from ArrOSt kernel sources.
- Kernel core remains Rust-only; C is used only in Doom integration/tooling paths.
