use crate::{
    runway::Request,
    units::{UncheckedSignedUnit, ValidationError, Validator},
    Data, Hasher, KeyBox, NodeCount, NodeIndex, Receiver, Round, Sender, Signable, Signature,
    SignatureError, UncheckedSigned,
};
use codec::{Decode, Encode};
use futures::{channel::oneshot, FutureExt, StreamExt};
use log::{error, warn};
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    fmt::{Display, Formatter, Result as FmtResult},
    hash::{Hash, Hasher as _},
    time::Duration,
};

/// Salt uniquely identifying an initial unit collection instance.
pub type Salt = u64;

fn generate_salt() -> Salt {
    let mut hasher = DefaultHasher::new();
    std::time::Instant::now().hash(&mut hasher);
    hasher.finish()
}

/// A response to the request for the newest unit.
#[derive(Debug, Encode, Decode, Clone)]
pub struct NewestUnitResponse<H: Hasher, D: Data, S: Signature> {
    requester: NodeIndex,
    responder: NodeIndex,
    unit: Option<UncheckedSignedUnit<H, D, S>>,
    salt: Salt,
}

impl<H: Hasher, D: Data, S: Signature> Signable for NewestUnitResponse<H, D, S> {
    type Hash = Vec<u8>;

    fn hash(&self) -> Self::Hash {
        self.encode()
    }
}

impl<H: Hasher, D: Data, S: Signature> crate::Index for NewestUnitResponse<H, D, S> {
    fn index(&self) -> NodeIndex {
        self.responder
    }
}

impl<H: Hasher, D: Data, S: Signature> NewestUnitResponse<H, D, S> {
    /// Create a newest unit response.
    pub fn new(
        requester: NodeIndex,
        responder: NodeIndex,
        unit: Option<UncheckedSignedUnit<H, D, S>>,
        salt: Salt,
    ) -> Self {
        NewestUnitResponse {
            requester,
            responder,
            unit,
            salt,
        }
    }

    /// The unit included in this response, if any.
    pub fn included_unit(&self) -> &Option<UncheckedSignedUnit<H, D, S>> {
        &self.unit
    }

    /// The data included in this message, i.e. contents of the unit if any.
    pub fn included_data(&self) -> Vec<D> {
        match &self.unit {
            Some(u) => vec![u.as_signable().data().clone()],
            None => Vec::new(),
        }
    }

    /// Who requested this response.
    pub fn requester(&self) -> NodeIndex {
        self.requester
    }
}

/// Ways in which a newest unit response might be wrong.
#[derive(Debug)]
pub enum Error<H: Hasher, D: Data, S: Signature> {
    WrongSignature,
    SaltMismatch(Salt, Salt),
    InvalidUnit(ValidationError<H, D, S>),
    ForeignUnit(NodeIndex),
}

impl<H: Hasher, D: Data, S: Signature> Display for Error<H, D, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        use Error::*;
        match self {
            WrongSignature => write!(f, "wrong signature"),
            SaltMismatch(expected, got) => {
                write!(f, "mismatched salt, expected {}, got {}", expected, got)
            }
            InvalidUnit(e) => write!(f, "invalid unit: {}", e),
            ForeignUnit(id) => write!(f, "unit from node {:?}", id),
        }
    }
}

impl<H: Hasher, D: Data, S: Signature> From<ValidationError<H, D, S>> for Error<H, D, S> {
    fn from(ve: ValidationError<H, D, S>) -> Self {
        Error::InvalidUnit(ve)
    }
}

impl<H: Hasher, D: Data, S: Signature> From<SignatureError<NewestUnitResponse<H, D, S>, S>>
    for Error<H, D, S>
{
    fn from(_: SignatureError<NewestUnitResponse<H, D, S>, S>) -> Self {
        Error::WrongSignature
    }
}

/// Initial unit collection to figure out at which round we should start unit production.
/// Unfortunately this isn't quite BFT, but it's good enough in many situations.
pub struct Collection<'a, MK: KeyBox> {
    keychain: &'a MK,
    validator: &'a Validator<'a, MK>,
    starting_round: Round,
    responders: HashSet<NodeIndex>,
    threshold: NodeCount,
    salt: Salt,
}

impl<'a, MK: KeyBox> Collection<'a, MK> {
    /// Create a new collection instance ready to collect responses.
    /// The returned salt should be used to initiate newest unit requests.
    pub fn new(
        keychain: &'a MK,
        validator: &'a Validator<'a, MK>,
        threshold: NodeCount,
    ) -> (Self, Salt) {
        let salt = generate_salt();
        (
            Collection {
                keychain,
                validator,
                starting_round: 0,
                responders: HashSet::new(),
                threshold,
                salt,
            },
            salt,
        )
    }

    /// Process a response to a newest unit request.
    pub fn on_newest_response<H: Hasher, D: Data>(
        &mut self,
        unchecked_response: UncheckedSigned<NewestUnitResponse<H, D, MK::Signature>, MK::Signature>,
    ) -> Result<(), Error<H, D, MK::Signature>> {
        let response = unchecked_response.check(self.keychain)?.into_signable();
        if response.salt != self.salt {
            return Err(Error::SaltMismatch(self.salt, response.salt));
        }
        if let Some(unchecked_unit) = response.unit {
            let checked_signed_unit = self.validator.validate_unit(unchecked_unit)?;
            let checked_unit = checked_signed_unit.as_signable();
            if checked_unit.creator() != self.keychain.index() {
                return Err(Error::ForeignUnit(checked_unit.creator()));
            }
            let starting_round_candidate = checked_unit.round() + 1;
            if starting_round_candidate > self.starting_round {
                self.starting_round = starting_round_candidate;
            }
        }
        self.responders.insert(response.responder);
        Ok(())
    }

    /// The salt associated with this collection instance.
    pub fn salt(&self) -> Salt {
        self.salt
    }

    fn ready(&self) -> bool {
        self.responders.len() + 1 >= self.threshold.0
    }

    /// Returns the starting round if it already collected enough responses, nothing otherwise.
    pub fn starting_round(&self) -> Option<Round> {
        match self.ready() {
            true => Some(self.starting_round),
            false => None,
        }
    }
}

/// A runnable wrapper around initial unit collection.
pub struct IO<'a, H: Hasher, D: Data, MK: KeyBox> {
    round_for_creator: oneshot::Sender<Round>,
    responses_from_network:
        Receiver<UncheckedSigned<NewestUnitResponse<H, D, MK::Signature>, MK::Signature>>,
    resolved_requests: Sender<Request<H>>,
    collection: Collection<'a, MK>,
}

impl<'a, H: Hasher, D: Data, MK: KeyBox> IO<'a, H, D, MK> {
    /// Create the IO instance for the specified collection and channels associated with it.
    pub fn new(
        round_for_creator: oneshot::Sender<Round>,
        responses_from_network: Receiver<
            UncheckedSigned<NewestUnitResponse<H, D, MK::Signature>, MK::Signature>,
        >,
        resolved_requests: Sender<Request<H>>,
        collection: Collection<'a, MK>,
    ) -> Self {
        IO {
            round_for_creator,
            responses_from_network,
            resolved_requests,
            collection,
        }
    }

    /// Run the initial unit collection until it sends the initial round.
    pub async fn run(mut self) {
        let mut catch_up_delay = futures_timer::Delay::new(Duration::from_secs(5)).fuse();

        loop {
            futures::select! {
                response = self.responses_from_network.next() => {
                    let response = match response {
                        Some(response) => response,
                        None => {
                            warn!(target: "AlephBFT-runway", "Response channel closed.");
                            return;
                        }
                    };
                    match self.collection.on_newest_response(response) {
                        Ok(()) => (),
                        Err(e) => warn!(target: "AlephBFT-runway", "Received wrong newest unit response: {}", e),
                    }
                },
                _ = catch_up_delay => if let Some(round) = self.collection.starting_round() {
                    if self.round_for_creator.send(round).is_err() {
                        error!(target: "AlephBFT-runway", "unable to send starting round to creator");
                    }
                    if let Err(e) = self.resolved_requests.unbounded_send(Request::NewestUnit(self.collection.salt())) {
                        warn!(target: "AlephBFT-runway", "unable to send resolved request:  {}", e);
                    }
                    return;
                } else {
                    break;
                },
            }
        }
        loop {
            let response = match self.responses_from_network.next().await {
                Some(response) => response,
                None => {
                    warn!(target: "AlephBFT-runway", "Response channel closed.");
                    return;
                }
            };
            match self.collection.on_newest_response(response) {
                Ok(()) => {
                    if let Some(round) = self.collection.starting_round() {
                        if self.round_for_creator.send(round).is_err() {
                            error!(target: "AlephBFT-runway", "unable to send starting round to creator");
                        }
                        if let Err(e) = self
                            .resolved_requests
                            .unbounded_send(Request::NewestUnit(self.collection.salt()))
                        {
                            warn!(target: "AlephBFT-runway", "unable to send resolved request:  {}", e);
                        }
                        return;
                    }
                }
                Err(e) => {
                    warn!(target: "AlephBFT-runway", "Received wrong newest unit response: {}", e)
                }
            }
        }
    }
}