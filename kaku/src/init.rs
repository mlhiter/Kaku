use anyhow::{anyhow, bail, Context};
use clap::Parser;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Parser, Clone, Default)]
pub struct InitCommand {
    /// Refresh shell integration without interactive prompts
    #[arg(long)]
    pub update_only: bool,
}

impl InitCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        imp::run(self.update_only)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use anyhow::bail;

    pub fn run(_update_only: bool) -> anyhow::Result<()> {
        bail!("`kaku init` is currently supported on macOS only")
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    pub fn run(update_only: bool) -> anyhow::Result<()> {
        let shell = detect_target_shell();
        ensure_user_config().context("ensure user config exists")?;

        install_kaku_wrapper(shell).context("install kaku wrapper")?;

        let script = resolve_setup_script(shell).ok_or_else(|| {
            anyhow!(
                "failed to locate {} for Kaku initialization",
                shell.setup_script_name()
            )
        })?;

        let mut cmd = Command::new("/bin/bash");
        cmd.arg(&script).env("KAKU_INIT_INTERNAL", "1");
        cmd.env("KAKU_TARGET_SHELL", shell.wrapper_dir());
        if update_only {
            cmd.arg("--update-only");
        }
        let status = cmd
            .status()
            .with_context(|| format!("run {}", script.display()))?;

        if status.success() {
            return Ok(());
        }

        bail!("kaku init failed with status {}", status);
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum KakuShell {
        Zsh,
        Fish,
    }

    impl KakuShell {
        fn setup_script_name(&self) -> &'static str {
            match self {
                Self::Zsh => "setup_zsh.sh",
                Self::Fish => "setup_fish.sh",
            }
        }

        fn wrapper_dir(&self) -> &'static str {
            match self {
                Self::Zsh => "zsh",
                Self::Fish => "fish",
            }
        }
    }

    fn detect_target_shell() -> KakuShell {
        if let Some(shell) = parse_shell_env("KAKU_TARGET_SHELL") {
            return shell;
        }

        if let Some(shell) = parse_shell_env("SHELL") {
            return shell;
        }

        KakuShell::Zsh
    }

    fn parse_shell_env(var: &str) -> Option<KakuShell> {
        std::env::var_os(var)
            .and_then(sanitize_shell_name)
            .map(|shell| match shell.as_str() {
                "fish" => KakuShell::Fish,
                _ => KakuShell::Zsh,
            })
    }

    fn sanitize_shell_name(shell: OsString) -> Option<String> {
        shell.into_string().ok().map(|s| {
            let s = s.trim().to_lowercase();
            s.rsplit('/').next().unwrap_or("").to_string()
        })
    }

    fn install_kaku_wrapper(shell: KakuShell) -> anyhow::Result<()> {
        let wrapper_path = wrapper_path(shell);
        let wrapper_dir = wrapper_path
            .parent()
            .ok_or_else(|| anyhow!("invalid wrapper path"))?;
        config::create_user_owned_dirs(wrapper_dir).context("create wrapper directory")?;

        if fs::symlink_metadata(&wrapper_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            fs::remove_file(&wrapper_path).with_context(|| {
                format!("remove legacy symlink wrapper {}", wrapper_path.display())
            })?;
        }

        let preferred_bin = resolve_preferred_kaku_bin()
            .unwrap_or_else(|| PathBuf::from("/Applications/Kaku.app/Contents/MacOS/kaku"));
        let preferred_bin = escape_for_double_quotes(&preferred_bin.display().to_string());

        let script = format!(
            r#"#!/bin/bash
set -euo pipefail

if [[ -n "${{KAKU_BIN:-}}" && -x "${{KAKU_BIN}}" ]]; then
	exec "${{KAKU_BIN}}" "$@"
fi

for candidate in \
	"{preferred_bin}" \
	"/Applications/Kaku.app/Contents/MacOS/kaku" \
	"$HOME/Applications/Kaku.app/Contents/MacOS/kaku"; do
	if [[ -n "$candidate" && -x "$candidate" ]]; then
		exec "$candidate" "$@"
	fi
done

echo "kaku: Kaku.app not found. Expected /Applications/Kaku.app." >&2
exit 127
"#
        );

        let mut file = fs::File::create(&wrapper_path)
            .with_context(|| format!("create wrapper {}", wrapper_path.display()))?;
        file.write_all(script.as_bytes())
            .with_context(|| format!("write wrapper {}", wrapper_path.display()))?;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod wrapper {}", wrapper_path.display()))?;
        Ok(())
    }

    fn wrapper_path(shell: KakuShell) -> PathBuf {
        config::HOME_DIR
            .join(".config")
            .join("kaku")
            .join(shell.wrapper_dir())
            .join("bin")
            .join("kaku")
    }

    fn resolve_preferred_kaku_bin() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("KAKU_BIN") {
            let path = PathBuf::from(path);
            if is_executable_file(&path) {
                return Some(path);
            }
        }

        if let Ok(exe) = std::env::current_exe() {
            if exe
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case("kaku"))
                .unwrap_or(false)
                && is_executable_file(&exe)
            {
                return Some(exe);
            }
        }

        for candidate in [
            PathBuf::from("/Applications/Kaku.app/Contents/MacOS/kaku"),
            config::HOME_DIR
                .join("Applications")
                .join("Kaku.app")
                .join("Contents")
                .join("MacOS")
                .join("kaku"),
        ] {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }

        None
    }

    fn is_executable_file(path: &Path) -> bool {
        fs::metadata(path)
            .map(|meta| meta.is_file() && (meta.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }

    fn escape_for_double_quotes(value: &str) -> String {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`")
    }

    fn resolve_setup_script(shell: KakuShell) -> Option<PathBuf> {
        let script = shell.setup_script_name();
        let mut candidates = Vec::new();

        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join("assets").join("shell-integration").join(script));
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(contents_dir) = exe.parent().and_then(|p| p.parent()) {
                candidates.push(contents_dir.join("Resources").join(script));
            }
        }

        candidates.push(config::HOME_DIR.join(".config").join("kaku").join(script));
        candidates.push(PathBuf::from(format!(
            "/Applications/Kaku.app/Contents/Resources/{}",
            script
        )));
        candidates.push(
            config::HOME_DIR
                .join("Applications")
                .join("Kaku.app")
                .join("Contents")
                .join("Resources")
                .join(script),
        );

        candidates.into_iter().find(|p| p.exists())
    }

    fn ensure_user_config() -> anyhow::Result<()> {
        config::ensure_user_config_exists().context("ensure user config exists")?;
        Ok(())
    }
}
