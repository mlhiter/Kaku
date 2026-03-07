use anyhow::Context;
use std::io::Write;
use std::path::Path;

pub fn is_jsonc_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonc"))
}

/// Parses JSON or JSONC text.
///
/// This supports comments and trailing commas, then returns standard JSON data.
pub fn parse_json_or_jsonc(input: &str) -> serde_json::Result<serde_json::Value> {
    serde_json::from_str(input).or_else(|_| {
        let stripped = strip_jsonc_comments(input);
        let normalized = strip_jsonc_trailing_commas(&stripped);
        serde_json::from_str(&normalized)
    })
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .context("atomic write requires a parent directory")?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("tempfile {}", path.display()))?;

    if let Ok(meta) = std::fs::metadata(path) {
        let _ = temp.as_file().set_permissions(meta.permissions());
    }

    temp.write_all(contents)
        .with_context(|| format!("write temp file for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync temp file for {}", path.display()))?;

    temp.persist(path)
        .map_err(|e| anyhow::Error::from(e.error))
        .with_context(|| format!("persist {}", path.display()))?;

    Ok(())
}

/// Strips JSONC (JSON with Comments) comments from the input string.
/// Handles both single-line (//) and multi-line (/* */) comments,
/// while preserving comments inside string literals.
pub fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.peek() {
                    out.push(next);
                    chars.next();
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '/' {
            if let Some(&next) = chars.peek() {
                if next == '/' {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        match ch {
                            '\n' => {
                                out.push('\n');
                                break;
                            }
                            '\r' => {
                                out.push('\r');
                                if chars.peek() == Some(&'\n') {
                                    out.push('\n');
                                    chars.next();
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                if next == '*' {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            break;
                        }
                    }
                    continue;
                }
            }
        }

        out.push(c);
    }

    out
}

fn strip_jsonc_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;

    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                i += 1;
                out.push(chars[i]);
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }

        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && matches!(chars[j], ']' | '}') {
                i += 1;
                continue;
            }
        }

        out.push(c);
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn strips_comments_but_keeps_comment_like_strings() {
        let input = r#"{
  "url": "https://example.com/a//b",
  "pattern": "/* keep me */",
  // remove me
  "v": 1
}"#;
        let stripped = strip_jsonc_comments(input);
        assert!(stripped.contains("https://example.com/a//b"));
        assert!(stripped.contains("/* keep me */"));
        assert!(!stripped.contains("// remove me"));
    }

    #[test]
    fn preserves_crlf_when_stripping_line_comments() {
        let input = "{\r\n  // c\r\n  \"a\": 1\r\n}\r\n";
        let stripped = strip_jsonc_comments(input);
        assert_eq!(stripped, "{\r\n  \r\n  \"a\": 1\r\n}\r\n");
    }

    #[test]
    fn parses_jsonc_with_comments_and_trailing_commas() {
        let input = r#"{
  // comment
  "items": [
    1,
    2,
  ],
  "obj": {
    "a": 1,
  },
}"#;
        let parsed = parse_json_or_jsonc(input).expect("parse jsonc");
        assert_eq!(parsed["items"], json!([1, 2]));
        assert_eq!(parsed["obj"]["a"], json!(1));
    }

    #[test]
    fn handles_eof_line_comment() {
        let input = "{ \"a\": 1 } // eof";
        let parsed = parse_json_or_jsonc(input).expect("parse jsonc");
        assert_eq!(parsed["a"], json!(1));
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("config.json");

        std::fs::write(&path, br#"{"a":1}"#).expect("seed");
        write_atomic(&path, br#"{"a":2}"#).expect("write atomic");

        let saved = std::fs::read_to_string(&path).expect("read");
        assert_eq!(saved, r#"{"a":2}"#);
    }
}
