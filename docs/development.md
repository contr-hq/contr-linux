# Development

## Building

The workspace builds with the toolchain in `rust-toolchain.toml`. It needs the GTK 4, libadwaita and VTE (GTK 4)
development packages; see the README. Without root, the VTE package can be unpacked into a private prefix:

```sh
apt-get download libvte-2.91-gtk4-dev
dpkg-deb -x libvte-2.91-gtk4-dev_*.deb ~/.cache/contr-dev/vte-dev
```

then point `PKG_CONFIG_PATH` at its `pkgconfig` directory, after fixing `libdir` in the `.pc` file and the
`libvte-2.91-gtk4.so` link to the system library.

## Running against a daemon

Build `contrd` from a checkout of contr-hq/contr and run it on its own socket:

```sh
contrd --backend fake --listen unix://$XDG_RUNTIME_DIR/contr-linux-fake.sock --no-docker-context
CONTR_HOST=unix://$XDG_RUNTIME_DIR/contr-linux-fake.sock cargo run -p contr-desktop
```

The fake backend serves fixture containers, images and a Compose project. For a real engine, run
`contrd --backend host` with `DOCKER_CONFIG` pointing at a configuration whose `cli-plugins` come from Docker Engine.
With `CONTR_HOST` set, the client never starts a daemon itself.

## Screenshots

GTK's Broadway backend draws the window in a browser, which makes screenshots reproducible:

```sh
gtk4-broadwayd --port 8085 :5 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 ADW_DEBUG_COLOR_SCHEME=prefer-light ADW_DEBUG_ACCENT_COLOR=blue \
  CONTR_HOST=unix://$XDG_RUNTIME_DIR/contr-linux-fake.sock cargo run -p contr-desktop
```

and open `http://127.0.0.1:8085`. Distribution defaults such as Ubuntu's Yaru theme still apply; a keyfile GSettings
backend (`GSETTINGS_BACKEND=keyfile` with `gtk-theme='Adwaita'`) gives the stock look the design uses.

## Packages

The `contr` .deb and .rpm hold `contr-desktop`, its desktop entry, AppStream metainfo and icon, and depend on the
`contr-daemon` package from contr-hq/contr. Each links against its own distribution's GTK, so the binary for the .deb
is built on Ubuntu 26.04 and the one for the .rpm on Fedora 44 (`.github/workflows/release.yml` does both). With the
two binaries and the crate notices from cargo-about:

```sh
cargo about generate --config packaging/licenses/about.toml --output-file target/notices/rust-crates.txt \
  packaging/licenses/about.hbs
cargo xtask package-linux --deb-binary PATH --rpm-binary PATH --version 0.1.0
```

nfpm runs in `packaging/linux/package.Dockerfile` through `docker buildx`, which must be Docker Engine's plugin, not
Docker Desktop's; the packages land in `target/dist`.

A tag's release goes into the signed repositories of [contr-packages](https://github.com/contr-hq/contr-packages)
within a day, or at once when this repository has a `PACKAGES_DISPATCH_TOKEN` secret: a fine-grained token with Contents
read and write on `contr-packages`.

## Flatpak (beta)

`packaging/flatpak/io.github.contr_hq.Contr.yml` builds the client on the GNOME 50 runtime; `.github/workflows/flatpak.yml`
uploads the bundle. Nothing of the daemon runs in the sandbox: the app reaches the host's contrd through
`$XDG_RUNTIME_DIR/contr` (`--filesystem=xdg-run/contr`), where the `contr-daemon` package's `contrd.socket` starts it
on the first connection. The build fetches crates and the `contr-sdk` git dependency over the network, which Flathub
does not allow; a Flathub build needs vendored sources.

In the sandbox the client:

- hides "Start Contr when I log in", since the autostart directory it sees is private to the sandbox;
- shows the tray under the bus's unique name, since it may not own a `StatusNotifierItem-PID-ID` name;
- reads the daemon's log from its file in the runtime directory, since `journalctl` is not in the sandbox;
- cannot switch the Docker CLI context or open the kubeconfig file, which live outside the sandbox.

## Checks before every push

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo xtask archlint
```
