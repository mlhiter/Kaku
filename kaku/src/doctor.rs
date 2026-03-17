//! Doctor command for diagnosing shell integration, environment, and runtime issues.

use clap::Parser;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug, Parser, Clone, Default)]
pub struct DoctorCommand {
    /// Apply safe automatic fixes, then rerun diagnostics
    #[arg(long)]
    pub fix: bool,
}

impl DoctorCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        let report = build_report();
        print!("{}", render_text_report(&report));

        if self.fix {
            run_auto_fix_and_rerun_report();
            return Ok(());
        }

        if should_offer_auto_fix(&report) && io::stdin().is_terminal() && io::stdout().is_terminal()
        {
            match prompt_yes_no("Run safe auto-fix now with `kaku init --update-only`? [Y/n] ") {
                Ok(true) => run_auto_fix_and_rerun_report(),
                Ok(false) => {}
                Err(err) => eprintln!("Auto-fix prompt skipped: {}", err),
            }
        }

        Ok(())
    }
}

fn should_offer_auto_fix(report: &DoctorReport) -> bool {
    report.overall_status.severity_rank() >= DoctorStatus::Warn.severity_rank()
}

fn prompt_yes_no(question: &str) -> anyhow::Result<bool> {
    print!("{}", question);
    io::stdout().flush()?;

    let mut input = String::new();
    let bytes = io::stdin().read_line(&mut input)?;
    if bytes == 0 {
        return Ok(false);
    }
    let answer = input.trim().to_ascii_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

fn run_auto_fix_and_rerun_report() {
    println!("Auto-fix: running `kaku init --update-only`");
    let init_cmd = crate::init::InitCommand { update_only: true };
    match init_cmd.run() {
        Ok(()) => println!("Auto-fix: completed"),
        Err(err) => println!("Auto-fix: failed: {:#}", err),
    }

    let after = build_report();
    println!();
    println!("After Auto-fix");
    print!("{}", render_text_report(&after));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Warn,
    Fail,
    Info,
}

impl DoctorStatus {
    fn severity_rank(self) -> u8 {
        match self {
            Self::Fail => 3,
            Self::Warn => 2,
            Self::Ok => 1,
            Self::Info => 0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Info => "INFO",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Warn => "!",
            Self::Fail => "x",
            Self::Info => "i",
        }
    }
}

#[derive(Debug)]
struct DoctorReport {
    overall_status: DoctorStatus,
    summary: DoctorSummary,
    groups: Vec<DoctorGroup>,
}

#[derive(Debug)]
struct DoctorSummary {
    ok: usize,
    warn: usize,
    fail: usize,
    info: usize,
}

#[derive(Debug)]
struct DoctorGroup {
    title: &'static str,
    status: DoctorStatus,
    checks: Vec<DoctorCheck>,
}

#[derive(Debug)]
struct DoctorCheck {
    title: &'static str,
    status: DoctorStatus,
    summary: String,
    details: Vec<String>,
    fix: Option<String>,
}

fn build_report() -> DoctorReport {
    let env_group = build_environment_group();
    let shell_group = build_shell_integration_group();
    let runtime_group = build_runtime_group();

    let mut all_checks = Vec::new();
    all_checks.extend(env_group.checks.iter());
    all_checks.extend(shell_group.checks.iter());
    all_checks.extend(runtime_group.checks.iter());

    let summary = DoctorSummary {
        ok: all_checks
            .iter()
            .filter(|c| c.status == DoctorStatus::Ok)
            .count(),
        warn: all_checks
            .iter()
            .filter(|c| c.status == DoctorStatus::Warn)
            .count(),
        fail: all_checks
            .iter()
            .filter(|c| c.status == DoctorStatus::Fail)
            .count(),
        info: all_checks
            .iter()
            .filter(|c| c.status == DoctorStatus::Info)
            .count(),
    };

    let overall_status = if summary.fail > 0 {
        DoctorStatus::Fail
    } else if summary.warn > 0 {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };

    let health_group = build_health_group(overall_status, &summary);

    DoctorReport {
        overall_status,
        summary,
        groups: vec![health_group, env_group, shell_group, runtime_group],
    }
}

fn build_health_group(overall_status: DoctorStatus, summary: &DoctorSummary) -> DoctorGroup {
    let mut details = vec![
        format!(
            "Summary: {} ok, {} warn, {} fail, {} info",
            summary.ok, summary.warn, summary.fail, summary.info
        ),
        format!("Kaku version: {}", doctor_version_string()),
    ];

    if summary.fail > 0 || summary.warn > 0 {
        details.push("Run `kaku init --update-only` after fixing shell or PATH issues".to_string());
    }

    let checks = vec![DoctorCheck {
        title: "Overall Health",
        status: overall_status,
        summary: match overall_status {
            DoctorStatus::Ok => "No blocking issues detected".to_string(),
            DoctorStatus::Warn => "Kaku works but setup is incomplete".to_string(),
            DoctorStatus::Fail => "Kaku command entry is broken or missing".to_string(),
            DoctorStatus::Info => "Informational only".to_string(),
        },
        details,
        fix: if overall_status.severity_rank() >= DoctorStatus::Warn.severity_rank() {
            Some("kaku init --update-only".to_string())
        } else {
            None
        },
    }];

    DoctorGroup {
        title: "Health",
        status: group_status(&checks),
        checks,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum KakuShell {
    Zsh,
    Fish,
}

fn build_environment_group() -> DoctorGroup {
    let mut checks = Vec::new();
    let shell = std::env::var("KAKU_TARGET_SHELL")
        .ok()
        .or_else(|| std::env::var("SHELL").ok());
    let shell_name = shell
        .as_deref()
        .and_then(|s| Path::new(s).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let shell_is_fish = shell_name == "fish";
    let shell_is_zsh = shell_name == "zsh";
    let target_shell = detect_target_shell();

    checks.push(DoctorCheck {
        title: "Current Shell Environment",
        status: if shell_is_fish || shell_is_zsh {
            DoctorStatus::Ok
        } else if shell.is_some() {
            DoctorStatus::Warn
        } else {
            DoctorStatus::Info
        },
        summary: match &shell {
            Some(value) if shell_is_zsh => format!("SHELL is {value}"),
            Some(value) if shell_is_fish => format!("SHELL is {value}"),
            Some(value) => format!("SHELL is {value} and not managed by default"),
            None => "SHELL is not set".to_string(),
        },
        details: vec![
            format!(
                "Kaku shell integration currently targets {} for PATH injection and managed shell config",
                shell_name_if_known(target_shell)
            )
                .to_string(),
            "Doctor reports the current process environment. GUI-launched apps can differ from a Terminal login shell."
                .to_string(),
        ],
        fix: if !shell_is_zsh && !shell_is_fish {
            Some("Use zsh/fish or add the managed Kaku bin directory to your shell PATH manually".to_string())
        } else {
            None
        },
    });

    let managed_bin = managed_bin_dir(target_shell);
    let path_entries: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    let path_has_managed_bin = path_entries.iter().any(|entry| entry == &managed_bin);
    checks.push(DoctorCheck {
        title: "PATH Contains Kaku Managed Bin",
        status: if path_has_managed_bin {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warn
        },
        summary: if path_has_managed_bin {
            format!("PATH includes {}", managed_bin.display())
        } else {
            format!("PATH is missing {}", managed_bin.display())
        },
        details: vec![
            format!(
                "Kaku command wrapper is expected at {}",
                managed_bin_dir(target_shell).display()
            ),
            match target_shell {
                KakuShell::Fish => "This PATH entry is normally added by ~/.config/fish/conf.d/kaku.fish loading the managed Kaku fish init".to_string(),
                KakuShell::Zsh => "This PATH entry is normally added by a managed PATH line in ~/.zshrc".to_string(),
            },
            "PATH in Doctor reflects the current process environment and can differ between GUI and Terminal launches."
                .to_string(),
        ],
        fix: if path_has_managed_bin {
            None
        } else {
            Some(format!(
                "Run `kaku init --update-only` and restart with `exec {}`",
                shell_exec_command(target_shell)
            ))
        },
    });

    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    checks.push(DoctorCheck {
        title: "Shell Config Target Path",
        status: DoctorStatus::Info,
        summary: if target_shell == KakuShell::Fish {
            format!("Fish conf.d entry: {}", fish_config_path().display())
        } else {
            match &zdotdir {
                Some(dir) => format!("ZDOTDIR is {}", dir.display()),
                None => "ZDOTDIR is not set and ~/.zshrc is used".to_string(),
            }
        },
        details: vec![format!(
            "Expected config path: {}",
            shell_config_path(target_shell).display()
        )],
        fix: None,
    });

    DoctorGroup {
        title: "Environment",
        status: group_status(&checks),
        checks,
    }
}

fn build_shell_integration_group() -> DoctorGroup {
    let mut checks = Vec::new();
    let target_shell = detect_target_shell();

    let init_file = managed_init_file(target_shell);
    let init_exists = init_file.is_file();
    checks.push(DoctorCheck {
        title: match target_shell {
            KakuShell::Fish => "Managed Fish Init File",
            KakuShell::Zsh => "Managed Zsh Init File",
        },
        status: if init_exists {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Warn
        },
        summary: if init_exists {
            format!("Found {}", init_file.display())
        } else {
            format!("Missing {}", init_file.display())
        },
        details: vec!["Kaku writes PATH and shell integration to this managed file".to_string()],
        fix: if init_exists {
            None
        } else {
            Some("Run `kaku init --update-only`".to_string())
        },
    });

    let wrapper = managed_wrapper_path(target_shell);
    let wrapper_exists = wrapper.is_file();
    let wrapper_exec = config::is_executable_file(&wrapper);
    checks.push(DoctorCheck {
        title: "Kaku Wrapper Script",
        status: if wrapper_exists && wrapper_exec {
            DoctorStatus::Ok
        } else if wrapper_exists {
            DoctorStatus::Warn
        } else {
            DoctorStatus::Fail
        },
        summary: if wrapper_exists && wrapper_exec {
            format!("Wrapper is ready at {}", wrapper.display())
        } else if wrapper_exists {
            format!(
                "Wrapper exists but is not executable: {}",
                wrapper.display()
            )
        } else {
            format!("Wrapper is missing: {}", wrapper.display())
        },
        details: vec![
            "The `kaku` shell command is provided by this wrapper".to_string(),
            "Wrapper is generated by `kaku init` before shell setup runs".to_string(),
        ],
        fix: if wrapper_exists && wrapper_exec {
            None
        } else if wrapper_exists {
            Some(format!(
                "Run `chmod +x {}` or `kaku init --update-only`",
                wrapper.display()
            ))
        } else {
            Some("Run `kaku init --update-only`".to_string())
        },
    });

    let shell_config = shell_config_path(target_shell);
    let source_check = match target_shell {
        KakuShell::Zsh => {
            let check = check_zshrc_source_line(&shell_config);
            ShellSourceCheck::Zsh(check)
        }
        KakuShell::Fish => {
            let check = check_fish_config_line(&shell_config);
            ShellSourceCheck::Fish(check)
        }
    };
    checks.push(DoctorCheck {
        title: match target_shell {
            KakuShell::Fish => "fish conf.d Sources Kaku Init",
            KakuShell::Zsh => "zshrc Sources Kaku Init",
        },
        status: match &source_check {
            ShellSourceCheck::Zsh(check) => {
                if check.read_error.is_some() {
                    DoctorStatus::Fail
                } else if check.has_active_lines() && !check.has_legacy_guarded_lines() {
                    DoctorStatus::Ok
                } else {
                    DoctorStatus::Warn
                }
            }
            ShellSourceCheck::Fish(check) => {
                if check.read_error.is_some() {
                    DoctorStatus::Fail
                } else if check.has_active_lines() {
                    DoctorStatus::Ok
                } else {
                    DoctorStatus::Warn
                }
            }
        },
        summary: match &source_check {
            ShellSourceCheck::Zsh(check) => {
                if let Some(err) = &check.read_error {
                    format!("Could not read {}: {}", shell_config.display(), err)
                } else if check.missing_file {
                    format!("No {} file found at {}", shell_config_label(target_shell), shell_config.display())
                } else if check.has_legacy_guarded_lines() {
                    format!(
                        "Found {} active Kaku source line(s), including {} legacy guarded line(s) in {}",
                        check.guarded_active_lines + check.unguarded_active_lines,
                        check.guarded_active_lines,
                        shell_config.display()
                    )
                } else if check.has_active_lines() {
                    format!(
                        "Found {} active Kaku source line(s) in {}",
                        check.guarded_active_lines + check.unguarded_active_lines,
                        shell_config.display()
                    )
                } else {
                    format!("No active Kaku source line in {}", shell_config.display())
                }
            }
            ShellSourceCheck::Fish(check) => {
                if let Some(err) = &check.read_error {
                    format!("Could not read {}: {}", shell_config.display(), err)
                } else if check.missing_file {
                    format!("No {} file found at {}", shell_config_label(target_shell), shell_config.display())
                } else if check.has_active_lines() {
                    format!("Found {} active Kaku source line(s) in {}", check.active_lines, shell_config.display())
                } else {
                    format!("No active Kaku source line in {}", shell_config.display())
                }
            }
        },
        details: match &source_check {
            ShellSourceCheck::Zsh(check) => check.details(&shell_config),
            ShellSourceCheck::Fish(check) => check.details(),
        },
        fix: match &source_check {
            ShellSourceCheck::Zsh(check) => {
                if check.read_error.is_some() {
                    Some(format!(
                        "Fix permissions or path access for {} then run `kaku doctor` again",
                        shell_config.display()
                    ))
                } else if check.has_active_lines() && !check.has_legacy_guarded_lines() {
                    None
                } else {
                    Some("Run `kaku init --update-only`".to_string())
                }
            }
            ShellSourceCheck::Fish(check) => {
                if check.read_error.is_some() {
                    Some(format!(
                        "Fix permissions or path access for {} then run `kaku doctor` again",
                        shell_config.display()
                    ))
                } else if check.has_active_lines() {
                    None
                } else {
                    Some("Run `kaku init --update-only`".to_string())
                }
            }
        },
    });

    DoctorGroup {
        title: "Shell Integration",
        status: group_status(&checks),
        checks,
    }
}

fn build_runtime_group() -> DoctorGroup {
    let mut checks = Vec::new();

    let candidates = kaku_bin_candidates();
    let existing = candidates
        .iter()
        .find(|p| config::is_executable_file(p))
        .cloned();
    checks.push(DoctorCheck {
        title: "Kaku App Binary",
        status: if existing.is_some() {
            DoctorStatus::Ok
        } else {
            DoctorStatus::Fail
        },
        summary: match &existing {
            Some(path) => format!("Found executable {}", path.display()),
            None => "Kaku CLI binary not found in known locations".to_string(),
        },
        details: candidates
            .iter()
            .map(|p| format!("Checked {}", p.display()))
            .collect(),
        fix: if existing.is_some() {
            None
        } else {
            Some("Install Kaku.app to /Applications or ~/Applications".to_string())
        },
    });

    let target_shell = detect_target_shell();
    let wrapper = managed_wrapper_path(target_shell);
    let wrapper_probe = probe_wrapper(&wrapper);
    checks.push(DoctorCheck {
        title: "Wrapper Execution Probe",
        status: wrapper_probe.status,
        summary: wrapper_probe.summary,
        details: wrapper_probe.details,
        fix: wrapper_probe.fix,
    });

    let login_shell_probe = probe_login_shell_integration();
    checks.push(DoctorCheck {
        title: "Login Shell Integration Probe",
        status: login_shell_probe.status,
        summary: login_shell_probe.summary,
        details: login_shell_probe.details,
        fix: login_shell_probe.fix,
    });

    DoctorGroup {
        title: "Runtime",
        status: group_status(&checks),
        checks,
    }
}

enum ShellSourceCheck {
    Zsh(ZshrcSourceCheck),
    Fish(FishConfigCheck),
}

#[derive(Default)]
struct FishConfigCheck {
    active_lines: usize,
    read_error: Option<String>,
    missing_file: bool,
}

impl FishConfigCheck {
    fn has_active_lines(&self) -> bool {
        self.active_lines > 0
    }

    fn details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some(err) = &self.read_error {
            details.push(format!("Read error: {}", err));
            return details;
        }
        if self.missing_file {
            details.push("fish conf.d entry point does not exist yet".to_string());
        }
        if self.active_lines > 0 {
            details.push(format!("Active Kaku source lines: {}", self.active_lines));
        } else {
            details.push(
                "Expected an active line that sources ~/.config/kaku/fish/kaku.fish".to_string(),
            );
        }
        details
    }
}

fn check_fish_config_line(config_path: &Path) -> FishConfigCheck {
    let mut result = FishConfigCheck::default();
    let content = match fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                result.missing_file = true;
            } else {
                result.read_error = Some(err.to_string());
            }
            return result;
        }
    };

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.contains("kaku/fish/kaku.fish") {
            result.active_lines += 1;
        }
    }

    result
}

fn detect_target_shell() -> KakuShell {
    if let Some(shell) = std::env::var("KAKU_TARGET_SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
    {
        return shell_from_candidate_path(&shell);
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    shell_from_candidate_path(&shell)
}

fn shell_from_candidate_path(shell: &str) -> KakuShell {
    let file = Path::new(shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(shell);
    let file = file.to_lowercase();
    if file.ends_with("fish") || file == "fish" {
        KakuShell::Fish
    } else {
        KakuShell::Zsh
    }
}

fn shell_name_if_known(shell: KakuShell) -> &'static str {
    match shell {
        KakuShell::Fish => "fish",
        KakuShell::Zsh => "zsh",
    }
}

fn shell_exec_command(shell: KakuShell) -> &'static str {
    match shell {
        KakuShell::Fish => "exec fish",
        KakuShell::Zsh => "exec zsh",
    }
}

fn shell_config_label(shell: KakuShell) -> &'static str {
    match shell {
        KakuShell::Fish => "fish conf.d entry",
        KakuShell::Zsh => "zshrc",
    }
}

fn shell_config_path(shell: KakuShell) -> PathBuf {
    match shell {
        KakuShell::Fish => fish_config_path(),
        KakuShell::Zsh => zshrc_path(),
    }
}

fn fish_config_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("fish")
        .join("conf.d")
        .join("kaku.fish")
}

fn group_status(checks: &[DoctorCheck]) -> DoctorStatus {
    checks
        .iter()
        .map(|c| c.status)
        .max_by_key(|s| s.severity_rank())
        .unwrap_or(DoctorStatus::Info)
}

fn render_text_report(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("Kaku Doctor\n");
    out.push_str(&format!(
        "Status: {} {}\n",
        report.overall_status.icon(),
        report.overall_status.label()
    ));
    out.push_str(&format!(
        "Summary: {} ok  {} warn  {} fail  {} info\n",
        report.summary.ok, report.summary.warn, report.summary.fail, report.summary.info
    ));
    out.push('\n');

    for (group_idx, group) in report.groups.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} [{}]\n",
            group_idx + 1,
            group.title,
            group.status.label()
        ));
        for check in &group.checks {
            out.push_str(&format!(
                "  - {} {}: {}\n",
                check.status.icon(),
                check.title,
                check.summary
            ));
            for detail in &check.details {
                out.push_str(&format!("    - {}\n", detail));
            }
            if let Some(fix) = &check.fix {
                out.push_str(&format!("    Fix: {}\n", fix));
            }
        }
        out.push('\n');
    }

    out
}

#[derive(Default)]
struct ZshrcSourceCheck {
    guarded_active_lines: usize,
    unguarded_active_lines: usize,
    malformed_escaped_path_lines: usize,
    read_error: Option<String>,
    missing_file: bool,
    commented_example: bool,
}

impl ZshrcSourceCheck {
    fn has_active_lines(&self) -> bool {
        self.guarded_active_lines + self.unguarded_active_lines > 0
    }

    fn has_legacy_guarded_lines(&self) -> bool {
        self.guarded_active_lines > 0
    }

    fn details(&self, zshrc: &Path) -> Vec<String> {
        let mut details = Vec::new();
        details.push(format!("Checked {}", zshrc.display()));
        if let Some(err) = &self.read_error {
            details.push(format!("Read error: {}", err));
            return details;
        }
        if self.missing_file {
            details.push("zshrc does not exist yet".to_string());
        }
        if !self.has_active_lines() {
            details.push(
                "Expected an active line that sources ~/.config/kaku/zsh/kaku.zsh".to_string(),
            );
        } else {
            details.push(format!(
                "Active source lines: {} total",
                self.guarded_active_lines + self.unguarded_active_lines
            ));
        }
        if self.has_legacy_guarded_lines() {
            details.push(
                "Found older Kaku-specific guarded source line variants; `kaku init --update-only` will normalize them."
                    .to_string(),
            );
        }
        if self.malformed_escaped_path_lines > 0 {
            details.push(format!(
                "Found {} malformed Kaku source line(s) with escaped absolute paths (for example, \"\\/Users/...\"), which prevents loading kaku.zsh",
                self.malformed_escaped_path_lines
            ));
        }
        if self.commented_example {
            details.push("Found a commented Kaku source line".to_string());
        }
        details
    }
}

fn check_zshrc_source_line(zshrc: &Path) -> ZshrcSourceCheck {
    let mut result = ZshrcSourceCheck::default();
    let content = match fs::read_to_string(zshrc) {
        Ok(content) => content,
        Err(err) => {
            if err.kind() == ErrorKind::NotFound {
                result.missing_file = true;
            } else {
                result.read_error = Some(err.to_string());
            }
            return result;
        }
    };

    // This is intentionally heuristic instead of a strict parser so it can
    // recognize both the current managed source line and older variants that
    // users may have edited manually.
    //
    // Scan all lines instead of stopping at the first match: mixed managed and
    // legacy lines can coexist during migration, and doctor should report
    // the remaining risk until legacy guarded variants are removed.
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') && trimmed.contains("kaku/zsh/kaku.zsh") {
            result.commented_example = true;
            continue;
        }
        if is_malformed_escaped_kaku_source_line(trimmed) {
            result.malformed_escaped_path_lines += 1;
            continue;
        }
        if is_active_kaku_source_line(trimmed) {
            if is_legacy_guarded_kaku_source_line(trimmed) {
                result.guarded_active_lines += 1;
            } else {
                result.unguarded_active_lines += 1;
            }
        }
    }
    result
}

fn contains_source_command(line: &str) -> bool {
    line.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .any(|token| token == "source")
}

fn is_active_kaku_source_line(trimmed_line: &str) -> bool {
    if trimmed_line.starts_with('#') || !trimmed_line.contains("kaku/zsh/kaku.zsh") {
        return false;
    }
    contains_source_command(trimmed_line)
}

fn is_legacy_guarded_kaku_source_line(trimmed_line: &str) -> bool {
    if !is_active_kaku_source_line(trimmed_line) {
        return false;
    }

    let compact = trimmed_line
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();

    let has_kaku_comparison = compact.contains("==\"kaku\"")
        || compact.contains("=='kaku'")
        || compact.contains("==kaku")
        || compact.contains("=\"kaku\"")
        || compact.contains("='kaku'")
        || compact.contains("=kaku");
    let has_term_guard =
        (compact.contains("${term") || compact.contains("$term")) && has_kaku_comparison;
    let has_term_program_guard = compact.contains("term_program") && has_kaku_comparison;

    compact.contains("wezterm_pane") || has_term_program_guard || has_term_guard
}

fn is_malformed_escaped_kaku_source_line(trimmed_line: &str) -> bool {
    if trimmed_line.starts_with('#') || !trimmed_line.contains("kaku/zsh/kaku.zsh") {
        return false;
    }
    if !contains_source_command(trimmed_line) {
        return false;
    }
    trimmed_line.contains("\"\\/") || trimmed_line.contains("'\\/")
}

fn probe_wrapper(wrapper: &Path) -> DoctorCheck {
    fn wrapper_check(
        status: DoctorStatus,
        summary: String,
        details: Vec<String>,
        fix: Option<String>,
    ) -> DoctorCheck {
        DoctorCheck {
            title: "Wrapper Execution Probe",
            status,
            summary,
            details,
            fix,
        }
    }

    if !wrapper.is_file() {
        return wrapper_check(
            DoctorStatus::Fail,
            format!(
                "Skipped probe because wrapper is missing: {}",
                wrapper.display()
            ),
            vec!["Generate the wrapper first with `kaku init`".to_string()],
            Some("Run `kaku init --update-only`".to_string()),
        );
    }

    // Spawn the child and poll with try_wait so we can kill it cleanly on timeout.
    // Using spawn()+try_wait() avoids the thread-leak that Command::output() in a
    // background thread would cause if the child never exits.
    let mut child = match Command::new(wrapper)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            return wrapper_check(
                DoctorStatus::Fail,
                format!("Failed to execute wrapper: {}", err),
                vec![format!("Command: {} --version", wrapper.display())],
                Some("Restore wrapper permissions or rerun `kaku init --update-only`".to_string()),
            );
        }
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return wrapper_check(
                    DoctorStatus::Warn,
                    "Wrapper probe timed out after 5 seconds".to_string(),
                    vec![
                        format!("Command: {} --version", wrapper.display()),
                        "The wrapper script did not exit within the time limit.".to_string(),
                    ],
                    Some(
                        "Check that the kaku binary is accessible and not blocked by network or permission issues".to_string(),
                    ),
                );
            }
            Err(err) => break Err(err),
        }
    };

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            wrapper_check(
                DoctorStatus::Ok,
                "Wrapper can launch Kaku binary".to_string(),
                if stdout.is_empty() {
                    vec![format!(
                        "Command succeeded: {} --version",
                        wrapper.display()
                    )]
                } else {
                    vec![
                        format!("Command succeeded: {} --version", wrapper.display()),
                        format!("Output: {}", stdout),
                    ]
                },
                None,
            )
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let mut details = vec![format!("Command: {} --version", wrapper.display())];
            if !stderr.is_empty() {
                details.push(format!("stderr: {}", stderr));
            }
            wrapper_check(
                DoctorStatus::Fail,
                format!("Wrapper exited with status {}", output.status),
                details,
                Some("Check Kaku.app location then run `kaku init --update-only`".to_string()),
            )
        }
        Err(err) => wrapper_check(
            DoctorStatus::Fail,
            format!("Failed to execute wrapper: {}", err),
            vec![format!("Command: {} --version", wrapper.display())],
            Some("Restore wrapper permissions or rerun `kaku init --update-only`".to_string()),
        ),
    }
}

fn probe_login_shell_integration() -> DoctorCheck {
    match detect_target_shell() {
        KakuShell::Fish => probe_login_fish_integration(),
        KakuShell::Zsh => probe_login_zsh_integration(),
    }
}

fn probe_login_zsh_integration() -> DoctorCheck {
    DoctorCheck {
        title: "Login Zsh Integration Probe",
        status: DoctorStatus::Info,
        summary: "Skipped interactive login zsh probe to avoid shell side effects".to_string(),
        details: vec![
            "Doctor does not execute `/bin/zsh -lic` because interactive login startup files can run user-defined commands, plugin managers, and network actions.".to_string(),
            "Use `exec zsh -l` manually and verify `echo $KAKU_ZSH_DIR` if you need end-to-end runtime validation.".to_string(),
        ],
        fix: None,
    }
}

fn probe_login_fish_integration() -> DoctorCheck {
    DoctorCheck {
        title: "Login Fish Integration Probe",
        status: DoctorStatus::Info,
        summary: "Skipped interactive login fish probe to avoid shell side effects".to_string(),
        details: vec![
            "Doctor does not execute `/bin/fish -l` because interactive login startup files can run user-defined commands, plugin managers, and network actions.".to_string(),
            "Use `exec fish -l` manually and verify `echo $KAKU_SHELL` if you need end-to-end runtime validation.".to_string(),
        ],
        fix: None,
    }
}

fn home_dir() -> PathBuf {
    config::HOME_DIR.clone()
}

fn managed_bin_dir(shell: KakuShell) -> PathBuf {
    home_dir()
        .join(".config")
        .join("kaku")
        .join(match shell {
            KakuShell::Fish => "fish",
            KakuShell::Zsh => "zsh",
        })
        .join("bin")
}

fn managed_wrapper_path(shell: KakuShell) -> PathBuf {
    managed_bin_dir(shell).join("kaku")
}

fn managed_init_file(shell: KakuShell) -> PathBuf {
    home_dir()
        .join(".config")
        .join("kaku")
        .join(match shell {
            KakuShell::Fish => "fish",
            KakuShell::Zsh => "zsh",
        })
        .join(match shell {
            KakuShell::Fish => "kaku.fish",
            KakuShell::Zsh => "kaku.zsh",
        })
}

fn zshrc_path() -> PathBuf {
    if let Some(zdotdir) = std::env::var_os("ZDOTDIR") {
        PathBuf::from(zdotdir).join(".zshrc")
    } else {
        home_dir().join(".zshrc")
    }
}

fn kaku_bin_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = std::env::var_os("KAKU_BIN") {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(exe) = std::env::current_exe() {
        if exe
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("kaku"))
            .unwrap_or(false)
        {
            candidates.push(exe);
        }
    }

    candidates.push(PathBuf::from("/Applications/Kaku.app/Contents/MacOS/kaku"));
    candidates.push(
        home_dir()
            .join("Applications")
            .join("Kaku.app")
            .join("Contents")
            .join("MacOS")
            .join("kaku"),
    );

    candidates
}

fn doctor_version_string() -> String {
    let version = config::wezterm_version();
    if version == "someone forgot to call assign_version_info" {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        version.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_zshrc_is_not_read_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join(".zshrc");

        let check = check_zshrc_source_line(&path);
        assert!(check.missing_file);
        assert!(check.read_error.is_none());
        assert!(!check.has_active_lines());
    }

    #[test]
    fn variable_assignment_is_not_detected_as_source_line() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join(".zshrc");
        fs::write(
            &path,
            r#"export KAKU_ZSH_INIT="$HOME/.config/kaku/zsh/kaku.zsh""#,
        )
        .expect("write zshrc");

        let check = check_zshrc_source_line(&path);
        assert_eq!(check.guarded_active_lines, 0);
        assert_eq!(check.unguarded_active_lines, 0);
    }

    #[test]
    fn counts_guarded_and_unguarded_source_lines() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join(".zshrc");
        fs::write(
            &path,
            r#"[[ "${TERM:-}" == "kaku" ]] && source "$HOME/.config/kaku/zsh/kaku.zsh"
[[ -f "$HOME/.config/kaku/zsh/kaku.zsh" ]] && source "$HOME/.config/kaku/zsh/kaku.zsh"
[[ -f "$HOME/.config/kaku/zsh/kaku.zsh" ]] && source "$HOME/.config/kaku/zsh/kaku.zsh"
"#,
        )
        .expect("write zshrc");

        let check = check_zshrc_source_line(&path);
        assert!(!check.commented_example);
        assert_eq!(check.unguarded_active_lines, 2);
        assert_eq!(check.guarded_active_lines, 1);
        assert!(check.has_legacy_guarded_lines());
        assert!(check.has_active_lines());
    }

    #[test]
    fn term_program_guard_is_detected_as_legacy_source_line() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join(".zshrc");
        fs::write(
            &path,
            r#"[[ "$TERM_PROGRAM" == "Kaku" ]] && [[ -f "$HOME/.config/kaku/zsh/kaku.zsh" ]] && source "$HOME/.config/kaku/zsh/kaku.zsh" # Kaku Shell Integration
"#,
        )
        .expect("write zshrc");

        let check = check_zshrc_source_line(&path);
        assert_eq!(check.guarded_active_lines, 1);
        assert_eq!(check.unguarded_active_lines, 0);
        assert!(check.has_legacy_guarded_lines());
        assert!(check.has_active_lines());
    }

    #[test]
    fn escaped_absolute_path_source_line_is_marked_malformed() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join(".zshrc");
        fs::write(
            &path,
            r#"[[ -f "\/Users/lex/.config/kaku/zsh/kaku.zsh" ]] && source "\/Users/lex/.config/kaku/zsh/kaku.zsh""#,
        )
        .expect("write zshrc");

        let check = check_zshrc_source_line(&path);
        assert_eq!(check.guarded_active_lines, 0);
        assert_eq!(check.unguarded_active_lines, 0);
        assert_eq!(check.malformed_escaped_path_lines, 1);
        assert!(!check.has_active_lines());
    }

    #[test]
    fn login_probe_is_passive_and_non_executing() {
        let check = probe_login_zsh_integration();
        assert_eq!(check.status, DoctorStatus::Info);
        assert!(check
            .summary
            .contains("Skipped interactive login zsh probe"));
        assert!(check.fix.is_none());
    }
}
