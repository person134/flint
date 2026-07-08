# flint

A simple GUI tool for flashing ISO files to USB drives using dd.

## Usage

Run with sudo or as root:

```
sudo flint
```

Select an ISO file, pick a USB device from the list, and click Start flashing.

## Build from source

```
cargo build --release
```

The binary will be at `target/release/flint`.

## License

MIT
