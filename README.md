# Contr for Linux

GTK4 and libadwaita desktop client for [Contr](https://github.com/contr-hq/contr) on Fedora and Ubuntu. It talks to the
`contrd` daemon only through the `contr-sdk` crate, over the daemon's per-user Unix socket.

## Status

The application shell (window, sidebar navigation, engine card and gate, primary menu, tray icon) and the Containers
section with each container's logs, inspection, terminal and stats, the Run dialog, Images with pulls, Volumes,
Networks, Compose, Kubernetes, Troubleshoot, Preferences and first-run onboarding.

## Stack

- Rust, relm4 with libadwaita (GNOME 49 or later: GTK 4.20, libadwaita 1.8), vte4 for the exec terminal, ksni for the
  tray.
- Modules mirror the Windows client; a module reaches another only through its `-api` crate, checked by
  `cargo xtask archlint`. See [docs/architecture.md](docs/architecture.md).
- Packages: `contr` as a .deb (Ubuntu 26.04) and .rpm (Fedora 44), depending on the daemon's `contr-daemon`
  package; see [docs/development.md](docs/development.md#packages). A beta Flatpak talks to the same host daemon.

## Building

Install the GTK, libadwaita and VTE development packages, then build with Cargo. See
[docs/development.md](docs/development.md) for running the client against a fake or real daemon.

```sh
# Fedora
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel vte291-gtk4-devel
# Ubuntu
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev

cargo run -p contr-desktop
```

## License

Apache-2.0. Docker is a trademark of Docker, Inc.; Contr is not affiliated with or endorsed by Docker, Inc.
