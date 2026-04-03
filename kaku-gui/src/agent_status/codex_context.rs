pub(crate) fn is_codex_like_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    for token in trimmed.split_whitespace() {
        if token.contains('=') && !token.starts_with('/') && !token.starts_with("./") {
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
        return executable == "codex";
    }

    false
}

pub(crate) fn is_codex_process_name(process_name: &str) -> bool {
    process_name
        .rsplit('/')
        .next()
        .is_some_and(|name| name.trim() == "codex")
}

#[cfg(test)]
mod tests {
    use super::{is_codex_like_command, is_codex_process_name};

    #[test]
    fn detects_codex_like_wrapped_commands() {
        assert!(is_codex_like_command("codex run"));
        assert!(is_codex_like_command("/usr/local/bin/codex run"));
        assert!(is_codex_like_command("FOO=bar codex run"));
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
        assert!(!is_codex_process_name("bash"));
    }
}
