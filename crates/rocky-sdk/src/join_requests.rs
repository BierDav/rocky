//! Join requests an issuer holds for its approval invitation: devices that
//! answered `Session::invitation()` and wait until the issuer admits or
//! refuses them. They belong to the current invitation and end with it.
use crate::{Failure, registry::MAX_LABEL};
use data_encoding::HEXLOWER;
use rocky_credentials::{AuthorRange, PairRequest, Profile};
use rocky_secure::PublicKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Unanswered requests one issuer node holds; the oldest is evicted past it.
pub const MAX_PENDING: usize = 32;
/// Admitted or refused requests kept for their device's retry.
const MAX_ANSWERED: usize = 32;

/// The 6 digits both devices show before an admission: a hash of the
/// issuer's node key, the requesting node key and the request nonce.
pub fn admission_code(issuer: &PublicKey, node: &PublicKey, nonce: &str) -> String {
    let digest = rocky_core::commitment(
        "rocky.sdk.admission-code.v1",
        &[
            issuer.as_bytes().as_slice(),
            node.as_bytes(),
            nonce.as_bytes(),
        ]
        .concat(),
    );
    let mut head = [0; 8];
    head.copy_from_slice(&digest[..8]);
    format!("{:06}", u64::from_le_bytes(head) % 1_000_000)
}

/// One device's request to join through the current invitation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub node: PublicKey,
    pub user: PublicKey,
    pub nonce: String,
    pub invitation: String,
    pub name: String,
    pub routes: Vec<(String, String)>,
    /// Epoch seconds the issuer first received it.
    pub received: u64,
    pub code: String,
    pub state: State,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum State {
    Pending,
    /// Enrolled at the device's next retry, as `profile`.
    Admitted {
        profile: Profile,
        label: Option<Vec<u8>>,
        /// Carved at the first answer and kept for the retries after it.
        authors: Option<AuthorRange>,
    },
    Refused,
}
/// Why a request left the issuer without being answered by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The bound was reached; the oldest pending request made room.
    Evicted,
    /// Its invitation expired.
    Expired,
    /// A newer invitation replaced its own.
    Replaced,
}
impl Dropped {
    fn name(self) -> &'static str {
        match self {
            Self::Evicted => "evicted",
            Self::Expired => "expired",
            Self::Replaced => "replaced",
        }
    }
}
/// What the issuer's application learns from a change of its requests.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    Requested(Request),
    Dropped(Request, Dropped),
    Admitted(Request),
    Refused(Request),
    Enrolled { node: PublicKey, profile: Profile },
}
impl Note {
    /// The activity event that surfaces it.
    pub fn event(&self, expires: u64) -> Option<(&'static str, Value)> {
        Some(match self {
            Self::Requested(request) => ("join_requested", request.view(expires)),
            Self::Dropped(request, why) => {
                let mut view = request.view(expires);
                view["reason"] = why.name().into();
                view["admitted"] = matches!(request.state, State::Admitted { .. }).into();
                ("join_request_dropped", view)
            }
            Self::Admitted(request) => ("join_admitted", request.view(expires)),
            Self::Refused(request) => ("join_refused", request.view(expires)),
            Self::Enrolled { .. } => return None,
        })
    }
}
impl Request {
    /// The request as `discovered` lists it; `expires` is its invitation's.
    pub fn view(&self, expires: u64) -> Value {
        json!({
            "node": self.node.to_hex(),
            "nonce": self.nonce,
            "invitation": self.invitation,
            "name": self.name,
            "received": self.received.to_string(),
            "expires": expires.to_string(),
            "code": self.code,
        })
    }
}

/// The current invitation's requests, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requests {
    /// The digest of the invitation token they answered, as the registry
    /// records it.
    pub invitation: Option<String>,
    requests: Vec<Request>,
}
impl Requests {
    pub fn pending(&self) -> impl Iterator<Item = &Request> {
        self.requests
            .iter()
            .filter(|request| request.state == State::Pending)
    }
    /// The request `request`'s device made before, with the same keys and nonce.
    pub fn of(&self, request: &PairRequest) -> Option<&Request> {
        self.requests.iter().find(|held| {
            held.node == request.node && held.user == request.user && held.nonce == request.nonce
        })
    }
    pub fn of_mut(&mut self, node: &PublicKey) -> Option<&mut Request> {
        self.requests.iter_mut().find(|held| held.node == *node)
    }
    /// Holds a new request of `invitation`, evicting the oldest pending one
    /// at the bound and any earlier request of the same node.
    pub fn receive(
        &mut self,
        invitation: &str,
        issuer: &PublicKey,
        request: &PairRequest,
        time: u64,
    ) -> Result<Vec<Note>, Failure> {
        if request.name.len() > MAX_LABEL {
            return Err(format!("a device name holds at most {MAX_LABEL} bytes").into());
        }
        let mut notes = Vec::new();
        if self.invitation.as_deref() != Some(invitation) {
            notes.extend(self.end(Dropped::Replaced));
            self.invitation = Some(invitation.into());
        }
        if let Some(index) = self
            .requests
            .iter()
            .position(|held| held.node == request.node)
        {
            // The same node with another nonce is another join attempt.
            let earlier = self.requests.remove(index);
            notes.push(Note::Dropped(earlier, Dropped::Evicted));
        }
        if self.pending().count() >= MAX_PENDING
            && let Some(index) = self
                .requests
                .iter()
                .position(|held| held.state == State::Pending)
        {
            notes.push(Note::Dropped(self.requests.remove(index), Dropped::Evicted));
        }
        let held = Request {
            node: request.node,
            user: request.user,
            nonce: request.nonce.clone(),
            invitation: invitation.into(),
            name: request.name.clone(),
            routes: request.routes.clone(),
            received: time,
            code: admission_code(issuer, &request.node, &request.nonce),
            state: State::Pending,
        };
        self.requests.push(held.clone());
        notes.push(Note::Requested(held));
        Ok(notes)
    }
    /// Marks `node`'s pending request admitted when `code` is its code.
    pub fn admit(
        &mut self,
        node: &PublicKey,
        code: &str,
        (profile, label): (Profile, Option<Vec<u8>>),
    ) -> Result<Vec<Note>, Failure> {
        let request = self
            .of_mut(node)
            .filter(|request| request.state == State::Pending)
            .ok_or("no pending join request of this node")?;
        if request.code != code {
            return Err("the admission code differs from the requesting device's".into());
        }
        request.state = State::Admitted {
            profile,
            label,
            authors: None,
        };
        let admitted = request.clone();
        Ok(self.answered(Note::Admitted(admitted)))
    }
    /// Refuses `node`'s pending request; its device learns at its retry.
    pub fn refuse(&mut self, node: &PublicKey) -> Result<Vec<Note>, Failure> {
        let request = self
            .of_mut(node)
            .filter(|request| request.state == State::Pending)
            .ok_or("no pending join request of this node")?;
        request.state = State::Refused;
        let refused = request.clone();
        Ok(self.answered(Note::Refused(refused)))
    }
    /// Keeps at most `MAX_ANSWERED` answered requests, the newest.
    fn answered(&mut self, note: Note) -> Vec<Note> {
        let mut notes = vec![note];
        while self
            .requests
            .iter()
            .filter(|request| request.state != State::Pending)
            .count()
            > MAX_ANSWERED
        {
            let Some(index) = self
                .requests
                .iter()
                .position(|request| request.state != State::Pending)
            else {
                break;
            };
            notes.push(Note::Dropped(self.requests.remove(index), Dropped::Evicted));
        }
        notes
    }
    /// Drops every request, as their invitation ended for `why`.
    pub fn end(&mut self, why: Dropped) -> Vec<Note> {
        self.invitation = None;
        self.requests
            .drain(..)
            .map(|request| Note::Dropped(request, why))
            .collect()
    }
}

/// The digest of an invitation token, as requests and the registry name it.
pub fn invitation_digest(token: &[u8; 32]) -> String {
    HEXLOWER.encode(&rocky_core::commitment("rocky.sdk.invitation.v1", token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_secure::PrivateKey;

    fn request(nonce: u8) -> PairRequest {
        PairRequest::new(
            &[7; 32],
            format!("{nonce:02x}").repeat(32),
            (
                PrivateKey::from_bytes(&[nonce; 32]).public_key(),
                PrivateKey::from_bytes(&[nonce.wrapping_add(1); 32]).public_key(),
            ),
            format!("device {nonce}"),
            [3; 32],
        )
    }
    fn dropped(notes: &[Note]) -> Vec<(String, Dropped)> {
        notes
            .iter()
            .filter_map(|note| match note {
                Note::Dropped(request, why) => Some((request.name.clone(), *why)),
                _ => None,
            })
            .collect()
    }

    // Mutation: evicting the newest instead of the oldest, or a bound of 33,
    // fails the kept names and the reported eviction.
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_33rd_request_evicts_the_oldest_and_reports_it() {
        let issuer = PrivateKey::new().public_key();
        let mut requests = Requests::default();
        for nonce in 0..32 {
            let notes = requests
                .receive("a", &issuer, &request(nonce), u64::from(nonce))
                .expect("received");
            assert!(dropped(&notes).is_empty());
        }
        let notes = requests
            .receive("a", &issuer, &request(32), 32)
            .expect("received");
        assert_eq!(dropped(&notes), [("device 0".into(), Dropped::Evicted)]);
        let names: Vec<_> = requests.pending().map(|held| held.name.clone()).collect();
        assert_eq!(
            names,
            (1..=32).map(|n| format!("device {n}")).collect::<Vec<_>>()
        );
    }

    // Mutation: dropping the code comparison admits with any code.
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn admission_needs_the_code_both_devices_show() {
        let issuer = PrivateKey::from_bytes(&[42; 32]).public_key();
        let asked = request(1);
        let mut requests = Requests::default();
        requests.receive("a", &issuer, &asked, 0).expect("received");
        let code = admission_code(&issuer, &asked.node, &asked.nonce);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
        // Fixed keys avoid random collisions in the 6-digit space.
        assert_ne!(
            code,
            admission_code(
                &PrivateKey::from_bytes(&[43; 32]).public_key(),
                &asked.node,
                &asked.nonce,
            )
        );
        let wrong = format!(
            "{:06}",
            (code.parse::<u32>().expect("digits") + 1) % 1_000_000
        );
        let profile = || (Profile::Issuer, None);
        assert!(requests.admit(&asked.node, &wrong, profile()).is_err());
        assert_eq!(requests.pending().count(), 1);
        requests
            .admit(&asked.node, &code, profile())
            .expect("admitted");
        assert_eq!(requests.pending().count(), 0);
        assert!(matches!(
            requests.of(&asked).map(|held| &held.state),
            Some(State::Admitted { .. })
        ));
        // Admitted once: neither admitted nor refused again.
        assert!(requests.admit(&asked.node, &code, profile()).is_err());
        assert!(requests.refuse(&asked.node).is_err());
    }

    // Mutation: keeping requests across invitations lets a replaced
    // invitation's request be admitted.
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn requests_end_with_their_invitation() {
        let issuer = PrivateKey::new().public_key();
        let mut requests = Requests::default();
        requests
            .receive("a", &issuer, &request(1), 0)
            .expect("received");
        let notes = requests
            .receive("b", &issuer, &request(2), 1)
            .expect("received");
        assert_eq!(dropped(&notes), [("device 1".into(), Dropped::Replaced)]);
        let notes = requests.end(Dropped::Expired);
        assert_eq!(dropped(&notes), [("device 2".into(), Dropped::Expired)]);
        assert_eq!(requests.pending().count(), 0);
    }
}
