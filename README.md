# Spotlit

Spotlit is a cross-platform desktop wallpaper companion built with Rust and Slint. It works with Windows Spotlight on Windows 11 and Bing wallpapers on GNOME-based Linux desktops.

## Features

- Maintains a local wallpaper library with favorites, history limits, and cached previews.
- Applies wallpapers manually or on a configurable schedule.
- Uses Windows Spotlight metadata when it is available.
- Retrieves recent Bing wallpapers on Linux without changing desktop settings at startup.
- Provides an optional GNOME Shell extension for an independent lock screen wallpaper, blur styling, and lock screen display policy.
- Supports English, Simplified Chinese, and German interfaces while preserving wallpaper-provided titles and descriptions.
- Checks GitHub Releases for Spotlit updates when automatic checks are enabled.

## Development

The repository is a single Cargo package. The pinned Rust toolchain is declared in `rust-toolchain.toml`.

```sh
cargo run
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

Starting Spotlit does not install the GNOME extension or write extension preferences. Wallpaper application, startup registration, extension installation, and extension preference changes require explicit user actions.

## GNOME Extension

The optional GNOME Shell extension is maintained with the application source under [`extensions/gnome-shell`](extensions/gnome-shell/README.md). Its packaging command only validates and creates a local bundle:

```sh
bash extensions/gnome-shell/package.sh
```

It does not install, enable, or configure the extension.

## Release

Release tags have no `v` prefix and must exactly match the package version in `Cargo.toml`. For example, package version `0.1.0` must use tag `0.1.0`:

```sh
git tag -a 0.1.0 -m "Release 0.1.0"
git push origin 0.1.0
```

Pushing the tag builds an Ubuntu/Debian `.deb` package, a portable Linux archive, and a Windows archive. Each distribution contains the generated third-party license report. The workflow generates `SHA256SUMS`, records GitHub artifact attestations, and creates a draft GitHub Release. It does not publish the Release.

A maintainer must review the draft on the repository's **Releases** page and click **Publish release**. Crates.io publishing is intentionally outside GitHub Actions.

After publication, verify a downloaded asset with `sha256sum --check SHA256SUMS` and `gh attestation verify <asset> --repo oksyd/spotlit`.

## License

Spotlit is licensed under the [Apache License 2.0](LICENSE).
