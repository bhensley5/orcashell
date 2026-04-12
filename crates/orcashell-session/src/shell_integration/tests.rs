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
    assert!(BASH_INTEGRATION.contains("133;A"));
    assert!(BASH_INTEGRATION.contains("133;B"));
    assert!(BASH_INTEGRATION.contains("133;C"));
    assert!(BASH_INTEGRATION.contains("133;D"));
    assert!(BASH_INTEGRATION.contains("]2;"));
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
