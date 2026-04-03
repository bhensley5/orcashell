//! Shell integration for OSC 133 semantic prompt markers.
//!
//! Embeds zsh/bash scripts at compile time and writes them to a temporary
//! directory at runtime so that spawned shells can source them.

use std::fs;
use std::path::{Path, PathBuf};

/// Zsh integration script content.
pub const ZSH_INTEGRATION: &str = include_str!("../shell-integration/orcashell.zsh");

/// Bash integration script content.
pub const BASH_INTEGRATION: &str = include_str!("../shell-integration/orcashell.bash");

/// PowerShell integration script content.
pub const POWERSHELL_INTEGRATION: &str = include_str!("../shell-integration/orcashell.ps1");

/// Detected shell type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellType {
    Zsh,
    Bash,
    PowerShellCore,
    WindowsPowerShell,
    Cmd,
    Unknown,
}

/// Determine shell type from a shell path or name.
///
/// Handles both `/` and `\` path separators so that Windows paths like
/// `C:\Windows\System32\cmd.exe` are parsed correctly on any host OS.
/// Matching is case-insensitive and strips `.exe` suffixes.
pub fn shell_type(shell: &str) -> ShellType {
    // Split on both / and \ to handle Windows paths on any host OS.
    let basename = shell.rsplit(['/', '\\']).next().unwrap_or("");
    // Case-insensitive comparison; strip .exe suffix.
    let lower = basename.to_ascii_lowercase();
    let name = lower.strip_suffix(".exe").unwrap_or(&lower);
    match name {
        "zsh" => ShellType::Zsh,
        "bash" => ShellType::Bash,
        "pwsh" => ShellType::PowerShellCore,
        "powershell" => ShellType::WindowsPowerShell,
        "cmd" => ShellType::Cmd,
        _ => ShellType::Unknown,
    }
}

/// Resolve the shell binary path from an optional user override, environment,
/// or platform defaults.
///
/// Precedence:
/// 1. Explicit `shell_override` (non-empty)
/// 2. `$SHELL` environment variable (Unix convention)
/// 3. Platform fallback:
///    - macOS: `/bin/zsh`
///    - Windows: `pwsh.exe` → `powershell.exe` → `cmd.exe` (first found)
///    - Other Unix: `/bin/bash`
pub fn resolve_shell_path(shell_override: Option<&str>) -> String {
    if let Some(s) = shell_override.filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return shell;
        }
    }
    platform_default_shell()
}

#[cfg(target_os = "macos")]
fn platform_default_shell() -> String {
    "/bin/zsh".to_string()
}

#[cfg(windows)]
fn platform_default_shell() -> String {
    // Prefer PowerShell Core, then Windows PowerShell, then CMD.
    // Use a PATH scan instead of probe-spawning, which avoids loading the
    // PowerShell runtime (~300-800ms) just to check if the binary exists.
    for candidate in &["pwsh.exe", "powershell.exe"] {
        if find_on_path(candidate) {
            return candidate.to_string();
        }
    }
    "cmd.exe".to_string()
}

/// Check whether an executable name exists on the system PATH.
#[cfg(windows)]
fn find_on_path(exe: &str) -> bool {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(exe).is_file() {
                return true;
            }
        }
    }
    false
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_default_shell() -> String {
    "/bin/bash".to_string()
}

/// Quote a file path for safe insertion into a shell command line.
///
/// The quoting strategy varies by shell type:
/// - Bash/Zsh/Unknown: single-quote, escape embedded `'` as `'\''`
/// - PowerShell: single-quote, double embedded `'` as `''`
/// - CMD: double-quote (known limitation: `%VAR%` and `!` may still expand)
pub fn quote_path_for_shell(path: &Path, shell: ShellType) -> String {
    let path_str = path.to_string_lossy();
    match shell {
        ShellType::Zsh | ShellType::Bash | ShellType::Unknown => {
            format!("'{}'", path_str.replace('\'', "'\\''"))
        }
        ShellType::PowerShellCore | ShellType::WindowsPowerShell => {
            format!("'{}'", path_str.replace('\'', "''"))
        }
        ShellType::Cmd => {
            // Double-quote for CMD. Known limitation: %VAR% still expands
            // (env var substitution), and ! expands under delayed expansion.
            // Full CMD escaping is out of scope for Phase 9's basic CMD support.
            format!("\"{}\"", path_str)
        }
    }
}

fn write_integration_dir(dir: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(dir)?;

    fs::write(dir.join("orcashell.zsh"), ZSH_INTEGRATION)?;
    fs::write(dir.join("orcashell.bash"), BASH_INTEGRATION)?;
    fs::write(dir.join("orcashell.ps1"), POWERSHELL_INTEGRATION)?;

    let zsh_script = quote_path_for_shell(&dir.join("orcashell.zsh"), ShellType::Zsh);
    let bash_script = quote_path_for_shell(&dir.join("orcashell.bash"), ShellType::Bash);

    let zshenv = "\
# OrcaShell Zsh integration loader\n\
export ORCASHELL_WRAPPER_ZDOTDIR=\"$ZDOTDIR\"\n\
__orcashell_real_zdotdir=\"${ORCASHELL_REAL_ZDOTDIR:-$HOME}\"\n\
if [[ -n \"$__orcashell_real_zdotdir\" && \"$__orcashell_real_zdotdir\" != \"$ORCASHELL_WRAPPER_ZDOTDIR\" ]]; then\n\
    export ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
    [[ -f \"$ZDOTDIR/.zshenv\" ]] && source \"$ZDOTDIR/.zshenv\"\n\
    export ORCASHELL_EFFECTIVE_ZDOTDIR=\"${ZDOTDIR:-$__orcashell_real_zdotdir}\"\n\
else\n\
    export ORCASHELL_EFFECTIVE_ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
fi\n\
export ZDOTDIR=\"$ORCASHELL_WRAPPER_ZDOTDIR\"\n";
    fs::write(dir.join(".zshenv"), zshenv)?;

    let zshprofile = "\
# OrcaShell Zsh profile wrapper\n\
__orcashell_real_zdotdir=\"${ORCASHELL_EFFECTIVE_ZDOTDIR:-${ORCASHELL_REAL_ZDOTDIR:-$HOME}}\"\n\
if [[ -n \"$__orcashell_real_zdotdir\" && \"$__orcashell_real_zdotdir\" != \"${ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}\" ]]; then\n\
    export ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
    [[ -f \"$ZDOTDIR/.zprofile\" ]] && source \"$ZDOTDIR/.zprofile\"\n\
fi\n\
export ZDOTDIR=\"${ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}\"\n";
    fs::write(dir.join(".zprofile"), zshprofile)?;

    let zshrc = format!(
        "\
# OrcaShell Zsh rc wrapper\n\
__orcashell_real_zdotdir=\"${{ORCASHELL_EFFECTIVE_ZDOTDIR:-${{ORCASHELL_REAL_ZDOTDIR:-$HOME}}}}\"\n\
if [[ -n \"$__orcashell_real_zdotdir\" && \"$__orcashell_real_zdotdir\" != \"${{ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}}\" ]]; then\n\
    export ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
    [[ -f \"$ZDOTDIR/.zshrc\" ]] && source \"$ZDOTDIR/.zshrc\"\n\
fi\n\
export ZDOTDIR=\"${{ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}}\"\n\
[[ -f {zsh_script} ]] && source {zsh_script}\n"
    );
    fs::write(dir.join(".zshrc"), zshrc)?;

    let zshlogin = "\
# OrcaShell Zsh login wrapper\n\
__orcashell_real_zdotdir=\"${ORCASHELL_EFFECTIVE_ZDOTDIR:-${ORCASHELL_REAL_ZDOTDIR:-$HOME}}\"\n\
if [[ -n \"$__orcashell_real_zdotdir\" && \"$__orcashell_real_zdotdir\" != \"${ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}\" ]]; then\n\
    export ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
    [[ -f \"$ZDOTDIR/.zlogin\" ]] && source \"$ZDOTDIR/.zlogin\"\n\
fi\n\
export ZDOTDIR=\"${ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}\"\n";
    fs::write(dir.join(".zlogin"), zshlogin)?;

    let zshlogout = "\
# OrcaShell Zsh logout wrapper\n\
__orcashell_real_zdotdir=\"${ORCASHELL_EFFECTIVE_ZDOTDIR:-${ORCASHELL_REAL_ZDOTDIR:-$HOME}}\"\n\
if [[ -n \"$__orcashell_real_zdotdir\" && \"$__orcashell_real_zdotdir\" != \"${ORCASHELL_WRAPPER_ZDOTDIR:-$ZDOTDIR}\" ]]; then\n\
    export ZDOTDIR=\"$__orcashell_real_zdotdir\"\n\
    [[ -f \"$ZDOTDIR/.zlogout\" ]] && source \"$ZDOTDIR/.zlogout\"\n\
fi\n";
    fs::write(dir.join(".zlogout"), zshlogout)?;

    let bash_profile = format!(
        "\
# OrcaShell Bash login wrapper\n\
export ORCASHELL_WRAPPER_HOME=\"$HOME\"\n\
__orcashell_real_home=\"${{ORCASHELL_REAL_HOME:-$HOME}}\"\n\
if [[ -n \"$__orcashell_real_home\" && \"$__orcashell_real_home\" != \"$ORCASHELL_WRAPPER_HOME\" ]]; then\n\
    export HOME=\"$__orcashell_real_home\"\n\
    if [[ -f \"$HOME/.bash_profile\" ]]; then\n\
        source \"$HOME/.bash_profile\"\n\
    elif [[ -f \"$HOME/.bash_login\" ]]; then\n\
        source \"$HOME/.bash_login\"\n\
    elif [[ -f \"$HOME/.profile\" ]]; then\n\
        source \"$HOME/.profile\"\n\
    fi\n\
fi\n\
[[ -f {bash_script} ]] && source {bash_script}\n"
    );
    fs::write(dir.join(".bash_profile"), bash_profile)?;

    Ok(())
}

/// Write shell integration scripts to a temp directory and return the path.
///
/// Creates scripts under `$TMPDIR/orcashell-shell-integration/`.
/// Zsh uses proxy startup files so OrcaShell can wrap the user's normal
/// interactive startup instead of replacing it.
pub fn prepare_integration_dir() -> Result<PathBuf, std::io::Error> {
    let dir = std::env::temp_dir().join("orcashell-shell-integration");
    write_integration_dir(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("orcashell-shell-integration-{name}-{nonce}"))
    }

    #[test]
    fn scripts_are_embedded() {
        assert!(!ZSH_INTEGRATION.is_empty());
        assert!(!BASH_INTEGRATION.is_empty());
        assert!(!POWERSHELL_INTEGRATION.is_empty());
        assert!(ZSH_INTEGRATION.contains("133;A"));
        assert!(ZSH_INTEGRATION.contains("133;B"));
        assert!(ZSH_INTEGRATION.contains("133;C"));
        assert!(ZSH_INTEGRATION.contains("133;D"));
        assert!(ZSH_INTEGRATION.contains("]2;"));
        assert!(ZSH_INTEGRATION.contains("\\e[1;3D"));
        assert!(ZSH_INTEGRATION.contains("\\e[1;3C"));
        assert!(BASH_INTEGRATION.contains("133;A"));
        assert!(BASH_INTEGRATION.contains("133;B"));
        assert!(BASH_INTEGRATION.contains("133;C"));
        assert!(BASH_INTEGRATION.contains("133;D"));
        assert!(BASH_INTEGRATION.contains("]2;"));
        assert!(BASH_INTEGRATION.contains("\\e[1;3D"));
        assert!(BASH_INTEGRATION.contains("\\e[1;3C"));
        assert!(POWERSHELL_INTEGRATION.contains("133;A"));
        assert!(POWERSHELL_INTEGRATION.contains("133;B"));
        assert!(POWERSHELL_INTEGRATION.contains("133;C"));
        assert!(POWERSHELL_INTEGRATION.contains("133;D"));
        assert!(POWERSHELL_INTEGRATION.contains("]2;"));
    }

    #[test]
    fn shell_type_detection() {
        // Unix shells
        assert_eq!(shell_type("/bin/zsh"), ShellType::Zsh);
        assert_eq!(shell_type("/usr/local/bin/zsh"), ShellType::Zsh);
        assert_eq!(shell_type("zsh"), ShellType::Zsh);
        assert_eq!(shell_type("/bin/bash"), ShellType::Bash);
        assert_eq!(shell_type("/usr/local/bin/bash"), ShellType::Bash);
        assert_eq!(shell_type("bash"), ShellType::Bash);
        assert_eq!(shell_type("/bin/fish"), ShellType::Unknown);
        assert_eq!(shell_type("/bin/sh"), ShellType::Unknown);

        // PowerShell Core
        assert_eq!(shell_type("pwsh"), ShellType::PowerShellCore);
        assert_eq!(shell_type("pwsh.exe"), ShellType::PowerShellCore);
        assert_eq!(shell_type("PWSH.EXE"), ShellType::PowerShellCore);
        assert_eq!(
            shell_type("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
            ShellType::PowerShellCore
        );

        // Windows PowerShell
        assert_eq!(shell_type("powershell"), ShellType::WindowsPowerShell);
        assert_eq!(shell_type("powershell.exe"), ShellType::WindowsPowerShell);
        assert_eq!(shell_type("PowerShell.exe"), ShellType::WindowsPowerShell);
        assert_eq!(
            shell_type("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            ShellType::WindowsPowerShell
        );

        // CMD
        assert_eq!(shell_type("cmd"), ShellType::Cmd);
        assert_eq!(shell_type("cmd.exe"), ShellType::Cmd);
        assert_eq!(shell_type("CMD.EXE"), ShellType::Cmd);
        assert_eq!(shell_type("C:\\Windows\\System32\\cmd.exe"), ShellType::Cmd);

        // Case-insensitive Unix shells
        assert_eq!(shell_type("ZSH"), ShellType::Zsh);
        assert_eq!(shell_type("BASH"), ShellType::Bash);
    }

    #[test]
    fn prepare_integration_dir_creates_files() {
        let dir = unique_test_dir("prepare");
        write_integration_dir(&dir).unwrap();
        assert!(dir.join("orcashell.zsh").exists());
        assert!(dir.join("orcashell.bash").exists());
        assert!(dir.join("orcashell.ps1").exists());
        assert!(dir.join(".zshenv").exists());
        assert!(dir.join(".zprofile").exists());
        assert!(dir.join(".zshrc").exists());
        assert!(dir.join(".zlogin").exists());
        assert!(dir.join(".zlogout").exists());
        assert!(dir.join(".bash_profile").exists());

        let zshenv = fs::read_to_string(dir.join(".zshenv")).unwrap();
        assert!(zshenv.contains("ORCASHELL_REAL_ZDOTDIR"));
        assert!(zshenv.contains("ORCASHELL_EFFECTIVE_ZDOTDIR"));

        let zshprofile = fs::read_to_string(dir.join(".zprofile")).unwrap();
        assert!(zshprofile.contains(".zprofile"));

        let zshrc = fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(zshrc.contains("ORCASHELL_WRAPPER_ZDOTDIR"));
        assert!(zshrc.contains("orcashell.zsh"));

        let zshlogin = fs::read_to_string(dir.join(".zlogin")).unwrap();
        assert!(zshlogin.contains(".zlogin"));

        let zshlogout = fs::read_to_string(dir.join(".zlogout")).unwrap();
        assert!(zshlogout.contains(".zlogout"));

        let bash_profile = fs::read_to_string(dir.join(".bash_profile")).unwrap();
        assert!(bash_profile.contains("ORCASHELL_REAL_HOME"));
        assert!(bash_profile.contains("orcashell.bash"));

        let ps1 = fs::read_to_string(dir.join("orcashell.ps1")).unwrap();
        assert!(ps1.contains("__orcashell_integrated"));
    }

    #[cfg(unix)]
    #[test]
    fn zsh_wrapper_sources_user_login_and_interactive_files() {
        let zsh = match orcashell_platform::command("zsh")
            .arg("-lc")
            .arg("exit")
            .output()
        {
            Ok(_) => "zsh",
            Err(_) => return,
        };

        let integration_dir = unique_test_dir("zsh");
        let user_dir = unique_test_dir("zsh-user");
        fs::create_dir_all(&user_dir).unwrap();
        write_integration_dir(&integration_dir).unwrap();
        fs::write(user_dir.join(".zshenv"), "echo USER_ZSHENV\n").unwrap();
        fs::write(user_dir.join(".zprofile"), "echo USER_ZPROFILE\n").unwrap();
        fs::write(user_dir.join(".zshrc"), "echo USER_ZSHRC\n").unwrap();
        fs::write(user_dir.join(".zlogin"), "echo USER_ZLOGIN\n").unwrap();
        fs::write(user_dir.join(".zlogout"), "echo USER_ZLOGOUT\n").unwrap();

        let output = orcashell_platform::command(zsh)
            .arg("-il")
            .arg("-c")
            .arg("exit")
            .env("HOME", &user_dir)
            .env("ZDOTDIR", &integration_dir)
            .env("ORCASHELL_REAL_ZDOTDIR", &user_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("USER_ZSHENV"));
        assert!(stdout.contains("USER_ZPROFILE"));
        assert!(stdout.contains("USER_ZSHRC"));
        assert!(stdout.contains("USER_ZLOGIN"));
        assert!(stdout.contains("USER_ZLOGOUT"));
    }

    #[cfg(unix)]
    #[test]
    fn bash_wrapper_sources_user_login_files() {
        let bash = match orcashell_platform::command("bash")
            .arg("-lc")
            .arg("exit")
            .output()
        {
            Ok(_) => "bash",
            Err(_) => return,
        };

        let integration_dir = unique_test_dir("bash");
        let home_dir = unique_test_dir("bash-home");
        fs::create_dir_all(&home_dir).unwrap();
        write_integration_dir(&integration_dir).unwrap();
        fs::write(
            home_dir.join(".bash_profile"),
            "echo USER_BASH_PROFILE\nsource \"$HOME/.bashrc\"\n",
        )
        .unwrap();
        fs::write(home_dir.join(".bashrc"), "echo USER_BASHRC\n").unwrap();
        fs::write(home_dir.join(".bash_logout"), "echo USER_BASH_LOGOUT\n").unwrap();

        let output = orcashell_platform::command(bash)
            .arg("--login")
            .arg("-i")
            .arg("-c")
            .arg("exit")
            .env("HOME", &integration_dir)
            .env("ORCASHELL_REAL_HOME", &home_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("USER_BASH_PROFILE"));
        assert!(stdout.contains("USER_BASHRC"));
        assert!(stdout.contains("USER_BASH_LOGOUT"));
    }

    #[test]
    fn quote_path_bash_simple() {
        let p = Path::new("/home/user/my file.txt");
        assert_eq!(
            quote_path_for_shell(p, ShellType::Bash),
            "'/home/user/my file.txt'"
        );
    }

    #[test]
    fn quote_path_bash_embedded_quote() {
        let p = Path::new("/home/user/it's a file");
        assert_eq!(
            quote_path_for_shell(p, ShellType::Bash),
            "'/home/user/it'\\''s a file'"
        );
    }

    #[test]
    fn quote_path_zsh_same_as_bash() {
        let p = Path::new("/home/user/it's a file");
        assert_eq!(
            quote_path_for_shell(p, ShellType::Zsh),
            "'/home/user/it'\\''s a file'"
        );
    }

    #[test]
    fn quote_path_powershell_doubled_quote() {
        let p = Path::new("C:\\Users\\me\\it's here");
        assert_eq!(
            quote_path_for_shell(p, ShellType::PowerShellCore),
            "'C:\\Users\\me\\it''s here'"
        );
    }

    #[test]
    fn quote_path_windows_powershell_doubled_quote() {
        let p = Path::new("C:\\Users\\me\\it's here");
        assert_eq!(
            quote_path_for_shell(p, ShellType::WindowsPowerShell),
            "'C:\\Users\\me\\it''s here'"
        );
    }

    #[test]
    fn quote_path_cmd_double_quotes() {
        let p = Path::new("C:\\Program Files\\app");
        assert_eq!(
            quote_path_for_shell(p, ShellType::Cmd),
            "\"C:\\Program Files\\app\""
        );
    }

    #[test]
    fn quote_path_unknown_falls_back_to_posix() {
        let p = Path::new("/tmp/test file");
        assert_eq!(
            quote_path_for_shell(p, ShellType::Unknown),
            "'/tmp/test file'"
        );
    }

    #[test]
    fn quote_path_no_special_chars() {
        let p = Path::new("/usr/bin/ls");
        assert_eq!(quote_path_for_shell(p, ShellType::Bash), "'/usr/bin/ls'");
    }
}
