pub(crate) fn is_codex_like_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let mut tokens = trimmed
        .split_whitespace()
        .filter(|token| !token.trim().is_empty());
    let mut saw_runtime_prefix = false;
    for token in &mut tokens {
        if token.contains('=') && !token.starts_with('/') && !token.starts_with("./") {
            continue;
        }
        if matches!(
            token,
            "node" | "nodejs" | "bun" | "deno" | "pnpm" | "pnpx" | "npx" | "yarn" | "npm"
        ) {
            saw_runtime_prefix = true;
            continue;
        }
        if matches!(
            token,
            "command" | "builtin" | "noglob" | "nocorrect" | "time" | "env"
        ) {
            continue;
        }
        if token.starts_with('-') {
            continue;
        }

        let executable = token.rsplit('/').next().unwrap_or(token);
        if executable == "codex" {
            return true;
        }
        if saw_runtime_prefix && executable.starts_with("codex") {
            return true;
        }
        return false;
    }

    false
}

pub(crate) fn is_codex_process_name(process_name: &str) -> bool {
    let leaf = process_name
        .rsplit('/')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    if leaf == "codex" {
        return true;
    }
    if matches!(leaf, "node" | "nodejs" | "bun" | "deno") {
        let lower = process_name.to_ascii_lowercase();
        return lower.contains("codex");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_codex_like_command, is_codex_process_name};

    #[test]
    fn detects_codex_like_wrapped_commands() {
        assert!(is_codex_like_command("codex run"));
        assert!(is_codex_like_command("/usr/local/bin/codex run"));
        assert!(is_codex_like_command("FOO=bar codex run"));
        assert!(is_codex_like_command("node codex"));
        assert!(is_codex_like_command("bun codex"));
        assert!(is_codex_like_command("command codex run"));
        assert!(is_codex_like_command("builtin codex run"));
        assert!(is_codex_like_command("nocorrect codex run"));
        assert!(is_codex_like_command("time codex run"));
        assert!(is_codex_like_command("env FOO=bar codex run"));
    }

    #[test]
    fn ignores_non_codex_commands() {
        assert!(!is_codex_like_command("claude --print"));
        assert!(!is_codex_like_command("echo codex"));
        assert!(!is_codex_like_command(""));
    }

    #[test]
    fn detects_codex_process_name() {
        assert!(is_codex_process_name("codex"));
        assert!(is_codex_process_name("/usr/local/bin/codex"));
        assert!(!is_codex_process_name("node"));
        assert!(!is_codex_process_name("bash"));
    }
}
