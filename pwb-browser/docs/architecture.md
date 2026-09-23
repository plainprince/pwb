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
| **Network Domain** | `welcome.pwb` | DomainRegistry lookup | Registered domains only |
| **System Domain** | `search.plain` | Built-in handler | Reserved TLD |
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

### Viewport-Relative Scaling

**Design Decision**: Dynamic viewport scaling ensures consistent rendering across window sizes.

```rust
let viewport_scale = (viewport_height as f32 / 600.0).clamp(0.75, 2.5);
```

- **Base font size**: 18px at 600px viewport height
- **Scaling range**: 0.75x to 2.5x
- **Responsive**: Adapts to window resizing

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

### Reserved System TLD

**Design Decision**: `.plain` TLD strictly reserved for system domains.

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