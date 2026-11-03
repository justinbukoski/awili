//! Textual screening floors (contract §6/§8). POLICY, not the security
//! boundary — C3's descriptor-relative execution enforces at runtime.

use awili_engine::events::ToolCall;
use awili_engine_log::authz::RiskClass;

use crate::risk::{has_path_traversal, path_args, touches_secrets, unplaced_path_args};

/// RC2-ENGB1 (TOP43 #17): the origin of the engine's own file tools — the
/// only tools whose relative paths resolve inside the bound folder (their
/// sandbox opens every path beneath the folder and refuses `~`).
pub const BUILTIN_FS_ORIGIN: &str = "builtin:fs";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screening {
    Pass,
    /// Cannot be auto-allowed regardless of risk class or grants at
    /// read_only; the dialog must run (secrets, deletions).
    RequireAsk,
    Deny {
        reason: String,
    },
}

/// Textual pre-screening. Deterministic; case-normalized; escalation-only.
///
/// `paths_resolve_in_folder`: the tool's executor opens relative paths
/// beneath the bound folder (the engine's own file tools,
/// [`BUILTIN_FS_ORIGIN`]). For any other tool (an MCP server resolves a
/// relative path against its own working directory, `~` against the home
/// folder) a path the folder check cannot place
/// ([`crate::risk::unplaced_path_args`]) is shown to the owner: the dialog
/// must run, whatever the risk class, and no grant or auto mode clears it.
/// It is not denied — it may well be inside the folder.
pub fn screen(
    call: &ToolCall,
    folder_root: Option<&str>,
    risk: RiskClass,
    paths_resolve_in_folder: bool,
) -> Screening {
    let paths = path_args(&call.arguments);

    // Folder boundary (§6): path arguments resolving textually outside the
    // bound root are denied by default. (Absolute-outside and `..`
    // traversal; symlink/TOCTOU enforcement is C3's.)
    if let Some(root) = folder_root {
        for p in &paths {
            if has_path_traversal(p) {
                return Screening::Deny {
                    // RC2-R51: plain words, for the model and the owner.
                    reason: format!("{p} steps out of the folder with \"..\" (a path traversal)"),
                };
            }
            // Boundary-safe containment (Codex C1p2 P0): "/root/x" is
            // inside "/root", "/root-evil/x" is not — compare with a
            // trailing separator or exact equality, never bare prefix.
            let root_dir = if root.ends_with('/') {
                root.to_string()
            } else {
                format!("{root}/")
            };
            if p.starts_with('/') && !(p.starts_with(&root_dir) || p == root) {
                // Escalated Ask is possible for read_only only (§6).
                if risk == RiskClass::ReadOnly {
                    return Screening::RequireAsk;
                }
                return Screening::Deny {
                    // RC2-R51 (usability RC2 round 4, Major 3): plain words —
                    // the model said its "write_file tool is restricted to the
                    // bound folder".
                    reason: format!(
                        "{p} is outside this conversation's folder, {root}, and Awili \
                         changes files only inside it"
                    ),
                };
            }
        }
        // RC2-ENGB1 (TOP43 #17): a relative or home path for a tool that
        // does not resolve it inside the folder cannot be placed.
        if !paths_resolve_in_folder && !unplaced_path_args(&call.arguments).is_empty() {
            return Screening::RequireAsk;
        }
    }

    // Secrets (§8): ask-minimum even inside the folder.
    if paths.iter().any(|p| touches_secrets(p)) {
        return Screening::RequireAsk;
    }

    // Deletion protection (§8): always >= Ask.
    if call.name == "fs.rm" {
        return Screening::RequireAsk;
    }

    Screening::Pass
}
