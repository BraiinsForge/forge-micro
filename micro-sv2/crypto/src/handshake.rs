//! Noise NX handshake state machine.
//!
//! Pattern:
//! ```text
//! Noise_NX(rs):
//!   -> e
//!   <- e, ee, s, es
//! ```

use crate::{
    error::NXError, keygen::KeyGen, keys::Secp256k1KeyPair, transport::TransportState,
};

pub enum InitiatorState {
    /// Ephemeral key not yet sent.
    Initial,
    /// `-> e` sent, awaiting `<- e, ee, s, es`.
    AwaitingResponse,
    /// Handshake complete.
    Done,
}

pub enum ResponderState {
    /// Awaiting `-> e` from initiator.
    AwaitingInitial,
    /// `<- e, ee, s, es` sent, transport ready.
    Done,
}

pub struct HandShake<R: KeyGen> {
    // TODO: static keypair, ephemeral keypair, chaining key, handshake hash,
    // current cipher key, RNG, role-specific state machine.
    _phantom: core::marker::PhantomData<R>,
}

impl<R: KeyGen> HandShake<R> {
    pub fn new_initiator(_static_keypair: Secp256k1KeyPair, _rng: R) -> Self {
        todo!()
    }

    pub fn new_responder(_static_keypair: Secp256k1KeyPair, _rng: R) -> Self {
        todo!()
    }

    /// Initiator: write the first handshake message (`-> e`). Returns bytes written.
    pub fn write_initial(&mut self, _out: &mut [u8]) -> Result<usize, NXError> {
        todo!()
    }

    /// Initiator: process the responder's reply (`<- e, ee, s, es`).
    /// Yields the post-handshake transport state.
    pub fn read_response(&mut self, _input: &mut [u8]) -> Result<TransportState, NXError> {
        todo!()
    }

    /// Responder: process the initiator's first message (`-> e`).
    pub fn read_initial(&mut self, _input: &mut [u8]) -> Result<(), NXError> {
        todo!()
    }

    /// Responder: write the reply (`<- e, ee, s, es`).
    /// Yields the post-handshake transport state.
    pub fn write_response(&mut self, _out: &mut [u8]) -> Result<TransportState, NXError> {
        todo!()
    }
}
