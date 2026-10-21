# Architecture

Contr for Linux is one relm4 application built from feature modules. It talks to the `contrd` daemon only through the
`contr-sdk` crate from [contr-hq/contr](https://github.com/contr-hq/contr), pinned to a commit in `Cargo.toml`.

## Crates

```text
crates/contr-desktop      the application: window, sidebar, engine card, primary menu, tray, page routing
crates/contr-daemon-ui    the one daemon connection, the engine state, and the engine gate pages show
crates/contr-ui-common    widgets, styles, icons and formatting every page shares; knows nothing of the daemon
crates/contr-<module>     a feature module: its pages and the SDK calls behind them
crates/contr-<module>-api plain types other modules use to reach a module, such as "open this container"
xtask                     cargo xtask archlint
```

`cargo xtask archlint` enforces the edges: a module depends on another module only through its `-api` crate, `-api`
crates reach neither the daemon nor a module, `contr-ui-common` reaches neither the daemon nor a module, and nothing
depends on `contr-desktop`.

## The daemon connection

`contr-daemon-ui::Daemon` owns the application's single `contr_sdk::Client`. It calls `Client::connect_or_start`, which
starts the daemon's systemd user unit (or `contrd` itself) when nothing answers and `CONTR_HOST` is unset, then follows
`EngineService.WatchState`. When the connection or the watch breaks it reconnects three seconds later. Every change is
published as a `Status` on a `tokio::sync::watch` channel: the link, the handshake (backend and capabilities), the
engine state and failure reason, `GetInfo`'s engine description while the engine runs, and the engine operation this
client is running with its progress.

Pages follow the engine with `while_running`, which calls a page's stream body with a client each time the engine
runs and drops it when the engine leaves `Running` or the connection changes. Every engine RPC fails with
`ENGINE_NOT_RUNNING` otherwise, so pages show no counts or lists while the engine is stopped.

`EngineGate::around` wraps a page's content in a stack that shows the content while the engine runs and the gate
otherwise: the stopped, starting, failed and unreachable states, the Start Engine button, and the "start the engine
when I log in" setting (`engine.start_with_daemon`).

## Threads

GTK objects stay on the main thread. SDK calls run on relm4's tokio runtime, either as component commands
(`ComponentSender::command`, `oneshot_command`) whose results come back as messages, or through `relm4::spawn` awaited
from a `glib` future on the main thread. tonic channels need the tokio runtime, so SDK futures are never polled by the
GLib main loop directly.

## Containers

`contr-containers` follows `ContainersService.WatchContainers` and `WatchContainerStats` while the engine runs and
groups the list by the `com.docker.compose.project` label, standalone containers last. A container that this client
saw running and then exit with a non-zero code raises the banner. Rows are reconciled in place by container ID, so the
two-second stats updates never rebuild the list.

The container page has four views. Logs follows `StreamLogs` with timestamps and reopens it from the last timestamp
when the container starts again. Inspect summarises `InspectContainer` and shows its raw JSON; environment values stay
hidden until asked for. Terminal runs a shell through `Exec` with a pseudo-terminal in a VTE widget and closes the
shell's stdin on disconnect, since the engine keeps an exec running after its client leaves. Stats plots the
`StreamStats` samples received since the view opened; the daemon keeps no history.

## Capabilities

The sidebar shows Compose and Kubernetes only when the handshake advertises `CAPABILITY_COMPOSE` and
`CAPABILITY_KUBERNETES`.

## Resources

`contr-ui-common/build.rs` compiles `data/` into a GResource bundle at the resource base path derived from
`contr_core::product::APP_ID`. `AdwApplication` loads `style.css` from it, with dark overrides in a
`prefers-color-scheme` media query, and GTK finds the icons under its `icons/` directory. relm4 creates the
application before it has an ID, so `main` sets the resource base path itself.

Icons are 16 px symbolic SVGs drawn with strokes, marked `foreground-stroke transparent-fill` so GTK 4.20 and later
recolor them.

## Window and tray

Closing the window hides it while "keep running" is on (the default, in `$XDG_CONFIG_HOME/contr/desktop.json`);
launching the application again presents it. The tray is a `StatusNotifierItem` from ksni; GNOME shows it only with
the AppIndicator extension, which Ubuntu enables. Its icon is sent as pixmaps, since the shell cannot read this
process's resources.
