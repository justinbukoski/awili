//! A host approval is answered only by a live native control.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    Once,
    Session,
    Deny,
}

#[derive(Clone, Debug)]
pub struct HostApproval {
    pub title: String,
    pub detail: String,
    pub session_allowed: bool,
    active: Arc<AtomicBool>,
    reply: mpsc::Sender<ApprovalChoice>,
}
impl PartialEq for HostApproval {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.active, &other.active)
    }
}
impl Eq for HostApproval {}
impl HostApproval {
    pub fn channel(
        title: String,
        detail: String,
        session_allowed: bool,
    ) -> (Self, mpsc::Receiver<ApprovalChoice>) {
        let (reply, rx) = mpsc::channel();
        (
            Self {
                title,
                detail,
                session_allowed,
                active: Arc::new(AtomicBool::new(true)),
                reply,
            },
            rx,
        )
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn expire(&self) {
        self.active.store(false, Ordering::Release);
    }
    pub fn expiration(&self) -> impl FnOnce() + use<> {
        let active = self.active.clone();
        move || active.store(false, Ordering::Release)
    }
    pub fn answer(&self, choice: ApprovalChoice) -> bool {
        if choice == ApprovalChoice::Session && !self.session_allowed {
            return false;
        }
        if self.active.swap(false, Ordering::AcqRel) {
            self.reply.send(choice).is_ok()
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_first_live_answer_is_delivered() {
        let (prompt, rx) = HostApproval::channel("Capture?".into(), "Whole screen".into(), true);
        assert!(prompt.answer(ApprovalChoice::Session));
        assert!(!prompt.answer(ApprovalChoice::Once));
        assert_eq!(rx.recv().unwrap(), ApprovalChoice::Session);
    }
    #[test]
    fn expired_and_once_only_controls_cannot_grant_a_session() {
        let (prompt, rx) = HostApproval::channel("Open URL?".into(), "URL".into(), false);
        assert!(!prompt.answer(ApprovalChoice::Session));
        let expire = prompt.expiration();
        expire();
        assert!(!prompt.answer(ApprovalChoice::Once));
        drop(prompt);
        assert!(rx.recv().is_err());
    }
}
