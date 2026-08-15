# Flow view vendor assets

These are pre-built UMD bundles of React, ReactDOM, and @xyflow/react,
served locally by `ti serve` so the lifecycle view has no CDN
dependencies.

## Files

| File | Source | License |
|------|--------|---------|
| `react.min.js` | `react@18.3.1` UMD production | MIT |
| `react-dom.min.js` | `react-dom@18.3.1` UMD production | MIT |
| `xyflow.min.js` | `@xyflow/react@12.11.3` UMD | MIT |
| `xyflow.css` | `@xyflow/react@12.11.3` dist/style.css | MIT |
| `jsx-runtime-shim.js` | Hand-written shim (see below) | MIT |

The `jsx-runtime-shim.js` provides the `jsxRuntime` global that the
@xyflow/react UMD bundle expects. It is a minimal reimplementation of
`react/cjs/react-jsx-runtime.production.min.js` — just `jsx`/`jsxs`
wrappers around `React.createElement`.

## Re-fetching

To update these bundles:

```sh
mkdir -p /tmp/xyflow-pack && cd /tmp/xyflow-pack
npm pack @xyflow/react@12 react@18.3.1 react-dom@18.3.1
tar xzf xyflow-react-*.tgz && tar xzf react-18.3.1.tgz
# react-dom is inside the same package/ dir after extraction
cp package/umd/react.production.min.js <repo>/crates/ticgit/vendor/flow/react.min.js
cp package/umd/react-dom.production.min.js <repo>/crates/ticgit/vendor/flow/react-dom.min.js
cp package/dist/umd/index.js <repo>/crates/ticgit/vendor/flow/xyflow.min.js
cp package/dist/style.css <repo>/crates/ticgit/vendor/flow/xyflow.css
```
