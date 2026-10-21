//! Debian and RPM packages of the client, which nfpm builds in packaging/linux/package.Dockerfile.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail, ensure};
use contr_core::product;

/// Where `cargo about generate` writes the Rust crate notices before the packages are built.
const RUST_CRATES: &str = "target/notices/rust-crates.txt";
const BINARY: &str = "contr-desktop";
const ICON: &str = "crates/contr-ui-common/data/icons/scalable/apps/app.svg";

pub(crate) struct Options {
    pub(crate) version: String,
    /// `contr-desktop` built on Ubuntu 26.04, linked against its GTK.
    pub(crate) deb_binary: PathBuf,
    /// `contr-desktop` built on Fedora 44.
    pub(crate) rpm_binary: PathBuf,
    pub(crate) out: PathBuf,
}

impl Options {
    /// Reads `--version`, `--deb-binary`, `--rpm-binary` and `--out`.
    pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut version = env!("CARGO_PKG_VERSION").to_owned();
        let (mut deb_binary, mut rpm_binary) = (None, None);
        let mut out = PathBuf::from("target/dist");
        while let Some(flag) = args.next() {
            let value = args.next().with_context(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--version" => version = value,
                "--deb-binary" => deb_binary = Some(PathBuf::from(value)),
                "--rpm-binary" => rpm_binary = Some(PathBuf::from(value)),
                "--out" => out = PathBuf::from(value),
                other => bail!("unknown option {other}"),
            }
        }
        Ok(Self {
            version,
            deb_binary: deb_binary.context("--deb-binary is required")?,
            rpm_binary: rpm_binary.context("--rpm-binary is required")?,
            out,
        })
    }
}

/// Assembles each packager's archive in `out`, then builds the packages into `out`.
pub(crate) fn build(root: &Path, options: &Options) -> anyhow::Result<()> {
    ensure_engine_buildx()?;
    let notices = root.join(RUST_CRATES);
    ensure!(
        notices.is_file(),
        "{} is missing; run cargo about generate --config packaging/licenses/about.toml --output-file {RUST_CRATES} \
         packaging/licenses/about.hbs",
        notices.display()
    );
    fs::create_dir_all(&options.out).with_context(|| format!("cannot create {}", options.out.display()))?;
    let deb = archive(root, &options.out, "deb", &options.deb_binary, &notices)?;
    let rpm = archive(root, &options.out, "rpm", &options.rpm_binary, &notices)?;

    let status = Command::new("docker")
        .args(["buildx", "build", "--file"])
        .arg(root.join("packaging/linux/package.Dockerfile"))
        .arg("--build-context")
        .arg(format!("deb={}", deb.display()))
        .arg("--build-context")
        .arg(format!("rpm={}", rpm.display()))
        .arg("--build-arg")
        .arg(format!("VERSION={}", options.version))
        .arg("--output")
        .arg(format!("type=local,dest={}", options.out.display()))
        .arg(root.join("packaging/linux"))
        .status()
        .context("cannot run docker buildx")?;
    ensure!(status.success(), "building the Linux packages failed with {status}");

    println!("{}", options.out.display());
    Ok(())
}

/// The files nfpm.yaml takes from `archive/` for one packager.
fn archive(root: &Path, out: &Path, packager: &str, binary: &Path, notices: &Path) -> anyhow::Result<PathBuf> {
    ensure!(binary.is_file(), "{} is not a file", binary.display());
    let dir = out.join(format!("{}-linux-{packager}", product::SLUG));
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("cannot empty {}", dir.display()))?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let copy =
        |from: &Path, to: &str| fs::copy(from, dir.join(to)).with_context(|| format!("cannot copy {}", from.display()));
    copy(binary, BINARY)?;
    copy(&root.join("LICENSE"), "LICENSE")?;
    copy(&root.join(ICON), &format!("{}.svg", product::APP_ID))?;
    let crates = fs::read_to_string(notices).with_context(|| format!("cannot read {}", notices.display()))?;
    fs::write(
        dir.join("THIRD-PARTY-NOTICES.md"),
        format!("# Third-party notices\n\n```text\n{crates}\n```\n"),
    )
    .context("cannot write THIRD-PARTY-NOTICES.md")?;
    Ok(dir)
}

/// Refuses Docker Desktop's buildx plugin, which shadows the engine's own plugin where both are installed;
/// see CLAUDE.md.
fn ensure_engine_buildx() -> anyhow::Result<()> {
    let output = Command::new("docker")
        .args(["buildx", "version"])
        .output()
        .context("cannot run docker buildx")?;
    let version = String::from_utf8_lossy(&output.stdout);
    ensure!(
        output.status.success() && !version.contains("-desktop"),
        "docker buildx resolves to a Docker Desktop plugin ({}); set DOCKER_CONFIG to a directory whose \
         cli-plugins links the engine's docker-buildx",
        version.trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NFPM: &str = include_str!("../../packaging/linux/nfpm.yaml");
    const DESKTOP: &str = include_str!("../../packaging/linux/io.github.contr_hq.Contr.desktop");
    const METAINFO: &str = include_str!("../../packaging/linux/io.github.contr_hq.Contr.metainfo.xml");

    fn installs(src: &str, dst: &str) -> bool {
        NFPM.contains(&format!("- src: {src}\n    dst: {dst}\n"))
    }

    #[test]
    fn packages_install_files_where_the_desktop_looks_for_them() {
        let id = product::APP_ID;
        assert!(NFPM.contains(&format!("\nname: {}\n", product::SLUG)));
        assert!(installs(&format!("archive/{BINARY}"), &format!("/usr/bin/{BINARY}")));
        assert!(DESKTOP.contains(&format!("\nExec={BINARY}\n")));
        assert!(DESKTOP.contains(&format!("\nIcon={id}\n")));
        assert!(installs(
            &format!("linux/{id}.desktop"),
            &format!("/usr/share/applications/{id}.desktop")
        ));
        assert!(installs(
            &format!("archive/{id}.svg"),
            &format!("/usr/share/icons/hicolor/scalable/apps/{id}.svg")
        ));
        assert!(installs(
            &format!("linux/{id}.metainfo.xml"),
            &format!("/usr/share/metainfo/{id}.metainfo.xml")
        ));
        assert!(METAINFO.contains(&format!("<id>{id}</id>")));
        assert!(METAINFO.contains(&format!("<launchable type=\"desktop-id\">{id}.desktop</launchable>")));
        assert!(NFPM.contains(&format!("{} (>= ", product::DAEMON_PACKAGE)));
    }

    #[test]
    fn options_need_both_binaries() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>().into_iter();
        assert!(Options::parse(args(&["--deb-binary", "a"])).is_err());
        let options = Options::parse(args(&["--deb-binary", "a", "--rpm-binary", "b", "--version", "0.2.0"])).unwrap();
        assert_eq!(options.version, "0.2.0");
        assert_eq!(options.out, PathBuf::from("target/dist"));
    }
}
