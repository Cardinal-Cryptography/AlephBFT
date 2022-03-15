use crate::{
    signed::SignatureError,
    units::{FullUnit, PreUnit, SignedUnit, UncheckedSignedUnit},
    Data, Hasher, KeyBox, NodeCount, Round, SessionId, Signature,
};
use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    result::Result as StdResult,
};

/// All that can be wrong with a unit except control hash issues.
#[derive(Debug)]
pub enum ValidationError<H: Hasher, D: Data, S: Signature> {
    WrongSignature(UncheckedSignedUnit<H, D, S>),
    WrongSession(FullUnit<H, D>),
    RoundTooHigh(FullUnit<H, D>),
    UnknownCreator(FullUnit<H, D>),
    WrongNumberOfMembers(PreUnit<H>),
    RoundZeroWithParents(PreUnit<H>),
    NotEnoughParents(PreUnit<H>),
    NotDescendantOfPreviousUnit(PreUnit<H>),
}

impl<H: Hasher, D: Data, S: Signature> Display for ValidationError<H, D, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        use ValidationError::*;
        match self {
            WrongSignature(usu) => write!(f, "wrongly signed unit: {:?}", usu),
            WrongSession(fu) => write!(f, "unit from wrong session: {:?}", fu),
            RoundTooHigh(fu) => write!(f, "unit with too high round{}: {:?}", fu.round(), fu),
            UnknownCreator(fu) => write!(
                f,
                "unit by nonexistent creator {:?}: {:?}",
                fu.creator(),
                fu
            ),
            WrongNumberOfMembers(pu) => write!(
                f,
                "wrong number of members implied by unit {:?}: {:?}",
                pu.n_members(),
                pu
            ),
            RoundZeroWithParents(pu) => write!(f, "zero round unit with parents: {:?}", pu),
            NotEnoughParents(pu) => write!(
                f,
                "nonzero round unit with only {:?} parents: {:?}",
                pu.n_parents(),
                pu
            ),
            NotDescendantOfPreviousUnit(pu) => write!(
                f,
                "nonzero round unit is not descendant of its creator's previous unit: {:?}",
                pu
            ),
        }
    }
}

impl<H: Hasher, D: Data, S: Signature> From<SignatureError<FullUnit<H, D>, S>>
    for ValidationError<H, D, S>
{
    fn from(se: SignatureError<FullUnit<H, D>, S>) -> Self {
        ValidationError::WrongSignature(se.unchecked)
    }
}

pub struct Validator<'a, KB: KeyBox> {
    session_id: SessionId,
    keychain: &'a KB,
    max_round: Round,
    n_members: NodeCount,
    threshold: NodeCount,
}

type Result<'a, H, D, KB> =
    StdResult<SignedUnit<'a, H, D, KB>, ValidationError<H, D, <KB as KeyBox>::Signature>>;

impl<'a, KB: KeyBox> Validator<'a, KB> {
    pub fn new(
        session_id: SessionId,
        keychain: &'a KB,
        max_round: Round,
        n_members: NodeCount,
        threshold: NodeCount,
    ) -> Self {
        Validator {
            session_id,
            keychain,
            max_round,
            n_members,
            threshold,
        }
    }

    pub fn validate_unit<H: Hasher, D: Data>(
        &self,
        uu: UncheckedSignedUnit<H, D, KB::Signature>,
    ) -> Result<'a, H, D, KB> {
        let su = uu.check(self.keychain)?;
        let full_unit = su.as_signable();
        if full_unit.session_id() != self.session_id {
            // NOTE: this implies malicious behavior as the unit's session_id
            // is incompatible with session_id of the message it arrived in.
            return Err(ValidationError::WrongSession(full_unit.clone()));
        }
        if full_unit.round() > self.max_round {
            return Err(ValidationError::RoundTooHigh(full_unit.clone()));
        }
        if full_unit.creator().0 >= self.n_members.0 {
            return Err(ValidationError::UnknownCreator(full_unit.clone()));
        }
        self.validate_unit_parents(su)
    }

    fn validate_unit_parents<H: Hasher, D: Data>(
        &self,
        su: SignedUnit<'a, H, D, KB>,
    ) -> Result<'a, H, D, KB> {
        // NOTE: at this point we cannot validate correctness of the control hash, in principle it could be
        // just a random hash, but we still would not be able to deduce that by looking at the unit only.
        let pre_unit = su.as_signable().as_pre_unit();
        if pre_unit.n_members() != self.n_members {
            return Err(ValidationError::WrongNumberOfMembers(pre_unit.clone()));
        }
        let round = pre_unit.round();
        let n_parents = pre_unit.n_parents();
        if round == 0 && n_parents > NodeCount(0) {
            return Err(ValidationError::RoundZeroWithParents(pre_unit.clone()));
        }
        let threshold = self.threshold;
        if round > 0 && n_parents < threshold {
            return Err(ValidationError::NotEnoughParents(pre_unit.clone()));
        }
        let control_hash = &pre_unit.control_hash();
        if round > 0 && !control_hash.parents_mask[pre_unit.creator()] {
            return Err(ValidationError::NotDescendantOfPreviousUnit(
                pre_unit.clone(),
            ));
        }
        Ok(su)
    }
}
