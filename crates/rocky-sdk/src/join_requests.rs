use crate::{Failure, registry::MAX_LABEL};
use data_encoding::HEXLOWER;
use rocky_credentials::{PairRequest, Profile};
use rocky_secure::PublicKey;
use serde::{Deserialize, Serialize};

pub const MAX_PENDING: usize = 32;
const MAX_ANSWERED: usize = 32;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub node: PublicKey,
    pub user: PublicKey,
    pub nonce: String,
    pub invitation: String,
    pub name: String,
    pub routes: Vec<(String, String)>,
    pub received: u64,
    pub code: String,
    pub state: State,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum State {
    Pending,
    Admitted {
        profile: Profile,
        label: Option<Vec<u8>>,
    },
    Refused,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    Evicted,
    Expired,
    Replaced,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    Requested(Request),
    Dropped(Request, Dropped),
    Admitted(Request),
    Refused(Request),
    Enrolled { node: PublicKey, profile: Profile },
}
impl Note {
    pub fn event(&self, expires: u64) -> Option<crate::ActivityKind> {
        Some(match self {
            Self::Requested(request) => crate::ActivityKind::JoinRequested {
                request: request.view(expires),
            },
            Self::Dropped(request, reason) => crate::ActivityKind::JoinRequestDropped {
                request: request.view(expires),
                reason: *reason,
                admitted: matches!(request.state, State::Admitted { .. }),
            },
            Self::Admitted(request) => crate::ActivityKind::JoinAdmitted {
                request: request.view(expires),
            },
            Self::Refused(request) => crate::ActivityKind::JoinRefused {
                request: request.view(expires),
            },
            Self::Enrolled { .. } => return None,
        })
    }
}
impl Request {
    pub fn view(&self, expires: u64) -> crate::JoinRequest {
        crate::JoinRequest {
            node: self.node.to_hex(),
            nonce: self.nonce.clone(),
            invitation: self.invitation.clone(),
            name: self.name.clone(),
            received_epoch_seconds: self.received,
            expires_epoch_seconds: expires,
            code: self.code.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requests {
    pub invitation: Option<String>,
    requests: Vec<Request>,
}
impl Requests {
    pub fn pending(&self) -> impl Iterator<Item = &Request> {
        self.requests
            .iter()
            .filter(|request| request.state == State::Pending)
    }
    pub fn of(&self, request: &PairRequest) -> Option<&Request> {
        self.requests.iter().find(|held| {
            held.node == request.node && held.user == request.user && held.nonce == request.nonce
        })
    }
    pub fn of_mut(&mut self, node: &PublicKey) -> Option<&mut Request> {
        self.requests.iter_mut().find(|held| held.node == *node)
    }
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
            let earlier_attempt_of_same_node = self.requests.remove(index);
            notes.push(Note::Dropped(
                earlier_attempt_of_same_node,
                Dropped::Evicted,
            ));
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
        request.state = State::Admitted { profile, label };
        let admitted = request.clone();
        Ok(self.answered(Note::Admitted(admitted)))
    }
    pub fn refuse(&mut self, node: &PublicKey) -> Result<Vec<Note>, Failure> {
        let request = self
            .of_mut(node)
            .filter(|request| request.state == State::Pending)
            .ok_or("no pending join request of this node")?;
        request.state = State::Refused;
        let refused = request.clone();
        Ok(self.answered(Note::Refused(refused)))
    }
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
    pub fn end(&mut self, why: Dropped) -> Vec<Note> {
        self.invitation = None;
        self.requests
            .drain(..)
            .map(|request| Note::Dropped(request, why))
            .collect()
    }
}

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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn the_33rd_request_evicts_the_oldest_and_reports_it() {
        let issuer = PrivateKey::new().public_key();
        let mut requests = Requests::default();
        for nonce in 0..32 {
            let notes = requests
                .receive("a", &issuer, &request(nonce), u64::from(nonce))
                .expect("received");
            assert!(dropped(&notes).is_empty(), "32 requests fit the bound");
        }
        let notes = requests
            .receive("a", &issuer, &request(32), 32)
            .expect("received");
        assert_eq!(
            dropped(&notes),
            [("device 0".into(), Dropped::Evicted)],
            "the 33rd request evicts the oldest and reports it"
        );
        let names: Vec<_> = requests.pending().map(|held| held.name.clone()).collect();
        assert_eq!(
            names,
            (1..=32).map(|n| format!("device {n}")).collect::<Vec<_>>(),
            "the newest 32 requests stay"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn admission_needs_the_code_both_devices_show() {
        let issuer = PrivateKey::from_bytes(&[42; 32]).public_key();
        let asked = request(1);
        let mut requests = Requests::default();
        requests.receive("a", &issuer, &asked, 0).expect("received");
        let code = admission_code(&issuer, &asked.node, &asked.nonce);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
        assert_ne!(
            code,
            admission_code(
                &PrivateKey::from_bytes(&[43; 32]).public_key(),
                &asked.node,
                &asked.nonce,
            ),
            "another issuer shows another code; fixed keys rule out a chance collision in 6 digits"
        );
        let wrong = format!(
            "{:06}",
            (code.parse::<u32>().expect("digits") + 1) % 1_000_000
        );
        let profile = || (Profile::Issuer, None);
        assert!(
            requests.admit(&asked.node, &wrong, profile()).is_err(),
            "a code other than the one both devices show does not admit"
        );
        assert_eq!(requests.pending().count(), 1);
        requests
            .admit(&asked.node, &code, profile())
            .expect("admitted");
        assert_eq!(requests.pending().count(), 0);
        assert!(matches!(
            requests.of(&asked).map(|held| &held.state),
            Some(State::Admitted { .. })
        ));
        assert!(
            requests.admit(&asked.node, &code, profile()).is_err(),
            "a request admitted once is not admitted again"
        );
        assert!(
            requests.refuse(&asked.node).is_err(),
            "a request admitted once is not refused"
        );
    }

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
        assert_eq!(
            dropped(&notes),
            [("device 1".into(), Dropped::Replaced)],
            "a replaced invitation's request ends with it and is never admitted"
        );
        let notes = requests.end(Dropped::Expired);
        assert_eq!(dropped(&notes), [("device 2".into(), Dropped::Expired)]);
        assert_eq!(requests.pending().count(), 0);
    }
}
