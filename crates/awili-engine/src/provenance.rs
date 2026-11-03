//! Provenance tags and opaque provider-native continuation artifacts.
//!
//! # The B1 continuation-blob rule (fixed here; B4 and C4 inherit it)
//!
//! Provider-native continuation and reasoning artifacts — OpenAI Responses
//! `reasoning` items, Anthropic `thinking` blocks — are **opaque,
//! provenance-tagged blobs** keyed to the exact provider/model + turn that
//! produced them. They may be replayed only back to that same compatible
//! provider/model; on a model switch they are excluded. The canonical
//! [`AgentEvent`](crate::AgentEvent) transcript is the **sole** cross-model
//! context carrier.
//!
//! This layer therefore *preserves* a blob and its eligibility metadata but
//! exposes **no** API that reads, interprets, or migrates the opaque payload.
//! [`Provenance::replayable_to`] is a pure function of the tag alone — it never
//! inspects [`ContinuationArtifact::payload`].

use serde::{Deserialize, Serialize};

/// Identifies the exact model instance that produced an artifact or turn:
/// `provider` + `model` + `turn` index. Two artifacts are replay-compatible
/// only when both their `provider` and `model` match.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Provenance {
    /// Adapter/dialect family, e.g. `"anthropic"`, `"openai-responses"`.
    pub provider: String,
    /// Concrete model id, e.g. `"claude-opus-4-8"`.
    pub model: String,
    /// Zero-based index of the turn within the session that produced this.
    pub turn: u32,
}

impl Provenance {
    pub fn new(provider: impl Into<String>, model: impl Into<String>, turn: u32) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            turn,
        }
    }

    /// Whether an artifact with this provenance may be replayed to a request
    /// targeting `provider`/`model`. Eligibility is a pure function of the
    /// provenance tag only — it never inspects any opaque payload. The turn
    /// index deliberately does *not* participate: a blob stays eligible for
    /// later turns of the *same* model, and only a model switch excludes it.
    pub fn replayable_to(&self, provider: &str, model: &str) -> bool {
        self.provider == provider && self.model == model
    }
}

/// A coarse tag for *which kind* of provider-native artifact this is. It exists
/// only so downstream can group or label artifacts; it deliberately says
/// nothing about the bytes' internal structure, which stay opaque.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ContinuationKind {
    /// OpenAI Responses reasoning item.
    Reasoning,
    /// Anthropic thinking block.
    Thinking,
    /// Any other provider-native continuation artifact.
    Other(String),
}

/// An opaque, provenance-tagged provider-native continuation artifact.
///
/// The `payload` bytes are carried verbatim and **never interpreted** by this
/// crate. Downstream slices may store it and consult [`Self::replayable_to`] to
/// decide eligibility, but must not decode, transform, or migrate it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationArtifact {
    pub provenance: Provenance,
    pub kind: ContinuationKind,
    /// Opaque payload, verbatim from the provider. Never interpreted here.
    pub payload: Vec<u8>,
}

impl ContinuationArtifact {
    pub fn new(provenance: Provenance, kind: ContinuationKind, payload: Vec<u8>) -> Self {
        Self {
            provenance,
            kind,
            payload,
        }
    }

    /// Eligibility metadata only: may this artifact be replayed to a request
    /// targeting `provider`/`model`? Delegates to the provenance tag; the
    /// payload is not read.
    pub fn replayable_to(&self, provider: &str, model: &str) -> bool {
        self.provenance.replayable_to(provider, model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replayable_only_to_same_provider_and_model() {
        let p = Provenance::new("anthropic", "claude-opus-4-8", 0);
        assert!(p.replayable_to("anthropic", "claude-opus-4-8"));
        assert!(!p.replayable_to("anthropic", "claude-sonnet-5"));
        assert!(!p.replayable_to("openai", "claude-opus-4-8"));
    }

    #[test]
    fn replay_ignores_turn_index() {
        // Same model, later turn: still eligible. Only a model switch excludes.
        let p = Provenance::new("openai", "gpt-x", 3);
        assert!(p.replayable_to("openai", "gpt-x"));
    }

    #[test]
    fn artifact_delegates_eligibility_to_provenance() {
        let a = ContinuationArtifact::new(
            Provenance::new("anthropic", "claude-opus-4-8", 1),
            ContinuationKind::Thinking,
            vec![0xDE, 0xAD, 0xBE, 0xEF],
        );
        assert!(a.replayable_to("anthropic", "claude-opus-4-8"));
        assert!(!a.replayable_to("anthropic", "claude-haiku-4-5"));
        // Payload is preserved verbatim and never touched by eligibility.
        assert_eq!(a.payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn artifact_serde_round_trip() {
        let a = ContinuationArtifact::new(
            Provenance::new("openai-responses", "o-mega", 2),
            ContinuationKind::Other("summary".into()),
            vec![1, 2, 3],
        );
        let json = serde_json::to_string(&a).unwrap();
        let back: ContinuationArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
