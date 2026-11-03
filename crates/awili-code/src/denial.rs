//! Bounded diagnostics for structured CLI permission denials.
//!
//! Tool inputs, result text and arbitrary denial objects never enter this
//! message. An executor denial is not evidence of a particular host hook.

use serde_json::Value;

pub(crate) fn claude_permission_denial(value: &Value) -> String {
    format!("Blocked by Claude CLI permission policy: permission denied for {}. The terminal result did not confirm success.", denied_tools(value))
}

pub(crate) fn claude_historical_denial(value: &Value) -> String {
    format!(
        "Claude was not allowed to use {} and finished without it.",
        denied_tools(value)
    )
}

fn denied_tools(value: &Value) -> String {
    let mut names: Vec<&str> = Vec::new();
    let mut omitted = false;
    if let Some(items) = value.get("permission_denials").and_then(Value::as_array) {
        for item in items {
            let Some(name) = item
                .get("tool_name")
                .or_else(|| item.get("tool"))
                .and_then(Value::as_str)
                .filter(|name| valid_name(name))
            else {
                omitted = true;
                continue;
            };
            if names.contains(&name) {
                continue;
            }
            if names.len() < 4 {
                names.push(name);
            } else {
                omitted = true;
            }
        }
    }
    if names.is_empty() {
        "a required tool".to_string()
    } else if omitted {
        format!("{} and other tools", names.join(", "))
    } else {
        names.join(", ")
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && name.as_bytes()[0].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    /// RC1-N (round 4 Minor 5): the note after a Claude turn that finished
    /// despite a refused tool is plain words — no "terminal success" or
    /// "permission denials".
    #[test]
    fn rc1n_historical_denial_is_plain() {
        let v = serde_json::json!({ "permission_denials": [{ "tool_name": "Bash" }] });
        assert_eq!(
            super::claude_historical_denial(&v),
            "Claude was not allowed to use Bash and finished without it."
        );
        let none = serde_json::json!({ "permission_denials": [] });
        let text = super::claude_historical_denial(&none);
        assert!(!text.contains("terminal success") && !text.contains("permission denials"));
    }
}
