//! Pure attestation vocabulary (unit 4b, design r9 §2.1–§2.3).
//!
//! Value types, fact accessors, the scoped low-level bind, and the
//! trusted-issuer mint — nothing else. The IO half (setup-record loader,
//! platform attestor, `InstallAuthorityView`) lives in `awili-attest`, above
//! this crate; `awili-tools` re-exports this vocabulary as
//! `awili_tools::attest` so registry-side consumers gain no new manifest edge.
//!
//! Trust posture (design r9 §2.2–§2.3, rulings A/F): the attestor and the
//! runtime are the trusted issuers. Minting is public because a hostile
//! issuer defeats any downstream check regardless; what the type system
//! guarantees is that *consumers* — pump, roots validation, projection,
//! grant path — can read facts but can never forge, substitute, re-pair, or
//! extract authority: `LaunchToken` is not `Clone`, has no public field, no
//! serde, no `Deref`, and the only route to a `&VerifiedLaunch` is the bind.

use awili_engine::generation::ConnectionGeneration;
use awili_engine::identity::ToolOrigin;

/// The attestation class of a selected launch. A `Copy` fact, carried in
/// lifecycle snapshots and death diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestationClass {
    Verified,
    Unverified,
}

/// How an installer-authored setup record classifies the launch shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// The recorded program IS the semantic executable; attestation applies.
    Direct,
    /// The recorded program interprets other content; never attested
    /// (design r9 §2.5, ruling B lineage).
    Interpreted,
}

/// Why a launch selected an unverified token (design r9 §2.4 Phase A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnverifiedReason {
    /// No setup record exists for the origin.
    NoSetupRecord,
    /// The record classifies the launch `interpreted`, or the basename
    /// denylist (defense in depth) matched.
    InterpreterOnly,
    /// The record's program disagrees with the spawned program.
    ProgramMismatch,
}

/// Opaque setup identity. Private field, comparisons only — possession of an
/// id is not authority; every consumer compares ids against record-sourced
/// values, so a self-made id matches nothing the loader did not also produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupIdentityId(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SetupIdentityParseError {
    #[error("setup identity id is empty")]
    Empty,
    #[error("setup identity id contains NUL or control bytes")]
    ControlBytes,
}

impl SetupIdentityId {
    /// Stable serialization surface (unit 5 r3 verdict rev 3): the exact
    /// identity string — never `Debug`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The one constructor (design r9 §2.3): validates shape and returns the
    /// opaque id. The loader in `awili-attest` is the production caller.
    pub fn parse(s: &str) -> Result<SetupIdentityId, SetupIdentityParseError> {
        if s.is_empty() {
            return Err(SetupIdentityParseError::Empty);
        }
        if s.chars().any(|c| c.is_control()) {
            return Err(SetupIdentityParseError::ControlBytes);
        }
        Ok(SetupIdentityId(s.to_owned()))
    }
}

/// Pure, installer-authored facts of one setup record (design r9 §2.3).
/// Parsed by the `awili-attest` loader; carried here so the mint can copy
/// from it — `VerifiedLaunch.origin` has no independent origin argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupRecordFacts {
    pub origin: ToolOrigin,
    pub setup_id: SetupIdentityId,
    pub pinned_requirement: String,
    pub expected_protocol_name: String,
    pub launch_kind: LaunchKind,
    pub program: String,
}

/// A verified launch's facts. No public constructor anywhere — minted only
/// inside [`LaunchToken::mint_verified`], reachable only through the bind.
#[derive(Debug)]
pub struct VerifiedLaunch {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    setup_id: SetupIdentityId,
    pinned_requirement: String,
    expected_protocol_name: String,
}

impl VerifiedLaunch {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn setup_id(&self) -> &SetupIdentityId {
        &self.setup_id
    }
    pub fn pinned_requirement(&self) -> &str {
        &self.pinned_requirement
    }
    pub fn expected_protocol_name(&self) -> &str {
        &self.expected_protocol_name
    }
}

/// Unit 5 (design r9 §3-5 of the verdict line, rulings 14/18-20/26): proof
/// that the CURRENT verified launch of `origin` carries this setup
/// identity — the per-call install-grant matching input.
///
/// The invariant `'scope` brand plus the callback-only issuer
/// ([`VerifiedLaunch::with_install_witness`]) make escaping the
/// synchronous callback a lifetime error in safe Rust: no `Clone`, no
/// `Copy`, no serde, no raw factory (ruling 18 removed it), and `Debug`
/// redacts. Claim, stated precisely: the witness cannot escape the
/// synchronous callback in safe Rust; no claim is made about async code
/// that completes entirely inside it.
pub struct InstallOriginWitness<'scope> {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    setup_id: SetupIdentityId,
    pinned_requirement: String,
    _brand: std::marker::PhantomData<fn(&'scope ()) -> &'scope ()>,
}

impl std::fmt::Debug for InstallOriginWitness<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "InstallOriginWitness({}, {})",
            self.origin.origin, self.generation
        )
    }
}

impl InstallOriginWitness<'_> {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn setup_id(&self) -> &SetupIdentityId {
        &self.setup_id
    }
    pub fn pinned_requirement(&self) -> &str {
        &self.pinned_requirement
    }
}

impl VerifiedLaunch {
    /// The ONLY witness issuer (rulings 18/19): rank-2, consuming — the
    /// witness exists only inside `f` and every fact is copied from
    /// `self` (ruling 14: possession of a `&VerifiedLaunch` authorizes
    /// exactly the facts of THAT launch, nothing caller-selected).
    pub fn with_install_witness<R>(
        &self,
        f: impl for<'s> FnOnce(InstallOriginWitness<'s>) -> R,
    ) -> R {
        f(InstallOriginWitness {
            origin: self.origin.clone(),
            generation: self.generation,
            setup_id: self.setup_id.clone(),
            pinned_requirement: self.pinned_requirement.clone(),
            _brand: std::marker::PhantomData,
        })
    }
}

/// An unverified launch's facts. Same construction posture as
/// [`VerifiedLaunch`].
#[derive(Debug)]
pub struct UnverifiedLaunch {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    reason: UnverifiedReason,
}

impl UnverifiedLaunch {
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn generation(&self) -> ConnectionGeneration {
        self.generation
    }
    pub fn reason(&self) -> UnverifiedReason {
        self.reason
    }
}

/// The selected launch token. NOT `Clone` — the attestor mints exactly one
/// per Phase A, and moving it into the runtime's one observation leaves
/// nothing to re-emit with ("one attested launch, one observation" is a
/// runtime-path invariant; design r9 §2.2). `Debug` prints facts and can
/// produce no authority borrow.
#[derive(Debug)]
pub struct LaunchToken(TokenInner);

#[derive(Debug)]
enum TokenInner {
    Verified(VerifiedLaunch),
    Unverified(UnverifiedLaunch),
}

/// The one authority-bearing read of a token, borrowed, class-routed.
#[derive(Debug)]
pub enum BoundLaunch<'a> {
    Verified(&'a VerifiedLaunch),
    Unverified(&'a UnverifiedLaunch),
}

/// The scoped low-level bind's refusal (design r9 §2.2): a foreign origin or
/// generation never yields a borrow.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindRefusal {
    #[error("launch token is scoped to origin {actual:?}, not {requested:?}")]
    OriginMismatch {
        actual: ToolOrigin,
        requested: ToolOrigin,
    },
    #[error("launch token is scoped to generation {actual}, not {requested}")]
    GenerationMismatch {
        actual: ConnectionGeneration,
        requested: ConnectionGeneration,
    },
}

impl LaunchToken {
    /// Trusted-issuer constructor for the verified class (design r9 §2.3).
    /// The origin is COPIED FROM THE RECORD — there is no origin parameter
    /// to disagree with it; the mint-time invariant is API-shaped.
    pub fn mint_verified(
        record: &SetupRecordFacts,
        generation: ConnectionGeneration,
    ) -> LaunchToken {
        LaunchToken(TokenInner::Verified(VerifiedLaunch {
            origin: record.origin.clone(),
            generation,
            setup_id: record.setup_id.clone(),
            pinned_requirement: record.pinned_requirement.clone(),
            expected_protocol_name: record.expected_protocol_name.clone(),
        }))
    }

    /// Trusted-issuer constructor for the unverified class.
    pub fn mint_unverified(
        origin: ToolOrigin,
        generation: ConnectionGeneration,
        reason: UnverifiedReason,
    ) -> LaunchToken {
        LaunchToken(TokenInner::Unverified(UnverifiedLaunch {
            origin,
            generation,
            reason,
        }))
    }

    /// Readable facts — §4.1/§4.2 need them; they confer no authority.
    pub fn origin(&self) -> &ToolOrigin {
        match &self.0 {
            TokenInner::Verified(v) => &v.origin,
            TokenInner::Unverified(u) => &u.origin,
        }
    }

    pub fn generation(&self) -> ConnectionGeneration {
        match &self.0 {
            TokenInner::Verified(v) => v.generation,
            TokenInner::Unverified(u) => u.generation,
        }
    }

    pub fn class(&self) -> AttestationClass {
        match &self.0 {
            TokenInner::Verified(_) => AttestationClass::Verified,
            TokenInner::Unverified(_) => AttestationClass::Unverified,
        }
    }

    /// The scoped low-level bind (design r9 §2.2). Refuses any origin or
    /// generation that is not this token's own. Safe to be public:
    /// downstream consumers never hold a raw token — it is moved into the
    /// runtime's observation at emit and exposed only as borrows — so the
    /// only production caller is the observation owner binding against its
    /// own private fields.
    pub fn bind_scoped(
        &self,
        origin: &ToolOrigin,
        generation: ConnectionGeneration,
    ) -> Result<BoundLaunch<'_>, BindRefusal> {
        if self.origin() != origin {
            return Err(BindRefusal::OriginMismatch {
                actual: self.origin().clone(),
                requested: origin.clone(),
            });
        }
        if self.generation() != generation {
            return Err(BindRefusal::GenerationMismatch {
                actual: self.generation(),
                requested: generation,
            });
        }
        Ok(match &self.0 {
            TokenInner::Verified(v) => BoundLaunch::Verified(v),
            TokenInner::Unverified(u) => BoundLaunch::Unverified(u),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(origin: &str) -> SetupRecordFacts {
        SetupRecordFacts {
            origin: ToolOrigin::new(origin),
            setup_id: SetupIdentityId::parse("setup-1").unwrap(),
            pinned_requirement: "identifier \"com.example.mac-shell\"".into(),
            expected_protocol_name: "mac-shell".into(),
            launch_kind: LaunchKind::Direct,
            program: "/opt/homebrew/bin/mac-shell".into(),
        }
    }

    fn gen_pair() -> (ConnectionGeneration, ConnectionGeneration) {
        let mut a = awili_engine::generation::GenerationAllocator::new();
        (a.next(), a.next())
    }

    /// Gate row 18e.
    #[test]
    fn a_launch_token_from_another_generation_cannot_bind() {
        let (g1, g2) = gen_pair();
        let origin = ToolOrigin::new("mcp:mac-shell");
        let token = LaunchToken::mint_verified(&record("mcp:mac-shell"), g1);
        let err = token.bind_scoped(&origin, g2).unwrap_err();
        assert_eq!(
            err,
            BindRefusal::GenerationMismatch {
                actual: g1,
                requested: g2
            }
        );
        assert!(token.bind_scoped(&origin, g1).is_ok());
    }

    /// Gate row 24.
    #[test]
    fn a_token_bound_to_another_origin_is_refused() {
        let (g1, _) = gen_pair();
        let token = LaunchToken::mint_verified(&record("mcp:mac-shell"), g1);
        let foreign = ToolOrigin::new("mcp:other");
        let err = token.bind_scoped(&foreign, g1).unwrap_err();
        assert!(matches!(err, BindRefusal::OriginMismatch { .. }));
    }

    #[test]
    fn the_mint_copies_the_origin_from_the_record() {
        let (g1, _) = gen_pair();
        let token = LaunchToken::mint_verified(&record("mcp:mac-shell"), g1);
        assert_eq!(token.origin(), &ToolOrigin::new("mcp:mac-shell"));
        assert_eq!(token.class(), AttestationClass::Verified);
        match token
            .bind_scoped(&ToolOrigin::new("mcp:mac-shell"), g1)
            .unwrap()
        {
            BoundLaunch::Verified(v) => {
                assert_eq!(v.expected_protocol_name(), "mac-shell");
                assert_eq!(v.setup_id(), &SetupIdentityId::parse("setup-1").unwrap());
            }
            BoundLaunch::Unverified(_) => panic!("verified mint bound unverified"),
        }
    }

    #[test]
    fn an_unverified_token_carries_its_reason_and_class() {
        let (g1, _) = gen_pair();
        let token = LaunchToken::mint_unverified(
            ToolOrigin::new("mcp:mac-shell"),
            g1,
            UnverifiedReason::NoSetupRecord,
        );
        assert_eq!(token.class(), AttestationClass::Unverified);
        match token
            .bind_scoped(&ToolOrigin::new("mcp:mac-shell"), g1)
            .unwrap()
        {
            BoundLaunch::Unverified(u) => assert_eq!(u.reason(), UnverifiedReason::NoSetupRecord),
            BoundLaunch::Verified(_) => panic!("unverified mint bound verified"),
        }
    }

    #[test]
    fn setup_identity_parse_fails_closed() {
        assert_eq!(
            SetupIdentityId::parse(""),
            Err(SetupIdentityParseError::Empty)
        );
        assert_eq!(
            SetupIdentityId::parse("a\0b"),
            Err(SetupIdentityParseError::ControlBytes)
        );
        assert!(SetupIdentityId::parse("setup-1").is_ok());
    }
}
