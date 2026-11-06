# OMM (Open Math Model)

[Читать на русском](README.md)

**OMM** is an ultra-light 3D model format and a standalone engine written in **Rust**, compiled to **WebAssembly** and rendered with **WebGL2**. It brings 3D graphics to web projects with no third-party dependencies.

> **v4.0** — the engine was rewritten from scratch: **parser → bytecode → virtual machine → viewer (WebGL2)**. The `.omm` format and syntax, the WASM/JS API and the `<omm-model>` tag are unchanged: existing models load as before and the previous JS wrapper is API-compatible (caveats in [What changed](#what-changed-since-3x)).

## 📦 Installation

### Local
Build the project (see [Building](#-building-and-testing)) or download the `pkg` artifact from GitHub Actions and put these files into one folder:
- `omm-component.js`
- `omm_core.js`
- `omm_core_bg.wasm`

```html
<script type="module" src="./omm-component.js"></script>
```

> ⚠️ A local HTTP server is required (Live Server, `python -m http.server`, …) — browsers block WASM/modules on `file://`.

### CDN
If you publish the `pkg/` folder (e.g. commit it), it can be served through jsDelivr — `omm_core.js` and `omm_core_bg.wasm` are fetched automatically from the same folder:

```html
<script type="module"
  src="https://cdn.jsdelivr.net/gh/NelikKKL/OMM-model@v4.0.0/pkg/omm-component.js">
</script>
```

> The repository workflow **only builds**: it uploads `pkg/` as an artifact and commits or publishes nothing.

## 🚀 Quick start

```html
<!-- Auto-rotation -->
<omm-model src="model.omm" autorate></omm-model>

<!-- Full control (mouse + touch + zoom) -->
<omm-model src="model.omm" freer></omm-model>

<!-- Model inside the tag -->
<omm-model autorate>
  cube3
  color(80, 140, 255)
  scale(2)
</omm-model>
```

### `<omm-model>` attributes

| Attribute | Description |
| :--- | :--- |
| `src` | Path to an `.omm`, `.glb` or `.obj` file. |
| `autorate` | Endless auto-rotation. |
| `freer` | Mouse/touch rotation + pinch zoom + panning (right / middle button). |

Events: `omm-load` (model parsed), `omm-error` (error; the text is also shown over the canvas). `el.engine` exposes the `OmmEngine`.

---

## 📝 `.omm` syntax

An `.omm` file is a plain text document. Every shape starts with a keyword. Lines starting with `//` are comments.

### 1. Geometry

| Keyword | Description |
| :--- | :--- |
| `cube3` | Regular cube. |
| `cube3 : 4` | Slab (height divided by 4). |
| `pyramid3` | Pyramid. |
| `sphere3` | Sphere. |
| `cylinder3` | Cylinder. |
| `triangle3` | Flat triangle. |
| `image3` | Textured plane (drawn once its texture is loaded). |
| `mesh3` | Arbitrary geometry — see below. |

### 1.1. Arbitrary geometry `mesh3`

`mesh3` is a container for any triangle geometry — imported from `.glb`/`.obj` or written by hand. Lighting is computed from each face normal (flat shading), not from the face index like primitives.

**By hand:**

```
mesh3
verts(0 -50 0, 50 50 0, -50 50 0, 0 -50 100)
tris(0 1 2, 0 3 1, 0 2 3, 1 3 2)
color(220, 120, 60)
```

- `verts(x y z, ...)` — vertices (commas and/or spaces).
- `tris(i j k, ...)` — triangles (zero-based vertex indices).
- `uvs(u v, ...)` — optional UVs (0–1), one pair per vertex.

Lists may span several lines and contain `//` comments.

**Automatically** — converters pack geometry into compact base64 inside `data(...)`:

```
mesh3
data(AAAAAA...)
color(200, 200, 200)
texture(data:image/png;base64,...)
```

`data(...)` is binary and not meant for manual editing. All usual transforms (`x/y/z`, `scale`, `rr/ru`, `animation`, `mono()`) apply to `mesh3`, except asymmetric deformation (`ur/ul/...`).

Broken geometry does not break the scene: triangles referring to missing vertices are dropped, and UVs whose count does not match the vertex count are ignored (a warning is printed).

### 1.2. Importing `.glb` and `.obj`

Just give `src` the right extension — conversion to `mesh3` happens in the browser:

```html
<omm-model src="model.glb" autorate></omm-model>
<omm-model src="model.obj" freer></omm-model>
```

| Format | What is imported | Limitations |
| :--- | :--- | :--- |
| `.glb` (binary glTF) | Whole node hierarchy (transforms baked into vertices), `baseColorFactor`, embedded PNG/JPEG textures | Self-contained `.glb` only; `TRIANGLES` mode only; no skeletal animation or morph targets |
| `.obj` (Wavefront) | Vertices, UVs, triangles/n-gons (fan-triangulated) | `.mtl` is not parsed — default color is gray; add `color()`/`texture()` by hand |

`.glb` parts sharing a material (color + texture) are merged into one `mesh3`, so an embedded image is written once.

To convert once and save a regular `.omm`:

```js
import init, { obj_to_omm, glb_to_omm } from './omm_core.js';
await init();

const ommText = obj_to_omm(await (await fetch('chair.obj')).text());
const bytes = new Uint8Array(await (await fetch('chair.glb')).arrayBuffer());
const ommText2 = glb_to_omm(bytes);
```

Both functions throw (with a descriptive string) when the file is damaged or unsupported.

### 2. Transforms

```
x(n)     y(n)     z(n)     — position (y points up, negative values raise)
scale(n)                    — scale (50 pixels per face by default)
rr(deg)                     — rotate right (Y axis)
ru(deg)                     — rotate up    (X axis)
```

Several properties may share a line, including the line with the shape name: `sphere3 x(40) color(255,0,0)`.

### 3. Animation

```
animation(x0 y0 z0, x0 y200 z0)   — comma-separated keyframes
animation(y200, y0)                — only the axes you need
```

Keyframes support `x`, `y`, `z`, `rr`, `ru`.

### 4. Asymmetric deformation

Stretching individual sides (all primitives except `mesh3`):

```
ur(n)  ul(n)   — right / left (X axis)
uu(n)  ud(n)   — up    / down (Y axis)
ug(n)  um(n)   — front / back (Z axis)
```

### 5. Color and textures

```
color(R, G, B)                         — fill color, 0–255
texture(https://site.com/img.png)      — external link
texture(data:image/png;base64,iVBOR…)  — embedded Base64
```

The texture covers every face of the shape. Until the image has loaded the shape is drawn in its own color.

> ⚠️ **WebGL requires CORS**: an image from another origin must be served with `Access-Control-Allow-Origin` (GitHub raw, jsDelivr, imgur and most CDNs do). Embedded `data:` textures and same-origin images are fine. If a texture cannot be uploaded the shape stays single-colored and the reason appears in `engine.warnings()` and the console.

### 6. Grouping `mono()`

Combines several shapes under a common transform:

```
mono(
  x(0) y(0) scale(1)   ← group transform (optional)

  cube3
  color(150, 150, 150)
  y(200)

  sphere3
  color(200, 100, 50)
)
```

The group transform (`x y z scale rr ru`) is applied once, at load time, to every shape in the block; `animation()` belongs to a particular shape. `mono()` blocks can be nested.

---

## 🏗 Architecture

```
 .omm text ──▶ parser ──▶ bytecode ──▶ vm ──▶ plain data ──▶ viewer ──▶ WebGL2
                 │           │          │                       │
        syntax only     the contract  builds the world:   knows nothing about the
        (builds nothing) (bytecode)   objects, groups,    format, bytecode or VM:
                                      meshes, animation   draws triangles and matrices
```

| Module | Role |
| :--- | :--- |
| `parser` | Reads text and **only emits instructions**. Creates no objects or geometry, applies no group transforms. Never panics; anything suspicious goes to `warnings` (with line numbers). |
| `bytecode` | The instruction set and the `OMMB` container (`Program::to_bytes` / `from_bytes`) — a compiled scene can be cached (`compile()` → `OmmEngine.load_bytecode()`). |
| `vm` | Executes bytecode: creates objects, applies properties, resolves `mono()`, builds meshes, advances animation. Any bytecode (truncated, unknown opcodes, bad indices) yields warnings, never a panic. |
| `viewer` | Takes triangle meshes, instances (matrix + color + texture) and a camera. Opaque instances use the depth buffer; textured instances (possibly transparent) go into a second pass, far to near. |
| `camera` | Camera state, controls, and the view-projection matrix reproducing the 3.x projection exactly. |
| `convert` | `.obj` / `.glb` → `.omm` text. |

Everything except the thin `web` layer and the WebGL backend has **zero dependencies** and is tested with plain `cargo test`. The project code contains no `unsafe` (`#![deny(unsafe_code)]`; the only exception is code generated by `wasm-bindgen` itself).

### What changed since 3.x

| | 3.x | 4.0 |
| :--- | :--- | :--- |
| Renderer | Canvas 2D, painter's algorithm | WebGL2 with a depth buffer — intersecting objects render correctly |
| Geometry | rebuilt every frame | shape tables built once and shared; mesh lighting precomputed at load |
| Face outlines | `stroke` per face | drawn in the shader (≈1 px, 0.15 alpha); `set_outline(0)` disables |
| Parser | rewrote the text, then parsed line by line | one pass → bytecode; multi-line lists; nested `mono()` |
| Behind the camera | drawn mirrored | clipped correctly |
| Panics/hangs | possible (glTF cycles, huge sizes, `NaN`) | ruled out: bounds checks, limits, cycle-safe traversal |
| Hi-DPI | — | `set_pixel_ratio()`: crisper canvas, same model size |

Compatibility: the `OmmEngine` API and `<omm-model>` are unchanged. Worth knowing when upgrading:

- **WebGL2** is required (all current browsers). Without it `setup()` throws and the component shows a message.
- External textures must be served with CORS (see above).
- Properties written *after* the closing `)` of a `mono()` block no longer attach to its last shape; they are ignored with a warning.
- Asymmetric deformation (`ur/ul/…`) works on `cube3` too (the 3.x docs said otherwise although the code did deform cubes).

## 🔌 `OmmEngine` API

| Method | Description |
| :--- | :--- |
| `new OmmEngine()`, `setup(canvas)`, `resize(w, h)` | Create, attach a canvas (WebGL2), set canvas size. |
| `parse(text)` | Replaces the scene. The camera is kept. |
| `pending_textures()`, `set_texture(src, img)` | Images the scene needs; hand over a loaded image. |
| `step()`, `auto_rotate()`, `render()` | Animation tick, auto-rotation step, draw a frame. |
| `drag(dx, dy, mode)`, `zoom(delta)`, `pinch_zoom(z0, d0, d)` | Camera controls (`mode` 0 rotates, otherwise pans). |
| `has_animation()`, `focal()`, `camera_z()` | State. |
| `obj_to_omm(text)`, `glb_to_omm(bytes)` | Converters (module functions). |
| **new:** `warnings()` | Notes about the last scene (typos, bad data) with line numbers. |
| **new:** `set_outline(a)`, `set_pixel_ratio(r)` | Outline opacity (0 disables); physical-to-logical pixel ratio of the canvas. |
| **new:** `compile(text)`, `load_bytecode(bytes)` | Compile to bytecode and load a compiled scene. |

---

## 🔧 Building and testing

Requirements: [Rust](https://rustup.rs/) + [wasm-pack](https://rustwasm.github.io/wasm-pack/).

```bash
wasm-pack build --release --target web --out-dir pkg
cp js/omm-component.js pkg/
```

Checks:

```bash
cargo test                          # parser, bytecode, VM, camera, converters, end-to-end
python3 tools/check_shaders.py      # shaders and GL call order in a real WebGL2 (headless Chromium)
python3 tools/check_component/run.py  # JS component: input, textures, DPR, errors (mocked core)
```

The last two need `pip install playwright && playwright install chromium`.

---

## History 📜

v4.0.0: Rewritten from scratch: parser → bytecode → VM → viewer; WebGL2 instead of Canvas 2D; safe handling of any input

v3.0.0: mesh3 support, glb and obj models

v2.0.5: Migration to Rust

v2.0.0: Migration to TypeScript

v1.0.0: (commits only), 2.0.0 in plain js
