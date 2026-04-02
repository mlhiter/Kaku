use crate::spawn::SpawnWhere;
use config::keyassignment::SpawnCommand;

const SHORTCUT_HELP_TEXT: &str = r#"Kaku Keyboard Shortcuts

Sidebar
  Cmd+Shift+/           Toggle Shortcuts Help
  Cmd+Shift+B           Toggle Sidebar
  Cmd+Opt+N / Cmd+Opt+T New Project / New Session
  Cmd+Opt+P / Cmd+Opt+R Pin/Unpin / Rename Session
  Cmd+Opt+Backspace     Delete Current Session

Tabs & Window
  Cmd+N / Cmd+T         New Window / New Tab
  Cmd+W / Cmd+Shift+W   Close Pane/Tab / Close Tab
  Cmd+Shift+[ / ]       Previous / Next Tab
  Cmd+1..9              Switch to Tab 1..9
  Cmd+Ctrl+F            Toggle Fullscreen
  Cmd+H / Cmd+M         Hide App / Minimize

Pane & Tools
  Cmd+D / Cmd+Shift+D   Split Vertical / Horizontal
  Cmd+Opt+Arrow         Focus Neighbor Pane
  Cmd+Ctrl+Arrow        Resize Pane
  Cmd+Shift+Enter / S   Zoom / Toggle Split Direction
  Cmd+Shift+A / E       AI Config / Apply Last Suggestion
  Cmd+Shift+G / Y / R   Lazygit / Yazi / Remote Files

Editing
  Cmd+K / Cmd+R         Clear Screen + Scrollback
  Cmd+Enter / Shift+Enter Insert Newline
  Cmd+Backspace / Opt+Backspace Delete Line / Prev Word

Close this help tab: press q in less, then close tab (Cmd+W)
"#;

fn shortcuts_help_shell_script() -> String {
    format!(
        r#"printf '\033]0;Kaku Shortcuts\007'
if command -v less >/dev/null 2>&1; then
  cat <<'KAKU_SHORTCUTS' | less -R
{help_text}
KAKU_SHORTCUTS
else
  cat <<'KAKU_SHORTCUTS'
{help_text}
KAKU_SHORTCUTS
  printf '\nPress Enter to close...'
  IFS= read -r _
fi
"#,
        help_text = SHORTCUT_HELP_TEXT
    )
}

impl crate::TermWindow {
    pub(crate) fn open_shortcuts_help_tab(&mut self) {
        let spawn = SpawnCommand {
            args: Some(vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                shortcuts_help_shell_script(),
            ]),
            ..SpawnCommand::default()
        };
        self.spawn_command(&spawn, SpawnWhere::NewTab);
    }
}
