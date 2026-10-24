# PlainWeb Browser Documentation

Welcome to the PlainWeb Browser documentation. This directory contains detailed information about the design, architecture, and implementation of the `pwb-browser`.

## Table of Contents

### Core Architecture
- [Architecture Overview](architecture.md) - High-level design and components

### Feature-Specific Design
- [URI Resolution & `file.pwb`](uri-resolution.md) - Local file access and domain resolution
- [Scrolling & Viewport](scrolling-viewport.md) - Content-bound scrolling and visual scrollbar
- [Text Selection & Clipboard](text-selection.md) - Text selection and copy

### Development
- [Building & Running](development.md) - Setup and usage instructions
- [Contributing](contributing.md) - Guidelines for contributors

## Key Design Principles

1. **Simplicity**: Minimal dependencies, straightforward architecture
2. **Portability**: Software rendering works on any platform
3. **Performance**: Optimized for smooth 60fps rendering
4. **Security**: Sandboxed execution environment
5. **Extensibility**: Modular design for future features

## Getting Started

```sh
# Build the browser
cargo build -p pwb-browser

# Run with a local file
cargo run -p pwb-browser -- --file demo.pwb

# Run with an official built-in page
cargo run -p pwb-browser -- welcome.plain

# Show help
cargo run -p pwb-browser -- --help
```

## License

PlainWeb is open-source software. See the main [README](../README.md) for licensing information.