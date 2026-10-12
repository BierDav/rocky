//! Strong command transfer: execution, confirmed results and custody
//! acknowledgments. The coordinator sees metadata only.
use super::*;

#[derive(Serialize, Deserialize)]
pub(super) enum CommandMessage {
    Execute {
        command: EncryptedCommand,
        scopes: Vec<ResourceScope>,
    },
    ResultPull {
        domain: rocky_core::Scope,
        sequence: u64,
        scopes: Vec<ResourceScope>,
    },
    ResultPush {
        result: ConfirmedCommand,
    },
    Result {
        result: Option<ConfirmedCommand>,
        /// The sender's signed receipt evidence: the result stays verifiable
        /// after the coordinator retired its identity.
        evidence: Option<Vec<u8>>,
    },
    ResultAck {
        states: Vec<(ResourceScope, bool)>,
    },
    /// The answering peer runs no coordination: it executes no command and
    /// keeps no results to exchange. The session goes on.
    Unavailable,
}
/// The refusal of a peer without coordination; see `CommandMessage::Unavailable`.
pub const NO_COORDINATION: &str = "the peer runs no coordination";

impl CommandMessage {
    pub(super) fn scopes(&self) -> Vec<ResourceScope> {
        match self {
            Self::Execute { scopes, .. } | Self::ResultPull { scopes, .. } => scopes.clone(),
            Self::ResultPush { result } => result.parts.iter().map(|part| part.scope).collect(),
            _ => Vec::new(),
        }
    }
}

impl NativeSession {
    pub async fn execute_command<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        command: EncryptedCommand,
        scopes: Vec<ResourceScope>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<ConfirmedCommand, Error> {
        let request = command.request()?;
        let expected_request = request.hash()?;
        let invocation: EncryptedPackage = decode(&request.arguments)?;
        self.ensure_holding(replica, &scopes).await?;
        self.ensure_holding(replica, &[invocation.scope]).await?;
        self.send(CommandMessage::Execute {
            command,
            scopes: scopes.clone(),
        })
        .await?;
        let CommandMessage::Result {
            result: Some(result),
            evidence,
        } = self.reply::<CommandMessage>().await?
        else {
            return Err(denied("missing native command result"));
        };
        if result.strong_package()?.request.hash()? != expected_request
            || result
                .parts
                .iter()
                .any(|part| !scopes.contains(&part.scope))
        {
            return Err(denied("result outside subscription"));
        }
        if let Some(evidence) = evidence {
            coordinator.import_evidence(&evidence).await?;
        }
        replica
            .receive_encrypted_result(result.clone(), coordinator)
            .await?;
        self.send_result_ack(replica, &result).await?;
        Ok(result)
    }
    pub async fn push_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: ConfirmedCommand,
    ) -> Result<(), Error> {
        let scopes: Vec<_> = result
            .parts
            .iter()
            .map(|part| part.scope)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.ensure_holding(replica, &scopes).await?;
        self.send(CommandMessage::ResultPush {
            result: result.clone(),
        })
        .await?;
        let CommandMessage::ResultAck { states } = self.reply::<CommandMessage>().await? else {
            return Err(denied("missing result custody acknowledgment"));
        };
        replica
            .acknowledge_result(self.peer, &self.credentials, &result, &states)
            .await
    }
    async fn send_result_ack<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: &ConfirmedCommand,
    ) -> Result<(), Error> {
        self.send(CommandMessage::ResultAck {
            states: replica.result_ack(result).await?,
        })
        .await?;
        self.acknowledged("missing result acknowledgment response")
            .await
    }
    async fn send_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        result: Option<ConfirmedCommand>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        let evidence = match &result {
            Some(result) => coordinator.export_evidence(&result.receipt()?).await?,
            None => None,
        };
        self.send(CommandMessage::Result {
            result: result.clone(),
            evidence,
        })
        .await?;
        if let Some(result) = result {
            let CommandMessage::ResultAck { states } = self.reply::<CommandMessage>().await? else {
                return Err(denied("missing named result acknowledgment"));
            };
            replica
                .acknowledge_result(self.peer, &self.credentials, &result, &states)
                .await?;
            self.send(Message::Acknowledged).await?;
        }
        Ok(())
    }
    pub async fn pull_result<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        domain: rocky_core::Scope,
        sequence: u64,
        scopes: Vec<ResourceScope>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<bool, Error> {
        self.ensure_holding(replica, &scopes).await?;
        self.send(CommandMessage::ResultPull {
            domain,
            sequence,
            scopes: scopes.clone(),
        })
        .await?;
        let (result, evidence) = match self.reply::<CommandMessage>().await? {
            CommandMessage::Result { result, evidence } => (result, evidence),
            CommandMessage::Unavailable => return Err(denied(NO_COORDINATION)),
            _ => return Err(denied("unexpected native result response")),
        };
        let Some(result) = result else {
            return Ok(false);
        };
        let package = result.strong_package()?;
        if package.request.scope != domain
            || package.sequence != sequence
            || result
                .parts
                .iter()
                .any(|part| !scopes.contains(&part.scope))
        {
            return Err(denied("result outside subscription/sequence"));
        }
        if let Some(evidence) = evidence {
            coordinator.import_evidence(&evidence).await?;
        }
        replica
            .receive_encrypted_result(result.clone(), coordinator)
            .await?;
        self.send_result_ack(replica, &result).await?;
        Ok(true)
    }
    /// Serve a strong invocation or result transfer; the coordinator sees metadata only.
    pub async fn serve_command_once<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        loop {
            match self.receive().await? {
                Message::Commands(message) => {
                    return self.answer_command(replica, coordinator, message).await;
                }
                Message::Holdings(message) => self.answer_holdings(replica, message).await?,
                _ => return Err(denied("unexpected native command request")),
            }
        }
    }
    /// Serves row sync and command transfers until the peer ends the
    /// session. An executed command passes the admission of a local one.
    pub async fn serve_with_commands<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
    ) -> Result<(), Error> {
        loop {
            let message = match self.answered().await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            };
            match message {
                Message::Commands(message) => {
                    self.answer_command(replica, coordinator, message).await?;
                }
                message => self.answer(replica, message).await?,
            }
        }
    }
    pub(super) async fn require_command_holding<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: &CommandMessage,
    ) -> Result<(), Error> {
        for scope in message.scopes() {
            self.require_holding(replica, scope).await?;
        }
        if let CommandMessage::Execute { command, .. } = message {
            let request = command.request()?;
            let invocation: EncryptedPackage = decode(&request.arguments)?;
            self.require_holding(replica, invocation.scope).await?;
        }
        Ok(())
    }

    pub(super) async fn answer_command<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: &dyn rocky_core::Coordinator,
        message: CommandMessage,
    ) -> Result<(), Error> {
        self.require_command_holding(replica, &message).await?;
        match message {
            CommandMessage::Execute { command, scopes } => {
                replica
                    .authorize_command_transfer(self.peer, &self.credentials, &scopes)
                    .await?;
                let result = replica.execute_encrypted(command, coordinator).await?;
                let package = result.strong_package()?;
                let result = replica
                    .encrypted_result_for(
                        self.peer,
                        &self.credentials,
                        package.request.scope,
                        package.sequence,
                        &scopes,
                    )
                    .await?;
                self.send_result(replica, result, coordinator).await
            }
            CommandMessage::ResultPull {
                domain,
                sequence,
                scopes,
            } => {
                let result = replica
                    .encrypted_result_for(self.peer, &self.credentials, domain, sequence, &scopes)
                    .await?;
                self.send_result(replica, result, coordinator).await
            }
            CommandMessage::ResultPush { result } => {
                let scopes: Vec<_> = result
                    .parts
                    .iter()
                    .map(|part| part.scope)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                replica
                    .authorize_command_transfer(self.peer, &self.credentials, &scopes)
                    .await?;
                replica
                    .receive_encrypted_result(result.clone(), coordinator)
                    .await?;
                self.send(CommandMessage::ResultAck {
                    states: replica.result_ack(&result).await?,
                })
                .await
            }
            _ => Err(denied("unexpected native command request")),
        }
    }
}
