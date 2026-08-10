# Spotlit Lock Screen

The optional GNOME Shell 50 extension gives Spotlit an independent lock screen wallpaper, configurable background blur, and an opt-in display policy on Ubuntu 26.04. Clear mode adds translucent surfaces behind the clock and authentication controls so they remain readable over detailed wallpapers. The display policy can preserve GNOME's default power behavior, keep the lock screen visible while plugged in, or keep it visible on all power sources.

Build the local extension bundle without installing it:

```sh
bash extensions/gnome-shell/package.sh
```

On Linux, Spotlit exposes the extension state, blur controls, and display policy under **Settings > Wallpaper > GNOME Lock Screen**. Opening Spotlit or the settings page only queries the current state. Installing, enabling, disabling, or changing an extension preference requires an explicit action in that section.

Install and enable it explicitly for the current user:

```sh
gnome-extensions install --force target/gnome-extension/lock-screen@spotlit.app.shell-extension.zip
gnome-extensions enable lock-screen@spotlit.app
```

Open its appearance and display preferences with:

```sh
gnome-extensions prefs lock-screen@spotlit.app
```

The build and Rust test commands do not install or enable the extension. Installation changes only the current user's GNOME extension configuration.

The extension source is licensed under Apache-2.0.
