use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use clap::Parser;

const REPO: &str = "whjvenyl/ticgit";

#[derive(Debug, Parser)]
pub struct Args {
    /// Check for updates without installing.
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: Args) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = fetch_latest_version()?;

    if latest == current {
        println!("Already up to date (v{current}).");
        return Ok(());
    }

    println!("Update available: v{current} -> v{latest}");

    if args.check {
        return Ok(());
    }

    let target = detect_target()?;
    let url = format!("https://github.com/{REPO}/releases/latest/download/ticgit-{target}.tar.gz");

    println!("Downloading from: {url}");

    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let archive_path = tmp.path().join("ticgit.tar.gz");

    let status = Command::new("curl")
        .args(["-fSL", &url, "-o"])
        .arg(&archive_path)
        .status()
        .context("running curl")?;

    if !status.success() {
        anyhow::bail!("download failed");
    }

    let status = Command::new("tar")
        .args(["xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(tmp.path())
        .status()
        .context("extracting archive")?;

    if !status.success() {
        anyhow::bail!("extraction failed");
    }

    let new_binary = tmp.path().join("ti");
    let current_binary = env::current_exe().context("finding current binary path")?;

    replace_current_binary(&new_binary, &current_binary)
        .context("replacing binary — you may need to run with sudo")?;

    println!("Updated to v{latest}.");
    Ok(())
}

fn replace_current_binary(new_binary: &Path, current_binary: &Path) -> Result<()> {
    let parent = current_binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current binary path has no parent"))?;
    let current_permissions = fs::metadata(current_binary)
        .with_context(|| format!("reading permissions for {}", current_binary.display()))?
        .permissions();

    let mut source = fs::File::open(new_binary)
        .with_context(|| format!("opening new binary {}", new_binary.display()))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".ti-update-")
        .tempfile_in(parent)
        .with_context(|| format!("creating staged binary in {}", parent.display()))?;

    io::copy(&mut source, staged.as_file_mut())
        .with_context(|| format!("staging new binary {}", new_binary.display()))?;
    staged
        .as_file_mut()
        .sync_all()
        .context("flushing staged binary")?;
    fs::set_permissions(staged.path(), current_permissions).with_context(|| {
        format!(
            "setting staged binary permissions for {}",
            staged.path().display()
        )
    })?;

    let staged_path = staged.into_temp_path();
    let staged_path_ref: &Path = staged_path.as_ref();
    fs::rename(staged_path_ref, current_binary).with_context(|| {
        format!(
            "atomically replacing {} with {}",
            current_binary.display(),
            staged_path.display()
        )
    })?;

    Ok(())
}

fn fetch_latest_version() -> Result<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/json",
            &format!("https://github.com/{REPO}/releases/latest"),
        ])
        .output()
        .context("fetching latest release")?;

    if !output.status.success() {
        anyhow::bail!("failed to check for updates");
    }

    let body = String::from_utf8_lossy(&output.stdout);

    // GitHub redirects to /releases/tag/vX.Y.Z and the JSON response
    // contains "tag_name":"vX.Y.Z"
    let tag = body
        .split("\"tag_name\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .ok_or_else(|| anyhow::anyhow!("could not parse latest version from GitHub"))?;

    Ok(tag.trim_start_matches('v').to_string())
}

fn detect_target() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let os_target = match os {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => anyhow::bail!("self-update on Windows is not yet supported"),
        other => anyhow::bail!("unsupported OS: {other}"),
    };

    let arch_target = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => anyhow::bail!("unsupported architecture: {other}"),
    };

    Ok(format!("{arch_target}-{os_target}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_current_binary_swaps_contents_and_preserves_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("ti");
        let next = tmp.path().join("new-ti");

        fs::write(&current, "old").unwrap();
        fs::write(&next, "new").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&current).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&current, permissions).unwrap();
        }

        replace_current_binary(&next, &current).unwrap();

        assert_eq!(fs::read_to_string(&current).unwrap(), "new");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&current).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }

        let leftovers = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ti-update-")
            })
            .count();
        assert_eq!(leftovers, 0);
    }
}
