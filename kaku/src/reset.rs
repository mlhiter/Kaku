use anyhow::{anyhow, bail, Context};
use clap::Parser;
use std::io::{self, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Parser, Clone, Default)]
pub struct ResetCommand {
    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,
}

impl ResetCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        imp::run(self.yes)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use anyhow::bail;

    pub fn run(_yes: bool) -> anyhow::Result<()> {
        bail!("`kaku reset` is currently supported on macOS only")
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    const KAKU_SOURCE_PATTERN: &str = "kaku/zsh/kaku.zsh";
    const KAKU_PATH_MARKER: &str = "# Kaku PATH Integration";
    const KAKU_TMUX_SOURCE_PATTERN: &str = "kaku/tmux/kaku.tmux.conf";
    const KAKU_LEGACY_INLINE_MARKER: &str = "# Kaku Shell Integration";
    const KAKU_LEGACY_INLINE_VAR: &str = "KAKU_ZSH_DIR";
    const KAKU_LEGACY_SYNTAX_HINT: &str = "zsh-syntax-highlighting/zsh-syntax-highlighting.zsh";

    const KAKU_GIT_DEFAULTS: &[(&str, &str)] = &[
        ("core.pager", "delta"),
        ("interactive.diffFilter", "delta --color-only"),
        ("delta.navigate", "true"),
        ("delta.pager", "less --mouse --wheel-lines=3 -R -F -X"),
        ("delta.line-numbers", "true"),
        ("delta.side-by-side", "true"),
        ("delta.line-fill-method", "spaces"),
        ("delta.syntax-theme", "Coldark-Dark"),
        ("delta.file-style", "omit"),
        ("delta.file-decoration-style", "omit"),
        ("delta.hunk-header-style", "file line-number syntax"),
    ];

    #[derive(Default)]
    struct ResetReport {
        changed: Vec<String>,
        skipped: Vec<String>,
    }

    impl ResetReport {
        fn changed(&mut self, msg: impl Into<String>) {
            self.changed.push(msg.into());
        }

        fn skipped(&mut self, msg: impl Into<String>) {
            self.skipped.push(msg.into());
        }

        fn print(self) {
            if !self.changed.is_empty() {
                println!("Applied reset actions:");
                for line in &self.changed {
                    println!("  - {}", line);
                }
            }

            if !self.skipped.is_empty() {
                println!("\nSkipped:");
                for line in &self.skipped {
                    println!("  - {}", line);
                }
            }

            println!("\nKaku reset completed.");
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum KakuShell {
        Zsh,
        Fish,
    }

    pub fn run(yes: bool) -> anyhow::Result<()> {
        confirm_reset(yes)?;

        let mut report = ResetReport::default();
        let active_shell = detect_target_shell();

        remove_zsh_integration(&mut report)?;
        remove_fish_integration(&mut report)?;
        remove_kaku_shell_dir(&mut report)?;
        remove_tmux_integration(&mut report)?;
        remove_file_if_exists(
            config_home().join("tmux").join("kaku.tmux.conf"),
            "removed managed tmux integration script",
            &mut report,
        )?;
        cleanup_git_delta_defaults(&mut report)?;
        cleanup_theme_block(&mut report)?;
        remove_file_if_exists(
            config_home().join("state.json"),
            "removed persisted Kaku state",
            &mut report,
        )?;
        remove_file_if_exists(
            config_home().join(".kaku_config_version"),
            "removed legacy Kaku config version marker",
            &mut report,
        )?;
        remove_file_if_exists(
            config_home().join(".kaku_window_geometry"),
            "removed legacy Kaku window geometry marker",
            &mut report,
        )?;
        remove_file_if_exists(
            config_home().join("lazygit_state.json"),
            "removed Lazygit hint state",
            &mut report,
        )?;
        remove_dir_if_exists(
            config_home().join("backups"),
            "removed Kaku backup directory",
            &mut report,
        )?;
        remove_empty_kaku_config_dir(&mut report)?;

        report.print();

        println!("\n⚠️  Shell restart required.");
        println!(
            "ℹ️  Tools preserved in ~/.config/kaku/{}/\n",
            if active_shell == KakuShell::Fish {
                "fish"
            } else {
                "zsh"
            }
        );

        if !yes && io::stdin().is_terminal() {
            print!("Restart shell now? [Y/n] ");
            io::stdout().flush().context("flush stdout")?;

            let mut input = String::new();
            io::stdin()
                .read_line(&mut input)
                .context("read restart confirmation")?;

            let answer = input.trim().to_ascii_lowercase();
            if answer.is_empty() || answer == "y" || answer == "yes" {
                println!("\nRestarting shell... 👋");
                println!("Tip: Run 'kaku init' to restore integration");
                let shell = default_shell_path_for_exec();
                let err = std::process::Command::new(&shell).arg("-l").exec();
                bail!("failed to restart shell: {}", err);
            } else {
                let shell_label = shell_exec_label(active_shell);
                println!(
                    "\nRun '{} -l' when ready. Restore with 'kaku init'",
                    shell_label
                );
            }
        } else {
            let shell_label = shell_exec_label(active_shell);
            println!(
                "Run '{} -l' to restart. Restore with 'kaku init'",
                shell_label
            );
        }

        Ok(())
    }

    fn confirm_reset(yes: bool) -> anyhow::Result<()> {
        if yes {
            return Ok(());
        }

        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!("non-interactive terminal detected; rerun with --yes to confirm reset")
        }

        println!(
            "This will remove Kaku shell and tmux integration and reset Kaku-managed git defaults."
        );
        print!("Continue with reset? [y/N] ");
        io::stdout().flush().context("flush stdout")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("read reset confirmation")?;

        let answer = input.trim().to_ascii_lowercase();
        if answer == "y" || answer == "yes" {
            Ok(())
        } else {
            println!("Reset cancelled.");
            std::process::exit(0);
        }
    }

    fn home_dir() -> PathBuf {
        config::HOME_DIR.clone()
    }

    fn detect_target_shell() -> KakuShell {
        let shell = std::env::var("KAKU_TARGET_SHELL")
            .ok()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/zsh".to_string());

        let shell = shell.trim().to_lowercase();
        let file = Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&shell);

        if file.ends_with("fish") || file == "fish" {
            KakuShell::Fish
        } else {
            KakuShell::Zsh
        }
    }

    fn shell_exec_label(shell: KakuShell) -> &'static str {
        match shell {
            KakuShell::Fish => "exec fish",
            KakuShell::Zsh => "exec zsh",
        }
    }

    fn default_shell_path_for_exec() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
    }

    fn config_home() -> PathBuf {
        home_dir().join(".config").join("kaku")
    }

    fn zshrc_path() -> PathBuf {
        if let Some(zdotdir) = std::env::var_os("ZDOTDIR") {
            PathBuf::from(zdotdir).join(".zshrc")
        } else {
            home_dir().join(".zshrc")
        }
    }

    fn tmuxrc_path() -> PathBuf {
        home_dir().join(".tmux.conf")
    }

    fn contains_tmux_source_command(line: &str) -> bool {
        line.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
            .any(|token| token == "source-file")
    }

    fn is_active_kaku_tmux_source_line(trimmed_line: &str) -> bool {
        if trimmed_line.starts_with('#') || !trimmed_line.contains(KAKU_TMUX_SOURCE_PATTERN) {
            return false;
        }
        contains_tmux_source_command(trimmed_line)
    }

    fn remove_zsh_integration(report: &mut ResetReport) -> anyhow::Result<()> {
        let zshrc = zshrc_path();
        if !zshrc.exists() {
            report.skipped(format!("{} not found", zshrc.display()));
            return Ok(());
        }

        let original =
            std::fs::read_to_string(&zshrc).with_context(|| format!("read {}", zshrc.display()))?;
        let filtered: Vec<&str> = original
            .lines()
            .filter(|line| !line.contains(KAKU_SOURCE_PATTERN) && !line.contains(KAKU_PATH_MARKER))
            .collect();
        let removed_managed_lines = filtered.len() != original.lines().count();

        let mut updated = filtered.join("\n");
        if !updated.is_empty() {
            updated.push('\n');
        }

        let (updated, removed_legacy_block) = strip_legacy_inline_block(&updated);
        if !removed_managed_lines && !removed_legacy_block {
            report.skipped(format!(
                "no Kaku shell integration found in {}",
                zshrc.display()
            ));
            return Ok(());
        }

        std::fs::write(&zshrc, updated).with_context(|| format!("write {}", zshrc.display()))?;
        if removed_managed_lines && removed_legacy_block {
            report.changed(format!(
                "removed Kaku-managed .zshrc lines and legacy inline block from {}",
                zshrc.display()
            ));
        } else if removed_managed_lines {
            report.changed(format!(
                "removed Kaku-managed .zshrc lines from {}",
                zshrc.display()
            ));
        } else {
            report.changed(format!(
                "removed legacy inline Kaku block from {}",
                zshrc.display()
            ));
        }
        Ok(())
    }

    fn remove_fish_integration(report: &mut ResetReport) -> anyhow::Result<()> {
        let conf_d_file = home_dir()
            .join(".config")
            .join("fish")
            .join("conf.d")
            .join("kaku.fish");
        remove_file_if_exists(
            conf_d_file,
            "removed ~/.config/fish/conf.d/kaku.fish",
            report,
        )?;

        let fish_init = config_home().join("fish").join("kaku.fish");
        remove_file_if_exists(fish_init, "removed ~/.config/kaku/fish/kaku.fish", report)?;

        let fish_wrapper = config_home().join("fish").join("bin").join("kaku");
        remove_file_if_exists(
            fish_wrapper,
            "removed ~/.config/kaku/fish/bin/kaku wrapper",
            report,
        )?;
        Ok(())
    }

    fn remove_kaku_shell_dir(report: &mut ResetReport) -> anyhow::Result<()> {
        let kaku_init = config_home().join("zsh").join("kaku.zsh");
        if kaku_init.exists() {
            std::fs::remove_file(&kaku_init)
                .with_context(|| format!("remove {}", kaku_init.display()))?;
            report.changed(format!("removed {}", kaku_init.display()));
        } else {
            report.skipped(format!("{} not found", kaku_init.display()));
        }

        Ok(())
    }

    fn remove_tmux_integration(report: &mut ResetReport) -> anyhow::Result<()> {
        let tmuxrc = tmuxrc_path();
        if !tmuxrc.exists() {
            report.skipped(format!("{} not found", tmuxrc.display()));
            return Ok(());
        }

        let original = std::fs::read_to_string(&tmuxrc)
            .with_context(|| format!("read {}", tmuxrc.display()))?;
        let filtered: Vec<&str> = original
            .lines()
            .filter(|line| !is_active_kaku_tmux_source_line(line.trim_start()))
            .collect();

        if filtered.len() == original.lines().count() {
            report.skipped(format!(
                "no Kaku tmux integration found in {}",
                tmuxrc.display()
            ));
            return Ok(());
        }

        let mut updated = filtered.join("\n");
        if !updated.is_empty() {
            updated.push('\n');
        }
        std::fs::write(&tmuxrc, updated).with_context(|| format!("write {}", tmuxrc.display()))?;
        report.changed(format!(
            "removed Kaku-managed tmux source line from {}",
            tmuxrc.display()
        ));
        Ok(())
    }

    fn cleanup_git_delta_defaults(report: &mut ResetReport) -> anyhow::Result<()> {
        if !command_exists("git") {
            report.skipped("git not found; skipped git config cleanup");
            return Ok(());
        }

        let mut removed = Vec::new();
        for (key, expected) in KAKU_GIT_DEFAULTS {
            if unset_git_key_if_matches(key, expected)? {
                removed.push(*key);
            }
        }

        if removed.is_empty() {
            report.skipped("no Kaku-managed git defaults to remove");
        } else {
            report.changed(format!("removed git defaults: {}", removed.join(", ")));
        }

        Ok(())
    }

    fn unset_git_key_if_matches(key: &str, expected: &str) -> anyhow::Result<bool> {
        let output = Command::new("git")
            .args(["config", "--global", "--get-all", key])
            .output()
            .with_context(|| format!("query git config key {}", key))?;

        if !output.status.success() {
            if output.status.code() == Some(1) {
                return Ok(false);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "git config --get-all {} failed: {}",
                key,
                stderr.trim()
            ));
        }

        let values: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();

        if values.is_empty() || values.iter().any(|v| v != expected) {
            return Ok(false);
        }

        let status = Command::new("git")
            .args(["config", "--global", "--unset-all", key])
            .status()
            .with_context(|| format!("unset git config key {}", key))?;

        Ok(status.success())
    }

    fn command_exists(name: &str) -> bool {
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn cleanup_theme_block(report: &mut ResetReport) -> anyhow::Result<()> {
        let config_path = config_home().join("kaku.lua");
        if !config_path.exists() {
            report.skipped(format!("{} not found", config_path.display()));
            return Ok(());
        }

        let original = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;

        let (after_managed, changed_managed) =
            strip_theme_block(&original, "-- ===== Kaku Theme Defaults (managed) =====");
        let (after_legacy, changed_legacy) =
            strip_theme_block(&after_managed, "-- ===== Kaku Theme =====");

        if !changed_managed && !changed_legacy {
            report.skipped("no managed Kaku theme block found in ~/.config/kaku/kaku.lua");
            return Ok(());
        }

        std::fs::write(&config_path, after_legacy)
            .with_context(|| format!("write {}", config_path.display()))?;
        report.changed("removed managed Kaku theme block from ~/.config/kaku/kaku.lua");
        Ok(())
    }

    fn strip_theme_block(content: &str, marker: &str) -> (String, bool) {
        let lines: Vec<&str> = content.lines().collect();
        let Some(start) = lines.iter().position(|line| line.contains(marker)) else {
            return (content.to_string(), false);
        };

        // Safety-first: only strip blocks that have a clear `return config`
        // terminator after the marker. Otherwise we leave user content untouched.
        let return_after = lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, line)| line.trim() == "return config")
            .map(|(idx, _)| idx);
        let Some(return_idx) = return_after else {
            return (content.to_string(), false);
        };

        let mut out = Vec::new();
        out.extend_from_slice(&lines[..start]);

        let return_before = out.iter().any(|line| line.trim() == "return config");
        if !return_before {
            out.push("return config");
        }
        out.extend_from_slice(&lines[return_idx + 1..]);

        let mut merged = out.join("\n");
        if !merged.is_empty() {
            merged.push('\n');
        }

        (merged, true)
    }

    /// Strips the old inline Kaku shell integration block that was written directly
    /// into .zshrc in early Kaku versions (before the source-based kaku.zsh approach).
    /// The block starts with `# Kaku Shell Integration`, contains `KAKU_ZSH_DIR`, and
    /// ends with a `fi` that follows the zsh-syntax-highlighting line.
    fn strip_legacy_inline_block(content: &str) -> (String, bool) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return (content.to_string(), false);
        }

        let mut out = Vec::with_capacity(lines.len());
        let mut i = 0usize;
        let mut changed = false;

        while i < lines.len() {
            let current = lines[i].trim();
            if current == KAKU_LEGACY_INLINE_MARKER {
                let mut j = i + 1;
                let mut saw_kaku_var = false;
                let mut saw_syntax_line = false;
                let mut end = None;

                while j < lines.len() {
                    let line = lines[j];
                    if line.contains(KAKU_LEGACY_INLINE_VAR) {
                        saw_kaku_var = true;
                    }
                    if line.contains(KAKU_LEGACY_SYNTAX_HINT) {
                        saw_syntax_line = true;
                    }
                    if saw_syntax_line && line.trim() == "fi" {
                        end = Some(j);
                        break;
                    }
                    if j.saturating_sub(i) > 600 {
                        break;
                    }
                    j += 1;
                }

                if saw_kaku_var {
                    if let Some(end_idx) = end {
                        changed = true;
                        i = end_idx + 1;
                        while i < lines.len() && lines[i].trim().is_empty() {
                            i += 1;
                        }
                        continue;
                    } else {
                        return (content.to_string(), false);
                    }
                }
            }

            out.push(lines[i]);
            i += 1;
        }

        if !changed {
            return (content.to_string(), false);
        }

        let mut merged = out.join("\n");
        if !merged.is_empty() {
            merged.push('\n');
        }
        (merged, true)
    }

    fn remove_file_if_exists(
        path: PathBuf,
        changed_msg: &str,
        report: &mut ResetReport,
    ) -> anyhow::Result<()> {
        if !path.exists() {
            report.skipped(format!("{} not found", path.display()));
            return Ok(());
        }

        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        report.changed(changed_msg.to_string());
        Ok(())
    }

    fn remove_dir_if_exists(
        path: PathBuf,
        changed_msg: &str,
        report: &mut ResetReport,
    ) -> anyhow::Result<()> {
        if !path.exists() {
            report.skipped(format!("{} not found", path.display()));
            return Ok(());
        }

        std::fs::remove_dir_all(&path).with_context(|| format!("remove {}", path.display()))?;
        report.changed(changed_msg.to_string());
        Ok(())
    }

    fn remove_empty_kaku_config_dir(report: &mut ResetReport) -> anyhow::Result<()> {
        let dir = config_home();
        if !dir.exists() {
            return Ok(());
        }

        if is_dir_empty(&dir)? {
            std::fs::remove_dir(&dir).with_context(|| format!("remove {}", dir.display()))?;
            report.changed(format!("removed empty {}", dir.display()));
        }

        Ok(())
    }

    fn is_dir_empty(path: &Path) -> anyhow::Result<bool> {
        let mut iter =
            std::fs::read_dir(path).with_context(|| format!("read {}", path.display()))?;
        Ok(iter.next().is_none())
    }

    #[cfg(test)]
    mod tests {
        use super::{is_active_kaku_tmux_source_line, KAKU_TMUX_SOURCE_PATTERN};

        #[test]
        fn active_tmux_source_line_is_detected() {
            let line =
                r#"source-file "$HOME/.config/kaku/tmux/kaku.tmux.conf" # Kaku tmux Integration"#;
            assert!(is_active_kaku_tmux_source_line(line));
        }

        #[test]
        fn commented_tmux_source_line_is_ignored() {
            let line = r#"# source-file "$HOME/.config/kaku/tmux/kaku.tmux.conf""#;
            assert!(!is_active_kaku_tmux_source_line(line));
        }

        #[test]
        fn plain_text_reference_is_ignored() {
            let line = format!("note: {}", KAKU_TMUX_SOURCE_PATTERN);
            assert!(!is_active_kaku_tmux_source_line(&line));
        }
    }
}
