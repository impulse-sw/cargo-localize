# cargo-localize

This command is a `cargo vendor` analogue with some differencies:

- directly edits Cargo.toml to specify dependencies, while creating backup copies of original files (`Cargo.toml.bak`)
- removes project's Cargo.lock file

> [!NOTE]
> Sometimes it can be done a little bit wrong, for example, confuse `rand-0.8.5` and `rand-0.9.1` and their `rand_core` subdependencies. But this is fixable.

> [!WARNING]
> It's recommended to use `cargo vendor` instead. DO NOT use `cargo-localize` in production!

## Build and installation

```bash
cargo install --path .
```

Or:

```bash
deployer run build
```

## Usage

```
Usage: cargo-localize [OPTIONS] [PROJECT_PATH]

Arguments:
  [PROJECT_PATH]  [default: .]

Options:
      --third-party-dir <THIRD_PARTY_DIR>  [default: 3rd-party]
  -h, --help                               Print help
```
