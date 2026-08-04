//! `salcc init <shell>` — emit shell integration that transparently wraps
//! configured agents so `claude` (etc.) runs through salcc with no change to
//! how the user invokes it. The user adds one line to their rc:
//! `eval "$(salcc init zsh)"`.
//!
//! Safety contract, shared by every shell variant:
//! - Recursion guard: salcc exports SALCC_ACTIVE=1 to the child (proxy/pty.rs).
//!   The wrapper no-ops to the real binary when SALCC_ACTIVE is set, so an
//!   agent that spawns a subshell which re-runs the agent never nests proxies.
//! - `command` bypasses the function for both the salcc call and the fallback,
//!   so exec resolves the real binaries, never the wrapper (no shell loop).
//! - Absent-salcc / absent-agent degrade: the wrapper only shadows an agent
//!   that exists on PATH, and falls back to the real agent if `salcc` is gone.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitShell {
    Zsh,
    Bash,
    Fish,
}

impl std::str::FromStr for InitShell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "zsh" => Ok(Self::Zsh),
            "bash" => Ok(Self::Bash),
            "fish" => Ok(Self::Fish),
            other => Err(format!("unsupported shell `{other}` (expected zsh, bash, or fish)")),
        }
    }
}

pub fn render(shell: InitShell, agents: &[String]) -> String {
    let agents: Vec<&str> = agents
        .iter()
        .map(String::as_str)
        .filter(|a| is_safe_name(a))
        .collect();
    match shell {
        InitShell::Zsh => posix(&agents, "eval \"$(salcc init zsh)\"", true),
        InitShell::Bash => posix(&agents, "eval \"$(salcc init bash)\"", false),
        InitShell::Fish => fish(&agents),
    }
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn posix(agents: &[&str], via: &str, zsh: bool) -> String {
    let mut out = format!(
        "# salcc shell integration — added via: {via}\n\
         _salcc_wrap() {{\n\
         \x20 local _salcc_agent=\"$1\"; shift\n\
         \x20 if [ -n \"$SALCC_ACTIVE\" ] || ! command -v salcc >/dev/null 2>&1; then\n\
         \x20   command \"$_salcc_agent\" \"$@\"\n\
         \x20 else\n\
         \x20   command salcc \"$_salcc_agent\" \"$@\"\n\
         \x20 fi\n\
         }}\n"
    );
    let escape = if zsh { "\\" } else { "" };
    for agent in agents {
        out.push_str(&format!(
            "unalias {agent} 2>/dev/null || true\n\
             if command -v {agent} >/dev/null 2>&1; then\n\
             \x20 {escape}{agent}() {{ _salcc_wrap {agent} \"$@\"; }}\n\
             fi\n"
        ));
    }
    out
}

fn fish(agents: &[&str]) -> String {
    let mut out = String::from(
        "# salcc shell integration — added via: salcc init fish | source\n\
         function _salcc_wrap\n\
         \x20 set -l _salcc_agent $argv[1]\n\
         \x20 set -e argv[1]\n\
         \x20 if set -q SALCC_ACTIVE; or not command -v salcc >/dev/null 2>&1\n\
         \x20   command $_salcc_agent $argv\n\
         \x20 else\n\
         \x20   command salcc $_salcc_agent $argv\n\
         \x20 end\n\
         end\n",
    );
    for agent in agents {
        out.push_str(&format!(
            "if command -v {agent} >/dev/null 2>&1\n\
             \x20 function {agent}; _salcc_wrap {agent} $argv; end\n\
             end\n"
        ));
    }
    out
}
