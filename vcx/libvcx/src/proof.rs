use serde_json;
use serde_json::Value;
use openssl;
use openssl::bn::{BigNum, BigNumRef};

use settings;
use connection;
use api::{VcxStateType, ProofStateType};
use messages;
use messages::proofs::proof_message::{ProofMessage, CredInfo};
use messages::{RemoteMessageType, GeneralMessage};
use messages::payload::{Payloads, PayloadKinds};
use messages::thread::Thread;
use messages::proofs::proof_request::{ProofRequestMessage, ProofRequestVersion};
use utils::error;
use utils::constants::*;
use utils::libindy::anoncreds;
use object_cache::ObjectCache;
use error::prelude::*;
use utils::openssl::encode;
use utils::qualifier::Qualifier;
use messages::proofs::proof_message::get_credential_info;

use v3::handlers::proof_presentation::verifier::verifier::Verifier;

lazy_static! {
    static ref PROOF_MAP: ObjectCache<Proofs> = Default::default();
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "version", content = "data")]
enum Proofs {
    #[serde(rename = "1.0")]
    V1(Proof),
    #[serde(rename = "2.0")]
    V3(Verifier),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct RevocationInterval {
    from: Option<u64>,
    to: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Proof {
    source_id: String,
    requested_attrs: String,
    requested_predicates: String,
    msg_uid: String,
    ref_msg_id: String,
    prover_did: String,
    prover_vk: String,
    state: VcxStateType,
    proof_state: ProofStateType,
    name: String,
    version: String,
    nonce: String,
    proof: Option<ProofMessage>,
    // Refactoring this name to 'proof_message' causes some tests to fail.
    proof_request: Option<ProofRequestMessage>,
    remote_did: String,
    remote_vk: String,
    agent_did: String,
    agent_vk: String,
    revocation_interval: RevocationInterval,
    thread: Option<Thread>
}

impl Proof {
    // leave this returning a u32 until we actually implement this method to do something
    // other than return success.
    fn validate_proof_request(&self) -> VcxResult<u32> {
        //TODO: validate proof request
        Ok(error::SUCCESS.code_num)
    }


    pub fn validate_proof_revealed_attributes(proof_json: &str) -> VcxResult<()> {
        if settings::test_indy_mode_enabled() { return Ok(()); }

        let proof: Value = serde_json::from_str(proof_json)
            .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidJson, format!("Cannot deserialize liibndy proof: {}", err)))?;

        let revealed_attrs = match proof["requested_proof"]["revealed_attrs"].as_object() {
            Some(revealed_attrs) => revealed_attrs,
            None => return Ok(())
        };

        for (attr1_referent, info) in revealed_attrs.iter() {
            let raw = info["raw"].as_str().ok_or(VcxError::from_msg(VcxErrorKind::InvalidProof, format!("Cannot get raw value for \"{}\" attribute", attr1_referent)))?;
            let encoded_ = info["encoded"].as_str().ok_or(VcxError::from_msg(VcxErrorKind::InvalidProof, format!("Cannot get encoded value for \"{}\" attribute", attr1_referent)))?;

            let expected_encoded = encode(&raw)?;

            if expected_encoded != encoded_.to_string() {
                return Err(VcxError::from_msg(VcxErrorKind::InvalidProof, format!("Encoded values are different. Expected: {}. From Proof: {}", expected_encoded, encoded_)));
            }
        }

        Ok(())
    }

    pub fn build_credential_defs_json(credential_data: &Vec<CredInfo>) -> VcxResult<String> {
        debug!("building credential_def_json for proof validation");
        let mut credential_json = json!({});

        for ref cred_info in credential_data.iter() {
            if credential_json.get(&cred_info.cred_def_id).is_none() {
                let (id, credential_def) = anoncreds::get_cred_def_json(&cred_info.cred_def_id)?;

                let credential_def = serde_json::from_str(&credential_def)
                    .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidProofCredentialData, format!("Cannot deserialize credential definition: {}", err)))?;

                credential_json[id] = credential_def;
            }
        }

        Ok(credential_json.to_string())
    }

    pub fn build_schemas_json(credential_data: &Vec<CredInfo>) -> VcxResult<String> {
        debug!("building schemas json for proof validation");

        let mut schemas_json = json!({});

        for ref cred_info in credential_data.iter() {
            if schemas_json.get(&cred_info.schema_id).is_none() {
                let (id, schema_json) = anoncreds::get_schema_json(&cred_info.schema_id)
                    .map_err(|err| err.map(VcxErrorKind::InvalidSchema, "Cannot get schema"))?;

                let schema_val = serde_json::from_str(&schema_json)
                    .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidSchema, format!("Cannot deserialize schema: {}", err)))?;

                schemas_json[id] = schema_val;
            }
        }

        Ok(schemas_json.to_string())
    }

    pub fn build_rev_reg_defs_json(credential_data: &Vec<CredInfo>) -> VcxResult<String> {
        debug!("building rev_reg_def_json for proof validation");

        let mut rev_reg_defs_json = json!({});

        for ref cred_info in credential_data.iter() {
            let rev_reg_id = cred_info
                .rev_reg_id
                .as_ref()
                .ok_or(VcxError::from(VcxErrorKind::InvalidRevocationDetails))?;

            if rev_reg_defs_json.get(rev_reg_id).is_none() {
                let (id, json) = anoncreds::get_rev_reg_def_json(rev_reg_id)
                    .or(Err(VcxError::from(VcxErrorKind::InvalidRevocationDetails)))?;

                let rev_reg_def_json = serde_json::from_str(&json)
                    .or(Err(VcxError::from(VcxErrorKind::InvalidSchema)))?;

                rev_reg_defs_json[id] = rev_reg_def_json;
            }
        }

        Ok(rev_reg_defs_json.to_string())
    }

    pub fn build_rev_reg_json(credential_data: &Vec<CredInfo>) -> VcxResult<String> {
        debug!("building rev_reg_json for proof validation");

        let mut rev_regs_json = json!({});

        for ref cred_info in credential_data.iter() {
            let rev_reg_id = cred_info
                .rev_reg_id
                .as_ref()
                .ok_or(VcxError::from(VcxErrorKind::InvalidRevocationDetails))?;

            let timestamp = cred_info
                .timestamp
                .as_ref()
                .ok_or(VcxError::from(VcxErrorKind::InvalidRevocationTimestamp))?;

            if rev_regs_json.get(rev_reg_id).is_none() {
                let (id, json, timestamp) = anoncreds::get_rev_reg(rev_reg_id, timestamp.to_owned())
                    .or(Err(VcxError::from(VcxErrorKind::InvalidRevocationDetails)))?;

                let rev_reg_json: Value = serde_json::from_str(&json)
                    .or(Err(VcxError::from(VcxErrorKind::InvalidJson)))?;

                let rev_reg_json = json!({timestamp.to_string(): rev_reg_json});
                rev_regs_json[id] = rev_reg_json;
            }
        }

        Ok(rev_regs_json.to_string())
    }

    fn build_proof_json(&self) -> VcxResult<String> {
        debug!("{} building proof json for proof validation", self.source_id);
        match self.proof {
            Some(ref x) => Ok(x.libindy_proof.clone()),
            None => Err(VcxError::from(VcxErrorKind::InvalidProof)),
        }
    }

    fn build_proof_req_json(&self) -> VcxResult<String> {
        debug!("{} building proof request json for proof validation", self.source_id);
        if let Some(ref x) = self.proof_request {
            return Ok(x.get_proof_request_data());
        }
        Err(VcxError::from(VcxErrorKind::InvalidProof))
    }

    fn proof_validation(&mut self) -> VcxResult<u32> {
        let proof_json = self.build_proof_json()?;
        let proof_req_json = self.build_proof_req_json()?;

        let valid = Proof::validate_indy_proof(&proof_json, &proof_req_json).map_err(|err| {
            error!("Error: {}, Proof {} wasn't valid", err, self.source_id);
            self.proof_state = ProofStateType::ProofInvalid;
            err.map(VcxErrorKind::InvalidProof, error::INVALID_PROOF.message)
        })?;

        if !valid {
            warn!("indy returned false when validating proof {}", self.source_id);
            self.proof_state = ProofStateType::ProofInvalid;
            return Ok(error::SUCCESS.code_num);
        }

        debug!("Indy validated proof: {}", self.source_id);
        self.proof_state = ProofStateType::ProofValidated;
        Ok(error::SUCCESS.code_num)
    }

    pub fn validate_indy_proof(proof_json: &str, proof_req_json: &str) -> VcxResult<bool> {
        if settings::test_indy_mode_enabled() { return Ok(true); }

        Proof::validate_proof_revealed_attributes(&proof_json)?;

        let credential_data = get_credential_info(&proof_json)?;

        let credential_defs_json = Proof::build_credential_defs_json(&credential_data)
            .unwrap_or(json!({}).to_string());
        let schemas_json = Proof::build_schemas_json(&credential_data)
            .unwrap_or(json!({}).to_string());
        let rev_reg_defs_json = Proof::build_rev_reg_defs_json(&credential_data)
            .unwrap_or(json!({}).to_string());
        let rev_regs_json = Proof::build_rev_reg_json(&credential_data)
            .unwrap_or(json!({}).to_string());

        debug!("*******\n{}\n********", credential_defs_json);
        debug!("*******\n{}\n********", schemas_json);
        debug!("*******\n{}\n********", proof_json);
        debug!("*******\n{}\n********", proof_req_json);
        debug!("*******\n{}\n********", rev_reg_defs_json);
        debug!("*******\n{}\n********", rev_regs_json);
        anoncreds::libindy_verifier_verify_proof(proof_req_json,
                                                 proof_json,
                                                 &schemas_json,
                                                 &credential_defs_json,
                                                 &rev_reg_defs_json,
                                                 &rev_regs_json)
    }

    fn generate_proof_request_msg(&mut self) -> VcxResult<String> {
        let proof_req_format_version = if Qualifier::is_fully_qualified(&self.remote_did) { Some(ProofRequestVersion::V2) } else { None };

        let data_version = "0.1";
        let mut proof_obj = messages::proof_request();
        let proof_request = proof_obj
            .type_version(&self.version)?
            .proof_request_format_version(proof_req_format_version)?
            .nonce(&self.nonce)?
            .proof_name(&self.name)?
            .proof_data_version(data_version)?
            .requested_attrs(&self.requested_attrs)?
            .requested_predicates(&self.requested_predicates)?
            .from_timestamp(self.revocation_interval.from)?
            .to_timestamp(self.revocation_interval.to)?
            .serialize_message()?;

        self.proof_request = Some(proof_obj);
        Ok(proof_request)
    }

    fn send_proof_request(&mut self, connection_handle: u32) -> VcxResult<u32> {
        trace!("Proof::send_proof_request >>> connection_handle: {}", connection_handle);

        if self.state != VcxStateType::VcxStateInitialized {
            warn!("proof {} has invalid state {} for sending proofRequest", self.source_id, self.state as u32);
            return Err(VcxError::from(VcxErrorKind::NotReady));
        }
        debug!("sending proof request with proof: {}, and connection {}", self.source_id, connection_handle);
        self.prover_did = connection::get_pw_did(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;
        self.agent_did = connection::get_agent_did(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;
        self.agent_vk = connection::get_agent_verkey(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;
        self.remote_did = connection::get_their_pw_did(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;
        self.remote_vk = connection::get_their_pw_verkey(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;
        self.prover_vk = connection::get_pw_verkey(connection_handle).or(Err(VcxError::from(VcxErrorKind::GeneralConnectionError)))?;

        debug!("prover_did: {} -- agent_did: {} -- agent_vk: {} -- remote_vk: {} -- prover_vk: {}",
               self.prover_did,
               self.agent_did,
               self.agent_vk,
               self.remote_vk,
               self.prover_vk);

        let title = format!("{} wants you to share: {}", settings::get_config_value(settings::CONFIG_INSTITUTION_NAME)?, self.name);

        let proof_request = self.generate_proof_request_msg()?;

        let response = messages::send_message()
            .to(&self.prover_did)?
            .to_vk(&self.prover_vk)?
            .msg_type(&RemoteMessageType::ProofReq)?
            .agent_did(&self.agent_did)?
            .set_title(&title)?
            .set_detail(&title)?
            .agent_vk(&self.agent_vk)?
            .edge_agent_payload(&self.prover_vk, &self.remote_vk, &proof_request, PayloadKinds::ProofRequest, self.thread.clone()).or(Err(VcxError::from(VcxErrorKind::InvalidConnectionHandle)))?
            .send_secure()
            .map_err(|err| err.extend("Cannot send proof request"))?;

        self.msg_uid = response.get_msg_uid()?;
        self.state = VcxStateType::VcxStateOfferSent;
        Ok(error::SUCCESS.code_num)
    }

    fn get_proof(&self) -> VcxResult<String> {
        Ok(self.proof.as_ref().ok_or(VcxError::from(VcxErrorKind::InvalidProofHandle))?.libindy_proof.clone())
    }

    fn get_proof_request_status(&mut self, message: Option<String>) -> VcxResult<u32> {
        debug!("updating state for proof {} with msg_id {:?}", self.source_id, self.msg_uid);
        if self.state == VcxStateType::VcxStateAccepted {
            return Ok(self.get_state());
        } else if message.is_none() && (self.state != VcxStateType::VcxStateOfferSent || self.msg_uid.is_empty() || self.prover_did.is_empty()) {
            return Ok(self.get_state());
        }


        let payload = match message {
            None => {
                // Check cloud agent for pending messages
                let (_, message) = messages::get_message::get_ref_msg(&self.msg_uid,
                                                                      &self.prover_did,
                                                                      &self.prover_vk,
                                                                      &self.agent_did,
                                                                      &self.agent_vk)?;

                let (payload, thread) = Payloads::decrypt(&self.prover_vk, &message)?;

                if let Some(_) = thread {
                    let remote_did = self.remote_did.as_str();
                    self.thread.as_mut().map(|thread| thread.increment_receiver(&remote_did));
                }

                payload
            }
            Some(ref message) => message.clone(),
        };
        debug!("proof: {}", payload);

        self.proof = match parse_proof_payload(&payload) {
            Err(_) => return Ok(self.get_state()),
            Ok(x) => Some(x),
        };

        self.state = VcxStateType::VcxStateAccepted;

        match self.proof_validation() {
            Ok(_) => {
                if self.proof_state != ProofStateType::ProofInvalid {
                    debug!("Proof format was validated for proof {}", self.source_id);
                    self.proof_state = ProofStateType::ProofValidated;
                }
            }
            Err(x) => {
                self.state = VcxStateType::VcxStateRequestReceived;
                warn!("Proof {} had invalid format with err {}", self.source_id, x);
                self.proof_state = ProofStateType::ProofInvalid;
            }
        };

        Ok(self.get_state())
    }

    fn update_state(&mut self, message: Option<String>) -> VcxResult<u32> {
        trace!("Proof::update_state >>>");
        self.get_proof_request_status(message)
    }

    fn get_state(&self) -> u32 {
        trace!("Proof::get_state >>>");
        self.state as u32
    }

    fn get_proof_state(&self) -> u32 {
        self.proof_state as u32
    }

    fn get_proof_uuid(&self) -> &String { &self.msg_uid }

    fn get_source_id(&self) -> String { self.source_id.to_string() }

    #[cfg(test)]
    fn from_str(data: &str) -> VcxResult<Proof> {
        use messages::ObjectWithVersion;
        ObjectWithVersion::deserialize(data)
            .map(|obj: ObjectWithVersion<Proof>| obj.data)
            .map_err(|err| err.extend("Cannot deserialize Proof"))
    }
}

pub fn create_proof(source_id: String,
                    requested_attrs: String,
                    requested_predicates: String,
                    revocation_details: String,
                    name: String) -> VcxResult<u32> {
    // Initiate proof of new format -- redirect to v3 folder
    if settings::ARIES_COMMUNICATION_METHOD.to_string() == settings::get_communication_method().unwrap_or_default() {
        let verifier = Verifier::create(source_id, requested_attrs, requested_predicates, revocation_details, name)?;
        return PROOF_MAP.add(Proofs::V3(verifier))
            .or(Err(VcxError::from(VcxErrorKind::CreateProof)));
    }

    trace!("create_proof >>> source_id: {}, requested_attrs: {}, requested_predicates: {}, name: {}", source_id, requested_attrs, requested_predicates, name);

    // TODO: Get this to actually validate as json, not just check length.
    if requested_attrs.len() <= 0 { return Err(VcxError::from(VcxErrorKind::InvalidJson)); }

    let revocation_details: RevocationInterval = serde_json::from_str(&revocation_details)
        .or(Err(VcxError::from(VcxErrorKind::InvalidJson)))?;

    debug!("creating proof with source_id: {}, name: {}, requested_attrs: {}, requested_predicates: {}", source_id, name, requested_attrs, requested_predicates);

    let mut new_proof = Proof {
        source_id,
        msg_uid: String::new(),
        ref_msg_id: String::new(),
        requested_attrs,
        requested_predicates,
        prover_did: String::new(),
        prover_vk: String::new(),
        state: VcxStateType::VcxStateNone,
        proof_state: ProofStateType::ProofUndefined,
        name,
        version: String::from("1.0"),
        nonce: generate_nonce()?,
        proof: None,
        proof_request: None,
        remote_did: String::new(),
        remote_vk: String::new(),
        agent_did: String::new(),
        agent_vk: String::new(),
        revocation_interval: revocation_details,
        thread: Some(Thread::new()),
    };

    new_proof.validate_proof_request()?;

    new_proof.state = VcxStateType::VcxStateInitialized;

    PROOF_MAP.add(Proofs::V1(new_proof))
        .or(Err(VcxError::from(VcxErrorKind::CreateProof)))
}

pub fn is_valid_handle(handle: u32) -> bool {
    PROOF_MAP.has_handle(handle)
}

pub fn update_state(handle: u32, message: Option<String>) -> VcxResult<u32> {
    PROOF_MAP.get_mut(handle, |obj| {
        match obj {
            Proofs::V1(ref mut obj) => {
                obj.update_state(message.clone())?;
                Ok(obj.get_state())
            }
            Proofs::V3(ref mut obj) => {
                obj.update_state(message.as_ref().map(String::as_str))?;
                Ok(obj.state())
            }
        }
    })
}

pub fn get_state(handle: u32) -> VcxResult<u32> {
    PROOF_MAP.get(handle, |obj| {
        match obj {
            Proofs::V1(ref obj) => Ok(obj.get_state()),
            Proofs::V3(ref obj) => Ok(obj.state())
        }
    })
}

pub fn get_proof_state(handle: u32) -> VcxResult<u32> {
    PROOF_MAP.get(handle, |obj| {
        match obj {
            Proofs::V1(ref obj) => Ok(obj.get_proof_state()),
            Proofs::V3(ref obj) => Ok(obj.presentation_status())
        }
    })
}

pub fn release(handle: u32) -> VcxResult<()> {
    PROOF_MAP.release(handle).or(Err(VcxError::from(VcxErrorKind::InvalidProofHandle)))
}

pub fn release_all() {
    PROOF_MAP.drain().ok();
}

pub fn to_string(handle: u32) -> VcxResult<String> {
    PROOF_MAP.get(handle, |obj| {
        serde_json::to_string(obj)
            .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidState, format!("cannot serialize Proof object: {:?}", err)))
    })
}

pub fn get_source_id(handle: u32) -> VcxResult<String> {
    PROOF_MAP.get(handle, |obj| {
        match obj {
            Proofs::V1(ref obj) => Ok(obj.get_source_id()),
            Proofs::V3(ref obj) => Ok(obj.get_source_id())
        }
    })
}

pub fn from_string(proof_data: &str) -> VcxResult<u32> {
    let proof: Proofs = serde_json::from_str(proof_data)
        .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidJson, format!("cannot deserialize Proofs object: {:?}", err)))?;

    PROOF_MAP.add(proof)
}

pub fn generate_proof_request_msg(handle: u32) -> VcxResult<String> {
    PROOF_MAP.get_mut(handle, |obj| {
        match obj {
            Proofs::V1(ref mut obj) => obj.generate_proof_request_msg(),
            Proofs::V3(ref obj) => obj.generate_presentation_request_msg()
        }
    })
}

pub fn send_proof_request(handle: u32, connection_handle: u32) -> VcxResult<u32> {
    PROOF_MAP.get_mut(handle, |obj| {
        match obj {
            Proofs::V1(ref mut obj) => {
                obj.send_proof_request(connection_handle)
            }
            Proofs::V3(ref mut obj) => {
                obj.send_presentation_request(connection_handle)?;
                Ok(error::SUCCESS.code_num)
            }
        }
    })
}

pub fn get_proof_uuid(handle: u32) -> VcxResult<String> {
    PROOF_MAP.get(handle, |obj| {
        match obj {
            Proofs::V1(ref obj) => Ok(obj.get_proof_uuid().clone()),
            Proofs::V3(_) => Err(VcxError::from(VcxErrorKind::InvalidProofHandle))
        }
    })
}

fn parse_proof_payload(payload: &str) -> VcxResult<ProofMessage> {
    let my_credential_req = ProofMessage::from_str(&payload)
        .map_err(|err| VcxError::from_msg(VcxErrorKind::InvalidJson, format!("Cannot deserialize ProofMessage: {}", err)))?;
    Ok(my_credential_req)
}

pub fn get_proof(handle: u32) -> VcxResult<String> {
    PROOF_MAP.get(handle, |obj| {
        match obj {
            Proofs::V1(ref obj) => obj.get_proof(),
            Proofs::V3(ref obj) => obj.get_presentation()
        }
    })
}

// TODO: This doesnt feel like it should be here (maybe utils?)
pub fn generate_nonce() -> VcxResult<String> {
    let mut bn = BigNum::new().map_err(|err| VcxError::from_msg(VcxErrorKind::EncodeError, format!("Cannot generate nonce: {}", err)))?;

    BigNumRef::rand(&mut bn, LARGE_NONCE as i32, openssl::bn::MsbOption::MAYBE_ZERO, false)
        .map_err(|err| VcxError::from_msg(VcxErrorKind::EncodeError, format!("Cannot generate nonce: {}", err)))?;
    Ok(bn.to_dec_str()
        .map_err(|err| VcxError::from_msg(VcxErrorKind::EncodeError, format!("Cannot generate nonce: {}", err)))?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use utils::httpclient;
    use connection::tests::build_test_connection;
    use utils::libindy::{pool, set_libindy_rc};

    fn create_boxed_proof() -> Box<Proof> {
        Box::new(Proof {
            source_id: "12".to_string(),
            msg_uid: String::from("1234"),
            ref_msg_id: String::new(),
            requested_attrs: String::from("[]"),
            requested_predicates: String::from("[]"),
            prover_did: String::from("GxtnGN6ypZYgEqcftSQFnC"),
            prover_vk: VERKEY.to_string(),
            state: VcxStateType::VcxStateOfferSent,
            proof_state: ProofStateType::ProofUndefined,
            name: String::new(),
            version: String::from("1.0"),
            nonce: generate_nonce().unwrap(),
            proof: None,
            proof_request: None,
            remote_did: DID.to_string(),
            remote_vk: VERKEY.to_string(),
            agent_did: DID.to_string(),
            agent_vk: VERKEY.to_string(),
            revocation_interval: RevocationInterval { from: None, to: None },
            thread: Some(Thread::new()),
        })
    }

    #[test]
    fn test_create_proof_succeeds() {
        init!("true");

        create_proof("1".to_string(),
                     REQUESTED_ATTRS.to_owned(),
                     REQUESTED_PREDICATES.to_owned(),
                     r#"{"support_revocation":false}"#.to_string(),
                     "Optional".to_owned()).unwrap();
    }

    #[test]
    fn test_revocation_details() {
        init!("true");

        // No Revocation
        create_proof("1".to_string(),
                     REQUESTED_ATTRS.to_owned(),
                     REQUESTED_PREDICATES.to_owned(),
                     r#"{"support_revocation":false}"#.to_string(),
                     "Optional".to_owned()).unwrap();

        // Support Revocation Success
        let revocation_details = json!({
            "to": 1234,
        });
        create_proof("1".to_string(),
                     REQUESTED_ATTRS.to_owned(),
                     REQUESTED_PREDICATES.to_owned(),
                     revocation_details.to_string(),
                     "Optional".to_owned()).unwrap();
    }

    #[test]
    fn test_nonce() {
        let nonce = generate_nonce().unwrap();
        assert!(BigNum::from_dec_str(&nonce).unwrap().num_bits() < 81)
    }

    #[test]
    fn test_to_string_succeeds() {
        init!("true");
        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        let proof_string = to_string(handle).unwrap();
        let s: Value = serde_json::from_str(&proof_string).unwrap();
        assert_eq!(s["version"], DEFAULT_SERIALIZE_VERSION);
        assert!(!proof_string.is_empty());
    }

    #[test]
    fn test_from_string_succeeds() {
        init!("true");
        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        let proof_data = to_string(handle).unwrap();
        let proof1: Proof = Proof::from_str(&proof_data).unwrap();
        assert!(release(handle).is_ok());

        let new_handle = from_string(&proof_data).unwrap();
        let proof2: Proof = Proof::from_str(&to_string(new_handle).unwrap()).unwrap();
        assert_eq!(proof1, proof2);
    }

    #[test]
    fn test_release_proof() {
        init!("true");
        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        assert!(release(handle).is_ok());
        assert!(!is_valid_handle(handle));
    }

    #[test]
    fn test_send_proof_request() {
        init!("true");

        let connection_handle = build_test_connection();
        connection::set_agent_verkey(connection_handle, VERKEY).unwrap();
        connection::set_agent_did(connection_handle, DID).unwrap();
        connection::set_their_pw_verkey(connection_handle, VERKEY).unwrap();

        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        assert_eq!(send_proof_request(handle, connection_handle).unwrap(), error::SUCCESS.code_num);
        assert_eq!(get_state(handle).unwrap(), VcxStateType::VcxStateOfferSent as u32);
        assert_eq!(get_proof_uuid(handle).unwrap(), "ntc2ytb");
    }


    #[test]
    fn test_send_proof_request_fails_with_no_pw() {
        //This test has 2 purposes:
        //1. when send_proof_request fails, Ok(c.send_proof_request(connection_handle)?) returns error instead of Ok(_)
        //2. Test that when no PW connection exists, send message fails on invalid did
        init!("true");

        let connection_handle = build_test_connection();
        connection::set_pw_did(connection_handle, "").unwrap();

        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();

        assert!(send_proof_request(handle, connection_handle).is_err());
    }

    #[test]
    fn test_get_proof_fails_with_no_proof() {
        init!("true");
        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        assert!(is_valid_handle(handle));
        assert!(get_proof(handle).is_err())
    }

    #[test]
    fn test_update_state_with_pending_proof() {
        init!("true");

        let mut proof = Box::new(Proof {
            source_id: "12".to_string(),
            msg_uid: String::from("1234"),
            ref_msg_id: String::new(),
            requested_attrs: String::from("[]"),
            requested_predicates: String::from("[]"),
            prover_did: String::from("GxtnGN6ypZYgEqcftSQFnC"),
            prover_vk: VERKEY.to_string(),
            state: VcxStateType::VcxStateOfferSent,
            proof_state: ProofStateType::ProofUndefined,
            name: String::new(),
            version: String::from("1.0"),
            nonce: generate_nonce().unwrap(),
            proof: None,
            proof_request: None,
            remote_did: DID.to_string(),
            remote_vk: VERKEY.to_string(),
            agent_did: DID.to_string(),
            agent_vk: VERKEY.to_string(),
            revocation_interval: RevocationInterval { from: None, to: None },
            thread: Some(Thread::new()),
        });

        httpclient::set_next_u8_response(PROOF_RESPONSE.to_vec());
        httpclient::set_next_u8_response(UPDATE_PROOF_RESPONSE.to_vec());

        proof.update_state(None).unwrap();
        assert_eq!(proof.get_state(), VcxStateType::VcxStateRequestReceived as u32);
    }

    #[test]
    fn test_update_state_with_message() {
        init!("true");

        let mut proof = Box::new(Proof {
            source_id: "12".to_string(),
            msg_uid: String::from("1234"),
            ref_msg_id: String::new(),
            requested_attrs: String::from("[]"),
            requested_predicates: String::from("[]"),
            prover_did: String::from("GxtnGN6ypZYgEqcftSQFnC"),
            prover_vk: VERKEY.to_string(),
            state: VcxStateType::VcxStateOfferSent,
            proof_state: ProofStateType::ProofUndefined,
            name: String::new(),
            version: String::from("1.0"),
            nonce: generate_nonce().unwrap(),
            proof: None,
            proof_request: None,
            remote_did: DID.to_string(),
            remote_vk: VERKEY.to_string(),
            agent_did: DID.to_string(),
            agent_vk: VERKEY.to_string(),
            revocation_interval: RevocationInterval { from: None, to: None },
            thread: Some(Thread::new()),
        });

        proof.update_state(Some(PROOF_RESPONSE_STR.to_string())).unwrap();
        assert_eq!(proof.get_state(), VcxStateType::VcxStateRequestReceived as u32);
    }

    #[test]
    fn test_get_proof_returns_proof_when_proof_state_invalid() {
        init!("true");

        let mut proof = Box::new(Proof {
            source_id: "12".to_string(),
            msg_uid: String::from("1234"),
            ref_msg_id: String::new(),
            requested_attrs: String::from("[]"),
            requested_predicates: String::from("[]"),
            prover_did: String::from("GxtnGN6ypZYgEqcftSQFnC"),
            prover_vk: VERKEY.to_string(),
            state: VcxStateType::VcxStateOfferSent,
            proof_state: ProofStateType::ProofUndefined,
            name: String::new(),
            version: String::from("1.0"),
            nonce: generate_nonce().unwrap(),
            proof: None,
            proof_request: None,
            remote_did: DID.to_string(),
            remote_vk: VERKEY.to_string(),
            agent_did: DID.to_string(),
            agent_vk: VERKEY.to_string(),
            revocation_interval: RevocationInterval { from: None, to: None },
            thread: Some(Thread::new()),
        });

        httpclient::set_next_u8_response(PROOF_RESPONSE.to_vec());
        httpclient::set_next_u8_response(UPDATE_PROOF_RESPONSE.to_vec());
        //httpclient::set_next_u8_response(GET_PROOF_OR_CREDENTIAL_RESPONSE.to_vec());

        proof.update_state(None).unwrap();
        assert_eq!(proof.get_state(), VcxStateType::VcxStateRequestReceived as u32);
        assert_eq!(proof.get_proof_state(), ProofStateType::ProofInvalid as u32);
        assert_eq!(proof.prover_did, "GxtnGN6ypZYgEqcftSQFnC");
        let proof_data = proof.get_proof().unwrap();
        assert!(proof_data.contains(r#""cred_def_id":"NcYxiDXkpYi6ov5FcYDi1e:3:CL:NcYxiDXkpYi6ov5FcYDi1e:2:gvt:1.0""#));
        assert!(proof_data.contains(r#""schema_id":"NcYxiDXkpYi6ov5FcYDi1e:2:gvt:1.0""#));
        /* converting proof to a string produces non-deterministic results */
    }

    #[test]
    fn test_build_credential_defs_json_with_multiple_credentials() {
        init!("true");
        let cred1 = CredInfo {
            schema_id: "schema_key1".to_string(),
            cred_def_id: "cred_def_key1".to_string(),
            rev_reg_id: None,
            timestamp: None
        };
        let cred2 = CredInfo {
            schema_id: "schema_key2".to_string(),
            cred_def_id: "cred_def_key2".to_string(),
            rev_reg_id: None,
            timestamp: None
        };
        let credentials = vec![cred1, cred2];
        let credential_json = Proof::build_credential_defs_json(&credentials).unwrap();

        let json: Value = serde_json::from_str(CRED_DEF_JSON).unwrap();
        let expected = json!({CRED_DEF_ID:json}).to_string();
        assert_eq!(credential_json, expected);
    }

    #[test]
    fn test_build_schemas_json_with_multiple_schemas() {
        init!("true");
        let cred1 = CredInfo {
            schema_id: "schema_key1".to_string(),
            cred_def_id: "cred_def_key1".to_string(),
            rev_reg_id: None,
            timestamp: None
        };
        let cred2 = CredInfo {
            schema_id: "schema_key2".to_string(),
            cred_def_id: "cred_def_key2".to_string(),
            rev_reg_id: None,
            timestamp: None
        };
        let credentials = vec![cred1, cred2];
        let schema_json = Proof::build_schemas_json(&credentials).unwrap();

        let json: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        let expected = json!({SCHEMA_ID:json}).to_string();
        assert_eq!(schema_json, expected);
    }

    #[test]
    fn test_build_rev_reg_defs_json() {
        init!("true");
        let cred1 = CredInfo {
            schema_id: "schema_key1".to_string(),
            cred_def_id: "cred_def_key1".to_string(),
            rev_reg_id: Some("id1".to_string()),
            timestamp: None
        };
        let cred2 = CredInfo {
            schema_id: "schema_key2".to_string(),
            cred_def_id: "cred_def_key2".to_string(),
            rev_reg_id: Some("id2".to_string()),
            timestamp: None
        };
        let credentials = vec![cred1, cred2];
        let rev_reg_defs_json = Proof::build_rev_reg_defs_json(&credentials).unwrap();

        let json: Value = serde_json::from_str(&rev_def_json()).unwrap();
        let expected = json!({REV_REG_ID:json}).to_string();
        assert_eq!(rev_reg_defs_json, expected);
    }

    #[test]
    fn test_build_rev_reg_json() {
        init!("true");
        let cred1 = CredInfo {
            schema_id: "schema_key1".to_string(),
            cred_def_id: "cred_def_key1".to_string(),
            rev_reg_id: Some("id1".to_string()),
            timestamp: Some(1),
        };
        let cred2 = CredInfo {
            schema_id: "schema_key2".to_string(),
            cred_def_id: "cred_def_key2".to_string(),
            rev_reg_id: Some("id2".to_string()),
            timestamp: Some(2),
        };
        let credentials = vec![cred1, cred2];
        let rev_reg_json = Proof::build_rev_reg_json(&credentials).unwrap();

        let json: Value = serde_json::from_str(REV_REG_JSON).unwrap();
        let expected = json!({REV_REG_ID:{"1":json}}).to_string();
        assert_eq!(rev_reg_json, expected);
    }

    #[test]
    fn test_get_proof() {
        init!("true");

        let mut proof_msg_obj = ProofMessage::new();
        proof_msg_obj.libindy_proof = PROOF_JSON.to_string();

        let mut proof = create_boxed_proof();
        proof.proof = Some(proof_msg_obj);

        let proof_str = proof.get_proof().unwrap();
        assert_eq!(&proof_str, PROOF_JSON);
    }

    #[test]
    fn test_release_all() {
        init!("true");
        let h1 = create_proof("1".to_string(), REQUESTED_ATTRS.to_owned(), REQUESTED_PREDICATES.to_owned(), r#"{"support_revocation":false}"#.to_string(), "Optional".to_owned()).unwrap();
        let h2 = create_proof("1".to_string(), REQUESTED_ATTRS.to_owned(), REQUESTED_PREDICATES.to_owned(), r#"{"support_revocation":false}"#.to_string(), "Optional".to_owned()).unwrap();
        let h3 = create_proof("1".to_string(), REQUESTED_ATTRS.to_owned(), REQUESTED_PREDICATES.to_owned(), r#"{"support_revocation":false}"#.to_string(), "Optional".to_owned()).unwrap();
        let h4 = create_proof("1".to_string(), REQUESTED_ATTRS.to_owned(), REQUESTED_PREDICATES.to_owned(), r#"{"support_revocation":false}"#.to_string(), "Optional".to_owned()).unwrap();
        let h5 = create_proof("1".to_string(), REQUESTED_ATTRS.to_owned(), REQUESTED_PREDICATES.to_owned(), r#"{"support_revocation":false}"#.to_string(), "Optional".to_owned()).unwrap();
        release_all();
        assert_eq!(release(h1).unwrap_err().kind(), VcxErrorKind::InvalidProofHandle);
        assert_eq!(release(h2).unwrap_err().kind(), VcxErrorKind::InvalidProofHandle);
        assert_eq!(release(h3).unwrap_err().kind(), VcxErrorKind::InvalidProofHandle);
        assert_eq!(release(h4).unwrap_err().kind(), VcxErrorKind::InvalidProofHandle);
        assert_eq!(release(h5).unwrap_err().kind(), VcxErrorKind::InvalidProofHandle);
    }

    #[ignore]
    #[test]
    fn test_proof_validation_with_predicate() {
        use utils::constants::{PROOF_LIBINDY, PROOF_REQUEST};
        init!("false");
        pool::tests::open_sandbox_pool();
        //Generated proof from a script using libindy's python wrapper

        let proof_msg: ProofMessage = serde_json::from_str(PROOF_LIBINDY).unwrap();
        let mut proof_req_msg = ProofRequestMessage::create();
        proof_req_msg.proof_request_data = serde_json::from_str(PROOF_REQUEST).unwrap();
        let mut proof = Proof {
            source_id: "12".to_string(),
            msg_uid: String::from("1234"),
            ref_msg_id: String::new(),
            requested_attrs: String::from("[]"),
            requested_predicates: REQUESTED_PREDICATES.to_string(),
            prover_did: String::from("GxtnGN6ypZYgEqcftSQFnC"),
            prover_vk: VERKEY.to_string(),
            state: VcxStateType::VcxStateRequestReceived,
            proof_state: ProofStateType::ProofUndefined,
            name: String::new(),
            version: String::from("1.0"),
            nonce: generate_nonce().unwrap(),
            proof: Some(proof_msg),
            proof_request: Some(proof_req_msg),
            remote_did: DID.to_string(),
            remote_vk: VERKEY.to_string(),
            agent_did: DID.to_string(),
            agent_vk: VERKEY.to_string(),
            revocation_interval: RevocationInterval { from: None, to: None },
            thread: Some(Thread::new()),
        };
        let rc = proof.proof_validation();
        assert!(rc.is_ok());
        assert_eq!(proof.proof_state, ProofStateType::ProofValidated);

        let proof_data = proof.get_proof().unwrap();
        assert!(proof_data.contains(r#""schema_seq_no":694,"issuer_did":"DunkM3x1y7S4ECgSL4Wkru","credential_uuid":"claim::1f927d68-8905-4188-afd6-374b93202802","attr_info":{"name":"age","value":18,"type":"predicate","predicate_type":"GE"}}"#));
    }

    #[ignore]
    #[test]
    fn test_send_proof_request_can_be_retried() {
        init!("true");

        let connection_handle = build_test_connection();
        connection::set_agent_verkey(connection_handle, VERKEY).unwrap();
        connection::set_agent_did(connection_handle, DID).unwrap();
        connection::set_their_pw_verkey(connection_handle, VERKEY).unwrap();

        let handle = create_proof("1".to_string(),
                                  REQUESTED_ATTRS.to_owned(),
                                  REQUESTED_PREDICATES.to_owned(),
                                  r#"{"support_revocation":false}"#.to_string(),
                                  "Optional".to_owned()).unwrap();
        set_libindy_rc(error::TIMEOUT_LIBINDY_ERROR.code_num);
        assert_eq!(send_proof_request(handle, connection_handle).unwrap_err().kind(), VcxErrorKind::TimeoutLibindy);
        assert_eq!(get_state(handle).unwrap(), VcxStateType::VcxStateInitialized as u32);
        assert_eq!(get_proof_uuid(handle).unwrap(), "");

        // Retry sending proof request
        assert_eq!(send_proof_request(handle, connection_handle).unwrap(), 0);
        assert_eq!(get_state(handle).unwrap(), VcxStateType::VcxStateOfferSent as u32);
        assert_eq!(get_proof_uuid(handle).unwrap(), "ntc2ytb");
    }

    #[test]
    fn test_get_proof_request_status_can_be_retried() {
        init!("true");

        let _new_handle = 1;

        let mut proof = create_boxed_proof();

        httpclient::set_next_u8_response(PROOF_RESPONSE.to_vec());
        httpclient::set_next_u8_response(UPDATE_PROOF_RESPONSE.to_vec());
        //httpclient::set_next_u8_response(GET_PROOF_OR_CREDENTIAL_RESPONSE.to_vec());

        proof.get_proof_request_status(None).unwrap();
        assert_eq!(proof.get_state(), VcxStateType::VcxStateRequestReceived as u32);
        assert_eq!(proof.get_proof_state(), ProofStateType::ProofInvalid as u32);

        // Changing the state and proof state to show that validation happens again
        // and resets the values to received and Invalid
        httpclient::set_next_u8_response(PROOF_RESPONSE.to_vec());
        httpclient::set_next_u8_response(UPDATE_PROOF_RESPONSE.to_vec());
        proof.state = VcxStateType::VcxStateOfferSent;
        proof.proof_state = ProofStateType::ProofUndefined;
        proof.get_proof_request_status(None).unwrap();
        proof.update_state(None).unwrap();
        assert_eq!(proof.get_state(), VcxStateType::VcxStateRequestReceived as u32);
        assert_eq!(proof.get_proof_state(), ProofStateType::ProofInvalid as u32);
    }

    #[test]
    fn test_proof_errors() {
        init!("false");

        let mut proof = create_boxed_proof();

        let bad_handle = 100000;
        // TODO: Do something to guarantee that this handle is bad
        assert_eq!(proof.send_proof_request(bad_handle).unwrap_err().kind(), VcxErrorKind::NotReady);
        // TODO: Add test that returns a INVALID_PROOF_CREDENTIAL_DATA
        assert_eq!(proof.get_proof_request_status(None).unwrap_err().kind(), VcxErrorKind::PostMessageFailed);


        let empty = r#""#;

        assert_eq!(create_proof("my source id".to_string(),
                                empty.to_string(),
                                "{}".to_string(),
                                r#"{"support_revocation":false}"#.to_string(),
                                "my name".to_string()).unwrap_err().kind(), VcxErrorKind::InvalidJson);


        assert_eq!(to_string(bad_handle).unwrap_err().kind(), VcxErrorKind::InvalidHandle);

        assert_eq!(get_source_id(bad_handle).unwrap_err().kind(), VcxErrorKind::InvalidHandle);

        assert_eq!(from_string(empty).unwrap_err().kind(), VcxErrorKind::InvalidJson);

        let mut proof_good = create_boxed_proof();
        assert_eq!(proof_good.get_proof_request_status(None).unwrap_err().kind(), VcxErrorKind::PostMessageFailed);
    }

    #[cfg(feature = "agency")]
    #[cfg(feature = "pool_tests")]
    #[test]
    fn test_proof_verification() {
        init!("ledger");
        let (_, _, proof_req, proof) = ::utils::libindy::anoncreds::tests::create_proof();

        let mut proof_req_obj = ProofRequestMessage::create();
        proof_req_obj.proof_request_data = serde_json::from_str(&proof_req).unwrap();

        let mut proof_msg = ProofMessage::new();
        proof_msg.libindy_proof = proof;

        let mut proof = create_boxed_proof();
        proof.proof = Some(proof_msg);
        proof.proof_request = Some(proof_req_obj);

        let rc = proof.proof_validation();

        println!("{}", serde_json::to_string(&proof).unwrap());
        assert!(rc.is_ok());
        assert_eq!(proof.proof_state, ProofStateType::ProofValidated);
    }

    #[cfg(feature = "agency")]
    #[cfg(feature = "pool_tests")]
    #[test]
    fn test_self_attested_proof_verification() {
        init!("ledger");
        let (proof_req, proof) = ::utils::libindy::anoncreds::tests::create_self_attested_proof();

        let mut proof_req_obj = ProofRequestMessage::create();
        proof_req_obj.proof_request_data = serde_json::from_str(&proof_req).unwrap();

        let mut proof_msg = ProofMessage::new();
        proof_msg.libindy_proof = proof;

        let mut proof = create_boxed_proof();
        proof.proof = Some(proof_msg);
        proof.proof_request = Some(proof_req_obj);

        let rc = proof.proof_validation();

        assert!(rc.is_ok());
        assert_eq!(proof.proof_state, ProofStateType::ProofValidated);
    }

    #[cfg(feature = "agency")]
    #[cfg(feature = "pool_tests")]
    #[test]
    fn test_proof_verification_restrictions() {
        init!("ledger");
        let proof_req = json!({
           "nonce":"123432421212",
           "name":"proof_req_1",
           "version":"0.1",
           "requested_attributes": {
               "address1_1": {
                   "name":"address1",
                   "restrictions": [{ "issuer_did": "Not Here" }]
               },
               "zip_2": { "name":"zip", },
               "self_attest_3": { "name":"self_attest", },
           },
           "requested_predicates": {},
        }).to_string();

        let (_, _, _, proof) = ::utils::libindy::anoncreds::tests::create_proof();

        let mut proof_req_obj = ProofRequestMessage::create();
        proof_req_obj.proof_request_data = serde_json::from_str(&proof_req).unwrap();

        let mut proof_msg = ProofMessage::new();
        proof_msg.libindy_proof = proof;

        let mut proof = create_boxed_proof();
        proof.proof = Some(proof_msg);
        proof.proof_request = Some(proof_req_obj);

        let rc = proof.proof_validation();

        // proof validation should fail because restriction
        rc.unwrap_err(); //FIXME check error code also
        assert_eq!(proof.proof_state, ProofStateType::ProofInvalid);

        // remove restriction, now validation should pass
        proof.proof_state = ProofStateType::ProofUndefined;
        proof.proof_request.as_mut().unwrap()
            .proof_request_data.requested_attributes
            .get_mut("address1_1").unwrap().restrictions = None;
        let rc = proof.proof_validation();

        rc.unwrap();
        assert_eq!(proof.proof_state, ProofStateType::ProofValidated);
    }

    #[cfg(feature = "pool_tests")]
    #[test]
    fn test_proof_validate_attribute() {
        init!("ledger");
        let (_, _, proof_req, proof_json) = ::utils::libindy::anoncreds::tests::create_proof();

        let mut proof_req_obj = ProofRequestMessage::create();

        proof_req_obj.proof_request_data = serde_json::from_str(&proof_req).unwrap();

        let mut proof_msg = ProofMessage::new();
        let mut proof = create_boxed_proof();
        proof.proof_request = Some(proof_req_obj);

        // valid proof_obj
        {
            proof_msg.libindy_proof = proof_json.clone();
            proof.proof = Some(proof_msg);

            let _rc = proof.proof_validation().unwrap();
            assert_eq!(proof.proof_state, ProofStateType::ProofValidated);
        }

        let mut proof_obj: serde_json::Value = serde_json::from_str(&proof_json).unwrap();

        // change Raw value
        {
            let mut proof_msg = ProofMessage::new();
            proof_obj["requested_proof"]["revealed_attrs"]["address1_1"]["raw"] = json!("Other Value");
            let proof_json = serde_json::to_string(&proof_obj).unwrap();

            proof_msg.libindy_proof = proof_json;
            proof.proof = Some(proof_msg);

            let rc = proof.proof_validation();
            rc.unwrap_err();
            assert_eq!(proof.get_proof_state(), ProofStateType::ProofInvalid as u32);
        }

        // change Encoded value
        {
            let mut proof_msg = ProofMessage::new();
            proof_obj["requested_proof"]["revealed_attrs"]["address1_1"]["encoded"] = json!("1111111111111111111111111111111111111111111111111111111111");
            let proof_json = serde_json::to_string(&proof_obj).unwrap();

            proof_msg.libindy_proof = proof_json;
            proof.proof = Some(proof_msg);

            let rc = proof.proof_validation();
            rc.unwrap_err(); //FIXME check error code also
            assert_eq!(proof.get_proof_state(), ProofStateType::ProofInvalid as u32);
        }
    }
}

