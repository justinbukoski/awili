//! Native Codex approvals. Grants are ephemeral, owned by one CodeSession,
//! and match the exact disclosed request, never a server-wide wildcard.
use crate::{theme, AwiliApp};
use awili_providers::{ApprovalChoice, HostApproval, ProviderEvent};
use gpui::{div, prelude::*, Context};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Default)]
pub(crate) struct Session(Arc<Mutex<HashSet<String>>>);

pub(crate) struct Request {
    id: Value,
    thread: String,
    turn: String,
    title: String,
    detail: String,
    key: Option<String>,
    allowed: Value,
    denied: Value,
}
impl Request {
    pub(crate) fn parse(line: &str, thread: Option<&str>, turn: Option<&str>) -> Option<Self> {
        let v: Value = serde_json::from_str(line).ok()?;
        let id = v.get("id")?.clone();
        if !id.is_string() && !id.is_number() {
            return None;
        }
        let method = v.get("method")?.as_str()?;
        let p = v.get("params")?;
        let thread = thread?;
        if p.get("threadId")?.as_str()? != thread || p.get("turnId")?.as_str()? != turn? {
            return None;
        }
        let (title, allowed, denied, scope) = match method {
            "item/commandExecution/requestApproval" => {
                // stdin/execve sub-approvals and missing commands need richer UI.
                let command = p.get("command")?.as_str()?;
                let cwd = p.get("cwd")?.as_str()?;
                if p.get("kind")
                    .is_some_and(|k| !k.is_null() && k.as_str() != Some("command"))
                {
                    return None;
                }
                (
                    "Run command?".to_string(),
                    json!({"decision":"accept"}),
                    json!({"decision":"decline"}),
                    Some(
                        json!([
                            method,
                            thread,
                            command,
                            cwd,
                            p.get("networkApprovalContext"),
                            p.get("additionalPermissions")
                        ])
                        .to_string(),
                    ),
                )
            }
            "item/permissions/requestApproval" => {
                let permissions = p.get("permissions")?.as_object()?;
                // Unknown permission categories must not be granted implicitly.
                if permissions
                    .keys()
                    .any(|k| !matches!(k.as_str(), "fileSystem" | "network"))
                {
                    return None;
                }
                (
                    "Grant these permissions?".to_string(),
                    json!({"permissions":permissions,"scope":"turn"}),
                    json!({"permissions":{}}),
                    Some(json!([method, thread, permissions]).to_string()),
                )
            }
            "mcpServer/elicitation/request" => {
                let server = p.get("serverName")?.as_str()?;
                let message = p.get("message")?.as_str()?;
                let mode = p.get("mode")?.as_str()?;
                // Codex's tool-approval elicitation carries exact tool arguments
                // in metadata. General forms and authentication URLs are separate.
                if mode != "form" {
                    return None;
                }
                let schema = p.get("requestedSchema")?;
                if schema.get("type")?.as_str()? != "object"
                    || !schema.get("properties")?.as_object()?.is_empty()
                {
                    return None;
                }
                if schema
                    .get("required")
                    .is_some_and(|r| r.as_array().is_none_or(|a| !a.is_empty()))
                {
                    return None;
                }
                let meta = p.get("_meta")?;
                if meta.get("codex_approval_kind")?.as_str()? != "mcp_tool_call" {
                    return None;
                }
                meta.get("tool_name")?.as_str()?;
                meta.get("tool_params")?.as_object()?;
                (
                    format!("{server}: {message}"),
                    json!({"action":"accept","content":{}}),
                    json!({"action":"decline"}),
                    Some(json!([method, thread, server, mode, message, schema, meta]).to_string()),
                )
            }
            _ => return None,
        };
        let key = scope.map(|_| {
            let mut semantic = p.clone();
            if let Some(object) = semantic.as_object_mut() {
                for field in ["threadId", "turnId", "itemId", "approvalId", "startedAtMs"] {
                    object.remove(field);
                }
            }
            json!([method, thread, semantic]).to_string()
        });
        Some(Self {
            id,
            thread: thread.to_string(),
            turn: turn?.to_string(),
            title,
            detail: serde_json::to_string_pretty(p).ok()?,
            key,
            allowed,
            denied,
        })
    }
    pub(crate) fn cancelled_by(&self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        if v.pointer("/params/threadId").and_then(Value::as_str) != Some(self.thread.as_str()) {
            return false;
        }
        match v.get("method").and_then(Value::as_str) {
            Some("serverRequest/resolved") => v.pointer("/params/requestId") == Some(&self.id),
            Some("turn/completed") => {
                v.pointer("/params/turn/id").and_then(Value::as_str) == Some(self.turn.as_str())
            }
            _ => false,
        }
    }
    fn response(&self, choice: ApprovalChoice) -> String {
        // Host-side exact matching implements session grants. Do not ask Codex
        // to install a broader command-prefix or filesystem-root policy.
        json!({"id":self.id,"result":if choice == ApprovalChoice::Deny { &self.denied } else { &self.allowed }}).to_string()
    }
}

pub(crate) fn resolve(
    request: &Request,
    session: &Session,
    tx: &async_channel::Sender<ProviderEvent>,
    mut ended: impl FnMut() -> bool,
) -> String {
    if ended() {
        return request.response(ApprovalChoice::Deny);
    }
    let cached = request
        .key
        .as_ref()
        .is_some_and(|key| session.0.lock().is_ok_and(|s| s.contains(key)));
    if cached {
        return request.response(ApprovalChoice::Once);
    }
    let (prompt, rx) = HostApproval::channel(
        request.title.clone(),
        request.detail.clone(),
        request.key.is_some(),
    );
    let expire = prompt.expiration();
    if crate::code_provider::send_native_event(tx, ProviderEvent::HostApproval(prompt)).is_err()
    {
        expire();
        return request.response(ApprovalChoice::Deny);
    }
    let choice = loop {
        if ended() || tx.is_closed() {
            break ApprovalChoice::Deny;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(choice) => break choice,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break ApprovalChoice::Deny,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    expire();
    let choice = if ended() {
        ApprovalChoice::Deny
    } else {
        choice
    };
    if choice == ApprovalChoice::Session {
        if let (Some(key), Ok(mut grants)) = (&request.key, session.0.lock()) {
            grants.insert(key.clone());
        }
    }
    request.response(choice)
}

impl AwiliApp {
    pub(crate) fn code_host_approval_card(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let tab = self.work.code_tabs.get(self.work.code_active)?;
        let request = tab.host_approval.as_ref().filter(|r| r.active())?;
        let mut buttons = div().flex().flex_wrap().gap_2();
        for (id, label, choice) in [
            ("host-approve-once", "Allow once", ApprovalChoice::Once),
            (
                "host-approve-session",
                "Allow for this session",
                ApprovalChoice::Session,
            ),
            ("host-approve-deny", "Deny", ApprovalChoice::Deny),
        ] {
            if choice == ApprovalChoice::Session && !request.session_allowed {
                continue;
            }
            let request = request.clone();
            buttons = buttons.child(
                crate::ui::button::Button::new(id, label)
                    .selector(id)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        request.answer(choice);
                        cx.notify();
                    })),
            );
        }
        Some(div().id("code-host-approval").flex().flex_col().gap_2().p_3()
            .border_1().border_color(theme::p().accent)
            .child(request.title.clone())
            .child(div().id("host-approval-detail").max_h(theme::z(180.)).overflow_y_scroll().child(request.detail.clone()))
            .child("Session approval covers this exact request in this conversation until Stop or the tab closes.")
            .child(buttons).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(command: &str, turn: &str) -> String {
        json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":turn,"itemId":"item","command":command,"cwd":"/project","reason":"Run the requested check","kind":"command"}}).to_string()
    }
    fn parse(line: &str) -> Request {
        Request::parse(line, Some("thread"), Some("turn")).unwrap()
    }
    fn resolve_with(request: &Request, session: &Session, choice: ApprovalChoice) -> Value {
        let (tx, rx) = async_channel::unbounded();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let ProviderEvent::HostApproval(prompt) = rx.recv_blocking().unwrap() else {
                    panic!("approval event")
                };
                assert!(prompt.answer(choice));
                assert!(!prompt.answer(ApprovalChoice::Once));
            });
            serde_json::from_str(&resolve(request, session, &tx, || false)).unwrap()
        })
    }
    #[test]
    fn session_grants_match_exact_semantics_and_never_another_conversation() {
        let session = Session::default();
        let request = parse(&command("cargo test", "turn"));
        assert_eq!(
            resolve_with(&request, &session, ApprovalChoice::Session)["result"]["decision"],
            "accept"
        );
        let mut next: Value = serde_json::from_str(&command("cargo test", "next")).unwrap();
        next["id"] = json!(8);
        next["params"]["itemId"] = json!("next-item");
        let next = Request::parse(&next.to_string(), Some("thread"), Some("next")).unwrap();
        assert_eq!(next.key, request.key);
        let (tx, rx) = async_channel::unbounded();
        assert!(resolve(&next, &session, &tx, || false).contains("accept"));
        assert!(rx.try_recv().is_err(), "cached approval never prompts");
        assert_ne!(parse(&command("cargo test --all", "turn")).key, request.key);
        assert!(Session::default().0.lock().unwrap().is_empty());
        assert!(
            resolve(&request, &session, &tx, || true).contains("decline"),
            "an ended worker cannot use a cached grant"
        );
    }
    #[test]
    fn once_and_deny_never_create_session_grants() {
        for choice in [ApprovalChoice::Once, ApprovalChoice::Deny] {
            let session = Session::default();
            let answer = resolve_with(&parse(&command("check", "turn")), &session, choice);
            assert_eq!(
                answer["result"]["decision"],
                if choice == ApprovalChoice::Deny {
                    "decline"
                } else {
                    "accept"
                }
            );
            assert!(session.0.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn stopped_or_dropped_consumers_deny_and_expire_their_buttons() {
        let request = parse(&command("check", "turn"));
        let (tx, rx) = async_channel::unbounded();
        let session = Session::default();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| resolve(&request, &session, &tx, || !tx.is_empty()));
            let answer = worker.join().unwrap();
            assert!(answer.contains("decline"));
            let ProviderEvent::HostApproval(prompt) = rx.recv_blocking().unwrap() else {
                panic!()
            };
            assert!(!prompt.answer(ApprovalChoice::Session));
        });
        let (tx, rx) = async_channel::unbounded();
        drop(rx);
        assert!(resolve(&request, &session, &tx, || false).contains("decline"));
    }
    #[test]
    fn stale_threads_turns_and_unknown_forms_are_not_approved() {
        let line = command("check", "turn");
        assert!(Request::parse(&line, Some("other"), Some("turn")).is_none());
        assert!(Request::parse(&line, Some("thread"), Some("old")).is_none());
        assert!(Request::parse(&line, None, None).is_none());
        let mut stdin: Value = serde_json::from_str(&line).unwrap();
        stdin["params"]["kind"] = json!("writeStdin");
        assert!(Request::parse(&stdin.to_string(), Some("thread"), Some("turn")).is_none());
        let mut form = json!({"id":"form","method":"mcpServer/elicitation/request","params":{"threadId":"thread","turnId":"turn","serverName":"desktop","mode":"form","message":"Capture the whole screen?","requestedSchema":{"type":"object","properties":{}},"_meta":{"codex_approval_kind":"mcp_tool_call","tool_name":"screenshot","tool_params":{"allScreens":true}}}});
        let request = parse(&form.to_string());
        assert_eq!(request.allowed["action"], "accept");
        form["params"]["_meta"]["tool_params"]["allScreens"] = json!(false);
        assert_ne!(parse(&form.to_string()).key, request.key);
        form["params"]["requestedSchema"]["properties"]["password"] = json!({"type":"string"});
        assert!(Request::parse(&form.to_string(), Some("thread"), Some("turn")).is_none());
    }
}
