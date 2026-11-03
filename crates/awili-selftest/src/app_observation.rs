//! Independent oracle for the app's single workspace-read diagnostic.
use crate::evaluate::Outcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correlation {
    pub fixture_id: String,
    pub conversation: String,
    pub run_seq: u64,
}

pub fn workspace_read_outcome(
    nonce: &str,
    request: &str,
    answer: &str,
    successful: bool,
    cancelled: bool,
    readback: Option<&[(String, String)]>,
) -> Outcome {
    if cancelled {
        return Outcome::Fail {
            reason: "cancelled or timed out".into(),
        };
    }
    if !successful {
        return Outcome::Fail {
            reason: "Work turn did not finish successfully".into(),
        };
    }
    if nonce.is_empty() || request.contains(nonce) {
        return Outcome::Error {
            message: "invalid challenge isolation".into(),
        };
    }
    if !answer.lines().any(|line| line.trim() == nonce) {
        return Outcome::Fail {
            reason: "challenge content not returned as an exact line".into(),
        };
    }
    let expected = [
        ("user".to_string(), request.to_string()),
        ("assistant".to_string(), answer.to_string()),
    ];
    if readback != Some(expected.as_slice()) {
        return Outcome::Error {
            message: "exact request and answer were not confirmed in persistence".into(),
        };
    }
    Outcome::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_line_and_persisted_pair_are_required() {
        let request = "Read the challenge path";
        let answer = "Version shown\n```\nrandom-nonce\n```";
        let rows = vec![
            ("user".into(), request.into()),
            ("assistant".into(), answer.into()),
        ];
        assert_eq!(
            workspace_read_outcome("random-nonce", request, answer, true, false, Some(&rows)),
            Outcome::Pass
        );
        assert!(matches!(
            workspace_read_outcome("random-nonce", request, answer, true, false, None),
            Outcome::Error { .. }
        ));
        assert!(matches!(
            workspace_read_outcome(
                "random-nonce",
                request,
                "prefix random-nonce suffix",
                true,
                false,
                Some(&rows)
            ),
            Outcome::Fail { .. }
        ));
        assert!(matches!(
            workspace_read_outcome("random-nonce", request, answer, false, false, Some(&rows)),
            Outcome::Fail { .. }
        ));
        assert!(matches!(
            workspace_read_outcome("random-nonce", request, answer, true, true, Some(&rows)),
            Outcome::Fail { .. }
        ));
        assert!(matches!(
            workspace_read_outcome(
                "random-nonce",
                "random-nonce",
                answer,
                true,
                false,
                Some(&rows)
            ),
            Outcome::Error { .. }
        ));
        let mut wrong = rows.clone();
        wrong[1].1 = "different answer".into();
        assert!(matches!(
            workspace_read_outcome("random-nonce", request, answer, true, false, Some(&wrong)),
            Outcome::Error { .. }
        ));
        let mut extra = rows.clone();
        extra.push(("assistant".into(), answer.into()));
        assert!(matches!(
            workspace_read_outcome("random-nonce", request, answer, true, false, Some(&extra)),
            Outcome::Error { .. }
        ));
    }
}
