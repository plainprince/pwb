# PlainWeb Browser Architecture

This documentation covers the architectural choices and design patterns in the PlainWeb Browser (`pwb-browser`).

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [URI Resolution](#uri-resolution)
3. [Rendering Pipeline](#rendering-pipeline)
4. [Security Model](#security-model)
5. [Performance](#performance)

---

## Architecture Overview

The PlainWeb Browser is a **native desktop application** that renders PlainWeb bundles (`.pwb` files) using a **software-rasterized 2D canvas** powered by Skia via `tiny-skia`. It follows a **pipeline architecture**:

```
[.pwb Bundle] → [DOM Parser] → [Layout Engine] → [Skia Renderer] → [Desktop Window]
```

### Key Components

| Component | Responsibility | Implementation |
|-----------|----------------|-----------------|
| **DOM Parser** | Decompiles `.pwb` binary bundles | Custom binary format parser |
| **Layout Engine** | Computes element positions | Viewport-relative scaling |
| **Skia Renderer** | Renders graphics and text | `tiny-skia` + `ab_glyph` |
| **QuickJS Engine** | Executes PlainJS scripts | QuickJS-NG with DOM bindings |
| **Event System** | Handles user interactions | Winit event loop |

### Design Principles

1. **Simplicity**: Minimal dependencies, straightforward architecture
2. **Portability**: Software rendering works on any platform
3. **Performance**: Optimized for smooth 60fps rendering
4. **Security**: Sandboxed execution environment
5. **Extensibility**: Modular design for future features

---

## URI Resolution

The browser implements a **two-tier URI resolution system** supporting both network domains and local files.

### URI Types

| URI Type | Example | Resolution Method | Security Model |
|----------|---------|-------------------|-----------------|
| **Unofficial Network Domain** | `mysite.pwb` | DomainRegistry lookup | Registered domains only |
| **Official Built-in Page** | `welcome.plain`, `search.plain` | Built-in handler / shipped bundle | Reserved TLD |
| **Local File** | `file.pwb/demo.pwb` | Filesystem access | Sandboxed |

### Local File Access

**Design Decision**: Special `file.pwb/` pseudo-domain for local file access.

- **Path Format**: `file.pwb/path/to/file.pwb`
- **Command Line**: `pwb-browser --file path/to/file.pwb`
- **Security**: Read-only, path-restricted access

This enables:
- Development and testing of `.pwb` bundles
- Offline access to local content
- Integration with local tooling

---

## Rendering Pipeline

### Theme-Scaled Pixel Sizing

**Design Decision**: Default sizes are pixels from the active theme
(`src/theme.rs`), scaled gently by both window dimensions so controls do not
become tiny on short windows or huge on tall ones.

```rust
let scale = ((width / 1200.0) * (height / 800.0)).sqrt().clamp(0.9, 1.15);
```

- **Base page font**: the theme's `sizes.page-font` (18px default)
- **Chrome**: its own clamp (0.92–1.2) for tabs, toolbar, and icons
- **Explicit PSS**: `px`, `%`, `vw`, and `vh` still behave as written

### Text Measurement & Layout

**Design Decision**: Dynamic layout based on text measurement.

1. Measure text width before rendering:
   ```rust
   let text_w = measure_text_width(txt, font_size, font_opt).min(max_width);
   ```

2. Compute element sizes from content:
   ```rust
   let btn_width = (text_w + (padding_h * 2.0)).min(max_width);
   ```

3. Handle text overflow with clipping

Benefits:
- Optimal screen space utilization
- Consistent padding/margins
- Readable text at all sizes
- Responsive design without CSS

---

## Security Model

### Sandboxed File Access

**Design Decision**: Strict sandboxing for local file access.

- **Path restriction**: Only `file.pwb/` paths allowed
- **Read-only**: No file writing capabilities
- **Explicit opt-in**: Requires `--file` flag or prefix
- **No traversal**: `../` sequences blocked by path parsing

### Reserved official TLD

**Design Decision**: `.plain` TLD strictly reserved for the official namespace — built-in system domains and browser-shipped content pages. `.pwb` is the unofficial user namespace.

```rust
pub fn is_reserved_system_domain(domain: &str) -> bool {
    domain.trim().to_lowercase().ends_with(".plain")
}
```

Prevents:
- User registration of system domains
- Phishing via similar domain names
- Unauthorized system access

### Content Isolation

Each `.pwb` bundle executes in isolation:
- Separate QuickJS runtime per bundle
- No shared state between domains
- Read-only DOM access from scripts
- No network access from PlainJS

---

## Performance

### Software Rasterization

**Design Decision**: Software rendering via `tiny-skia`.

Advantages:
- **Portability**: Works on any platform
- **Consistency**: Pixel-perfect rendering
- **Simplicity**: No GPU driver dependencies
- **Fallback**: Graceful degradation

### Efficient Text Rendering

Optimizations:
- Glyph caching in `ab_glyph`
- Subpixel positioning
- Alpha blending for anti-aliasing
- Viewport-relative scaling

### Layout Performance

- Compute positions once per frame
- Cache bounding boxes for hit testing
- Minimize reflow with stable DOM updates
- Simple box model for fast computation

---

*PlainWeb Browser v0.0.0*