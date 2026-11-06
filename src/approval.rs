//! The seam the access gate asks its question through.
//!
//! Every approval path (`write`, `edit`, `multi_edit`, `bash`, networked `read`, MCP tools, the
//! path sandbox) funnels into [`crate::tools::prompt_user_approval`], which asks an [`Approver`]
//! and turns the answer into a `bool`. The terminal implementation lives here; a host without a
//! terminal (the desktop webview) supplies its own, so the gate does not have to know where the
//! question is drawn.
//!
//! Fail-closed is the contract, not a detail: an answer that cannot be collected is a "no".

use std::io::{self, Write};
use std::sync::Arc;

use crate::ansi::{BOLD, DIM, GREEN, RESET, YELLOW};
use crate::config::tr_en;
use crate::tools::AccessMode;

/// What the human said. `Sovereign` approves *and* widens the mode, which is a side effect the
/// caller applies — an approver never mutates agent state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAnswer {
    Approve,
    Deny,
    Sovereign,
}

/// Who answers the access gate.
pub trait Approver: Send + Sync {
    /// Ask whether `action_title` may run, showing `detail` as justification.
    ///
    /// `action_title` and `detail` carry model-controlled bytes (file content, shell command, MCP
    /// arguments); an implementation must render them safely, never as raw terminal input.
    fn ask(&self, action_title: &str, detail: &str, access: AccessMode) -> ApprovalAnswer;

    /// Report something the gate did on the human's behalf (currently: mode widened). Defaults to
    /// silence, so a GUI approver needs to implement only `ask`.
    fn notice(&self, _message: &str) {}
}

/// The approver every CLI context starts with.
pub fn terminal() -> Arc<dyn Approver> {
    Arc::new(TerminalApprover)
}

/// Reads the answer from a line on stdin, drawn as a bordered card.
#[derive(Debug, Default)]
pub struct TerminalApprover;

impl Approver for TerminalApprover {
    fn ask(&self, action_title: &str, detail: &str, access: AccessMode) -> ApprovalAnswer {
        // Fail closed without a TTY: the read below would block forever on a piped/inherited stdin
        // and the turn could never make progress. A prompt that cannot be answered is a "no".
        if !crate::hud::stdin_is_tty() {
            return ApprovalAnswer::Deny;
        }
        crate::hud::pause_active_spinner();
        // The window may be behind another one while the gate waits: the title glyph is the only
        // cue that a decision is blocking the turn.
        crate::hud::set_title_state(crate::hud::TitleState::Attention);
        println!(
            "\n{}╭─ 🛡️  {} ({}){}",
            YELLOW,
            tr_en("SPAVN ERİŞİM KAPISI", "SPAVN ACCESS GATE"),
            access.short_badge(),
            RESET
        );
        println!(
            "{}│{} {}{}{}",
            YELLOW,
            RESET,
            BOLD,
            crate::tools::gate_safe(action_title),
            RESET
        );
        for line in detail.lines().take(6) {
            println!(
                "{}│{} {}{}{}",
                YELLOW,
                RESET,
                DIM,
                crate::tools::gate_safe(line),
                RESET
            );
        }
        print!(
            "{}╰─ {}: {}",
            YELLOW,
            tr_en(
                "Onaylıyor musunuz? [e = Evet / H = Hayır / s = Sovereign Moda Geç]",
                "Approve? [y = Yes / N = No / s = Switch to Sovereign Mode]"
            ),
            RESET
        );
        let _ = io::stdout().flush();

        let mut ans = String::new();
        // Ok(0) = EOF (closed/piped stdin): never treat it as the empty "default yes" answer.
        if !matches!(io::stdin().read_line(&mut ans), Ok(n) if n > 0) {
            crate::hud::set_title_state(crate::hud::TitleState::Working);
            crate::hud::resume_active_spinner();
            return ApprovalAnswer::Deny;
        }
        crate::hud::set_title_state(crate::hud::TitleState::Working);
        crate::hud::resume_active_spinner();
        let clean = ans.trim().to_lowercase();
        if clean == "s" || clean == "sovereign" {
            return ApprovalAnswer::Sovereign;
        }
        // Enter alone declines: a stray keypress must not approve a risky action.
        if matches!(clean.as_str(), "e" | "evet" | "y" | "yes") {
            ApprovalAnswer::Approve
        } else {
            ApprovalAnswer::Deny
        }
    }

    fn notice(&self, message: &str) {
        println!("{}🔥 {}{}", GREEN, message, RESET);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test approver that returns a fixed answer and records what it was asked.
    struct Stub(ApprovalAnswer);

    impl Approver for Stub {
        fn ask(&self, _action_title: &str, _detail: &str, _access: AccessMode) -> ApprovalAnswer {
            self.0
        }
    }

    #[test]
    fn answer_maps_to_the_gate_boolean() {
        let mut ctx = crate::tools::ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            std::path::PathBuf::from("."),
            crate::spavn_engine::AgentPhase::Code,
        );
        ctx.approver = Arc::new(Stub(ApprovalAnswer::Approve));
        assert!(crate::tools::prompt_user_approval(
            &mut ctx,
            "write a.rs",
            "+1 -0"
        ));
        assert_eq!(ctx.access, AccessMode::Safe);

        ctx.approver = Arc::new(Stub(ApprovalAnswer::Deny));
        assert!(!crate::tools::prompt_user_approval(
            &mut ctx,
            "write a.rs",
            "+1 -0"
        ));
    }

    #[test]
    fn sovereign_approves_and_widens_the_mode() {
        let mut ctx = crate::tools::ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            std::path::PathBuf::from("."),
            crate::spavn_engine::AgentPhase::Code,
        );
        ctx.approver = Arc::new(Stub(ApprovalAnswer::Sovereign));
        assert!(crate::tools::prompt_user_approval(
            &mut ctx,
            "bash",
            "rm -rf /tmp/x"
        ));
        assert_eq!(ctx.access, AccessMode::Sovereign);
    }

    /// The terminal approver must decline without a TTY instead of blocking on stdin.
    #[test]
    fn terminal_approver_fails_closed_without_a_tty() {
        if crate::hud::stdin_is_tty() {
            return;
        }
        assert_eq!(
            TerminalApprover.ask("bash", "echo hi", AccessMode::Safe),
            ApprovalAnswer::Deny
        );
    }
}
