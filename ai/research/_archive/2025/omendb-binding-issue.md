# OmenDB Native Binding Resolution Issue

## Summary

OmenDB v0.0.15 fails to load native bindings on macOS ARM64 due to package name mismatch.

## Environment

- OmenDB: 0.0.15
- Platform: macOS ARM64 (darwin-arm64)
- Runtime: Bun 1.3.5

## Error

```
error: Cannot find module '@omendb/darwin-arm64' from '/path/to/node_modules/@omendb/omendb/index.js'
```

## Root Cause

The native binding package is installed as `@omendb/omendb-darwin-arm64` but the loader in `@omendb/omendb/index.js` looks for `@omendb/darwin-arm64`:

```javascript
// node_modules/@omendb/omendb/index.js line 42
nativeBinding = require("@omendb/darwin-arm64");
```

But npm installs:

```
node_modules/@omendb/
├── omendb/
└── omendb-darwin-arm64/   # <-- has "omendb-" prefix
```

## Workaround

Create a symlink to bridge the name mismatch:

```bash
cd node_modules/@omendb
ln -sf omendb-darwin-arm64 darwin-arm64
```

## Suggested Fix

In `@omendb/omendb/index.js`, update the require path to match the actual package name:

```javascript
// Before
nativeBinding = require("@omendb/darwin-arm64");

// After
nativeBinding = require("@omendb/omendb-darwin-arm64");
```

This applies to all platform-specific packages:

- `@omendb/darwin-arm64` → `@omendb/omendb-darwin-arm64`
- `@omendb/darwin-x64` → `@omendb/omendb-darwin-x64`
- `@omendb/linux-x64-gnu` → `@omendb/omendb-linux-x64-gnu`
- etc.

## Affected Lines

In `node_modules/@omendb/omendb/index.js`:

- Line 39: `@omendb/darwin-x64`
- Line 42: `@omendb/darwin-arm64`
- (likely other platforms as well)

## Reproduction

```bash
mkdir test-omendb && cd test-omendb
bun init -y
bun add omendb
echo 'import { open } from "omendb"; console.log(open);' > index.ts
bun index.ts  # Error: Cannot find module '@omendb/darwin-arm64'
```
