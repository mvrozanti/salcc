//! Shell integration emitter: wrapper functions are recursion-guarded,
//! alias-safe per shell, and only defined for the configured agents.

use salcc::shell_init::{InitShell, render};

fn agents() -> Vec<String> {
    vec!["claude".to_string(), "codex".to_string()]
}

#[test]
fn zsh_escapes_function_name_and_guards() {
    let script = render(InitShell::Zsh, &agents());
    assert!(script.contains("SALCC_ACTIVE"), "must carry the recursion guard");
    assert!(script.contains("command salcc \"$_salcc_agent\""), "routes through salcc");
    assert!(script.contains("\\claude()"), "zsh needs the backslash to dodge parse-time alias expansion");
    assert!(script.contains("\\codex()"));
    assert!(script.contains("unalias claude"));
}

#[test]
fn bash_defines_plain_names() {
    let script = render(InitShell::Bash, &agents());
    assert!(script.contains("SALCC_ACTIVE"));
    assert!(script.contains("unalias claude"), "bash relies on runtime unalias");
    assert!(script.contains("  claude() { _salcc_wrap claude"), "bash rejects a backslashed identifier");
    assert!(!script.contains("\\claude()"));
}

#[test]
fn fish_uses_function_syntax_and_guard() {
    let script = render(InitShell::Fish, &agents());
    assert!(script.contains("set -q SALCC_ACTIVE"));
    assert!(script.contains("command salcc"));
    assert!(script.contains("function claude;"));
}

#[test]
fn unsafe_agent_names_are_dropped() {
    let bad = vec!["claude".to_string(), "rm -rf /".to_string(), "$(evil)".to_string()];
    let script = render(InitShell::Zsh, &bad);
    assert!(script.contains("\\claude()"));
    assert!(!script.contains("rm -rf"), "shell metacharacters must never reach the emitted script");
    assert!(!script.contains("evil"));
}

#[test]
fn unknown_shell_is_rejected() {
    assert!("powershell".parse::<InitShell>().is_err());
    assert_eq!("ZSH".parse::<InitShell>().unwrap(), InitShell::Zsh);
}

#[test]
fn default_config_wraps_claude() {
    let config = salcc::config::Config::default();
    assert_eq!(config.agents, vec!["claude".to_string()]);
    let script = render(InitShell::Zsh, &config.agents);
    assert!(script.contains("\\claude()"));
}

#[test]
fn agents_override_parses() {
    let yaml = "agents: [claude, codex, gemini]\n";
    let config = salcc::config::Config::from_yaml(yaml).unwrap();
    assert_eq!(config.agents, vec!["claude", "codex", "gemini"]);
}
