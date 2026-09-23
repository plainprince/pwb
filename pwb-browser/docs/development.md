# Building & Running the PlainWeb Browser

## Prerequisites

- Rust 1.70+ (via [rustup](https://rustup.rs/))
- Cargo package manager
- Git (for cloning the repository)

## Building

```sh
# Clone the repository
git clone https://github.com/plainweb/pwb.git
cd pwb

# Build the browser (debug mode)
cargo build -p pwb-browser

# Build with optimizations
cargo build -p pwb-browser --release
```

## Running

### Basic Usage

```sh
# Run with a local .pwb file
cargo run -p pwb-browser -- --file path/to/bundle.pwb

# Run with a network domain
cargo run -p pwb-browser -- welcome.pwb

# Run with a system domain
cargo run -p pwb-browser -- search.plain?q=plainweb
```

### Command Line Options

| Option | Description | Example |
|--------|-------------|---------|
| `--file <path>` or `-f <path>` | Open a local .pwb file | `--file demo.pwb` |
| `--help` or `-h` | Show help message | `--help` |

### Examples

```sh
# Open the included demo bundle
cargo run -p pwb-browser -- --file demo.pwb

# Open a network domain
cargo run -p pwb-browser -- welcome.pwb

# Use the search system domain
cargo run -p pwb-browser -- search.plain?q=plainweb

# Show help
cargo run -p pwb-browser -- --help
```

## Development

### Running Tests

```sh
cargo test -p pwb-browser
```

### Debugging

The browser includes built-in developer tools:

- Press `F12` or `Cmd+Shift+I` to open devtools
- Use `Cmd+Shift+J` for quick console access

## Troubleshooting

### Common Issues

1. **File not found**:
   - Ensure the path is correct
   - Use absolute paths if needed: `--file /full/path/to/file.pwb`

2. **Domain not found**:
   - Verify the domain is registered
   - Check network connectivity

3. **Rendering issues**:
   - Try resizing the window
   - Check console for errors

### Getting Help

- Check the [main README](../README.md)
- Read the [architecture documentation](architecture.md)
- Open an issue on GitHub