# Contr for Linux: working rules

- Clean room: never inspect, analyze or read files from Docker Desktop, never use the rea MCP server, never read the
  `contr-research` repository. Behavior comes from the core repository's `specs/`.
- Comments follow [docs/COMMENTS.md](docs/COMMENTS.md): facts the code cannot show; `///` on items; `//` only in bodies,
  at most two lines.
- The daemon is reached only through `contr-sdk`. Modules depend on each other only through `-api` crates.
- Same lint bar as the core: rustfmt, clippy pedantic with `-D warnings`, no `unwrap`/`expect`/`panic!` outside tests.
- GTK objects stay on the main thread; async work runs on the relm4/tokio runtime and returns results as messages.
- Conventional Commits; `main` is protected.
