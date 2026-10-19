# Contr for Linux

GTK4 and libadwaita desktop client for [Contr](https://github.com/contr-hq/contr) on Fedora and Ubuntu. It talks to the
`contrd` daemon only through the generated `contr-sdk` crate, over the daemon's per-user Unix socket.

## Status

Not started. Development begins in core Phase 4.

## Planned stack

- Rust, relm4 with libadwaita, gtk4-rs, vte4 for the exec terminal, ksni for the tray.
- Modules mirror the Windows client; a module reaches another only through its `-api` crate, checked by
  `cargo xtask archlint`.
- Packages: RPM (Fedora), DEB (Ubuntu), Flatpak. The daemon ships separately as `contr-daemon`.

## License

Apache-2.0. Docker is a trademark of Docker, Inc.; Contr is not affiliated with or endorsed by Docker, Inc.
