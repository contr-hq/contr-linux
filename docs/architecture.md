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
engine state and failure reason, `GetInfo`'s engine description while the engine runs, and the running engine
operation with its progress, whichever client started it.

Pages follow the engine with `while_running`, which calls a page's stream body with a client each time the engine
runs and drops it when the engine leaves `Running` or the connection changes. Every engine RPC fails with
`ENGINE_NOT_RUNNING` otherwise, so pages show no counts or lists while the engine is stopped.

`EngineGate::around` wraps a page's content in a stack that shows the content while the engine runs and the gate
otherwise: the stopped, starting, failed and unreachable states, the Start Engine button, and the "start the engine
when I log in" setting (`engine.start_with_daemon`).

## Operations

Pulls, pushes, Compose actions, image imports, diagnostics bundles, fixes and engine start, stop, restart and reset run
as operations the daemon owns (`OperationsService`), so they go on when a page closes, the window closes or the client
quits. `Daemon` follows `WatchOperations` while connected, starting over after each reconnect, and publishes the
`Activity`, every operation the daemon keeps, newest first. Pages start work with `StartOperation`, take their busy
state from the activity, and follow their own subject's operation with `FollowOperation`, which replays the events the
daemon kept; a page opened again, or a client started again, shows the progress of work already running.

The shell reports each operation that finishes in one toast overlay, with the next step where there is one: Run for a
pulled image, Save… for a bundle, and the first remediation for a failure. It sends a desktop notification as well
while the window is not in front and `notify.finished` is on (the default). A page that tells the user the result
itself, as the bundle's save and onboarding's fixes do, claims the operation so the shell leaves it out. Work that is
not an operation but should finish after its page closes, such as a container action from the container page, runs on
its own task and reports through `Daemon::notice`.

The activity button in the sidebar header lists the running and recent operations with their progress, Cancel where
the daemon allows it, the remediations of a failure, and Save… for a finished bundle, which replays the bundle's
chunks into the file a save dialog names.

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

## Images and the Run dialog

`contr-images` follows `WatchImages`, and `WatchContainers` to name the containers that use each image. Images that no
container uses, untagged ones included, can be removed together with `PruneImages`. A pull streams `PullImage` into a
card with the overall bytes and estimate and a line per layer, for every pull the daemon runs; Cancel calls
`CancelOperation`. Tag… calls `TagImage`, and Push shows the engine's latest status line, since a push reports no
per-layer progress; a registry that refuses the push names the `docker login` to run, because pulls and pushes use the
credentials the Docker CLI stores.

The Run dialog belongs to the containers module and is reached from other modules through `RunImage`. It inspects the
image to offer the ports it exposes, picks host ports that no container publishes and `CheckHostPorts` reports free,
publishes them on 127.0.0.1, and calls `CreateContainer` with `start`, pulling first when the image is not local.
Cancel during that pull cancels it when the dialog started it, and nothing is created.

## Volumes and networks

Which container mounts a volume or joins a network is known only from `InspectContainer`, so
`contr_daemon_ui::follow_attachments` follows `WatchContainers` and inspects each container that appears or changes.
Volumes and Networks combine it with `WatchVolumes` and `WatchNetworks`. The API reports no volume sizes, so the pages
show none. A user network connects any container not on it yet through `ConnectNetwork`, with optional aliases, and each
member disconnects through `DisconnectNetwork`.

## Compose

`contr-compose` follows `ComposeService.WatchProjects`, which sends every project after each change, and forwards the
open project to its page. A project page starts Up, Pull, Restart, Stop and Down as operations, one at a time per
project, showing Compose's output lines while one runs and its last lines in a dialog when it fails. Logs follow the project's `StreamLogs`,
filtered by service, and restart when the set of running containers changes.

## Kubernetes

`contr-kubernetes` follows `KubernetesService.WatchCluster` and `SettingsService.WatchSettings` while the engine runs.
The Enabled switch, Version and Traefik are the settings `kubernetes.enabled`, `kubernetes.version` and
`kubernetes.traefik`; Cancel during start-up turns `kubernetes.enabled` off. After `UpdateSettings` the page keeps the
change pending until a cluster state carries the returned etag or a later one. A version change deletes the cluster's
data, so it asks first, as Reset does. The kubectl context switch calls `SetCurrentContext`, and Import Images
starts an import operation for the tagged images the engine holds. The sidebar shows a dot while the cluster is ready and
a spinner while it starts or stops, and a notification says when a start finishes while the window is not focused.

## Troubleshoot

`contr-troubleshoot` runs `DiagnosticsService.RunChecks` once a client exists and again whenever the connection or the
engine state changes. Its alert leads with an unreachable daemon, then a failed engine and its `ErrorReason`, then the
first failing check, and offers the remediations of the failing checks as operations. A fix that succeeds, from any
client, runs the checks again, and one whose outcome needs a new login offers to log out through
`org.gnome.SessionManager`. Logs come from `journalctl -o json`:
the daemon's user unit, falling back to the log file in the runtime directory, or the engine's unit. Create diagnostics
bundle asks where to save it, starts the bundle operation and writes its chunks to that file once the bundle is whole.
Reset Engine starts the factory reset as an engine operation.

## Updates

The client installs no updates itself; packages and Flatpak do. Once connected, and hourly after, it reads
`UpdatesService.GetUpdateStatus`, the result of the daemon's daily check, together with `updates.check`. While that
setting is on and a newer release exists, a banner above the window links to its release page; opening it hides the
banner for that version (`dismissed_update` in `desktop.json`). Preferences shows the latest check, its error if it
failed, Check Now (`CheckForUpdates`) and the `updates.check` and `updates.prereleases` switches.

## Onboarding

Until the user finishes or skips it (`onboarded` in `desktop.json`), the window shows `contr-onboarding` instead of the
sections. It shows the one engine the daemon manages, since no RPC lists the other engines on the computer, so there
is nothing to choose between yet. The fixes come from the failing checks of `RunChecks`: installing an engine gets its
own row, and the rest sit under the engine, with a warning before joining the docker group. A running fix shows the
steps its operation reports; one that needs a new session ends on a page that logs out through
`org.gnome.SessionManager`, which `contr_ui_common::session` serves to Troubleshoot too.

## Capabilities

The sidebar shows Compose and Kubernetes only when the handshake advertises `CAPABILITY_COMPOSE` and
`CAPABILITY_KUBERNETES`.

## Resources

`contr-ui-common/build.rs` compiles `data/` into a GResource bundle at the resource base path derived from
`contr_core::product::APP_ID`. `AdwApplication` loads `style.css` from it, with dark overrides in a
`prefers-color-scheme` media query, and GTK finds the icons under its `icons/` directory. relm4 creates the
application before it has an ID, so `main` sets the resource base path itself.

`style.css` sets Contr's muted slate palette over libadwaita's colour variables: the accent, which replaces the
system accent, the surfaces and the status colours, each pair of text and surface at 4.5:1 or more.
`contr_ui_common::palette` repeats the colours that code draws, such as the Stats charts and the console, and its
tests check them against the stylesheet.

Icons are 16 px symbolic SVGs drawn with strokes, marked `foreground-stroke transparent-fill` so GTK 4.20 and later
recolor them.

## Window and tray

Closing the window hides it while "keep running" is on (the default, in `$XDG_CONFIG_HOME/contr/desktop.json`);
launching the application again presents it. The tray is a `StatusNotifierItem` from ksni; GNOME shows it only with
the AppIndicator extension, which Ubuntu enables. Its icon is sent as pixmaps, since the shell cannot read this
process's resources.

"Start Contr when I log in" writes an XDG autostart entry that passes `--background`, which skips presenting the
window once. Notifications go through `GApplication`: a container that exits with a non-zero code, with buttons that
open its logs or restart it, and finished operations. Preferences edits the daemon's settings through
`UpdateSettings` with a field mask; settings the administrator locked show as managed, and engine configuration
needs `CAPABILITY_ENGINE_CONFIGURATION`.
