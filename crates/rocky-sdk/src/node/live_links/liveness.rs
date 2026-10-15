use super::*;
use std::collections::VecDeque;
use tokio::sync::mpsc;

const CLOSE: Duration = Duration::from_millis(100);
enum Exit {
    Finished(Result<&'static str, Failure>),
    Close(CloseReason),
}
impl Node {
    pub(super) async fn run_link(
        &self,
        service: &Link,
        mut opened: link::Link,
        mut close: watch::Receiver<Option<CloseReason>>,
        changed: watch::Sender<Option<Vec<ResourceScope>>>,
        mut authority: watch::Receiver<u64>,
    ) -> Result<&'static str, Failure> {
        let peer = opened.peer;
        let direction = opened.direction;
        let expires = opened.expires;
        let mut stage = service.stage.subscribe();
        let mut changes = changed.subscribe();
        let mut outgoing = std::mem::replace(&mut opened.channels.outgoing, mpsc::channel(1).1);
        let exit = {
            let receive = async {
                let mut requests = futures_util::stream::FuturesUnordered::<
                    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Failure>>>>,
                >::new();
                let mut seen = VecDeque::new();
                loop {
                    let frame = {
                        let receive = opened.reader.receive();
                        tokio::pin!(receive);
                        loop {
                            tokio::select! {
                                result = &mut receive => break result?,
                                Some(result) = requests.next(), if !requests.is_empty() => { result?; }
                            }
                        }
                    };
                    if expires <= now()? {
                        return Err("link credentials expired".into());
                    }
                    if matches!(frame.message, Message::Channel(_)) {
                        if let Some(mut session) = opened.channels.accept(frame)? {
                            requests.push(Box::pin(async move {
                                let result = session
                                    .serve_all(
                                        &self.replica,
                                        &delivery::Port {
                                            node: self,
                                            serving: true,
                                        },
                                        Some(self),
                                    )
                                    .await;
                                let result = result.map_err(|error| Box::new(error) as Failure);
                                let absorbed = self.absorb_revocations(result.as_ref().err()).await;
                                result?;
                                absorbed?;
                                self.topics_changed();
                                session.close().await?;
                                Ok(())
                            }));
                        }
                        continue;
                    }
                    match frame.message {
                        Message::Reconcile {
                            generation,
                            started,
                        } => opened
                            .channels
                            .reconcile(frame.direction, generation, started)?,
                        Message::Push {
                            scope,
                            frames,
                            voids,
                        } if frame.direction != direction => {
                            if requests.len() >= 8 {
                                return Err("link request queue quota".into());
                            }
                            let handle = opened.channels.handle.clone();
                            requests.push(Box::pin(async move {
                                let _publishing = service.live.publishing.lock().await;
                                let (complete, received) = handle
                                    .receive_push(&self.replica, scope, frames, voids)
                                    .await?;
                                self.received_push(service, peer, scope, received).await?;
                                if !complete {
                                    self.link_pending(peer);
                                }
                                Ok(())
                            }));
                        }
                        Message::Changed { scopes, token } if frame.direction != direction => {
                            if scopes.iter().any(|scope| !opened.offered.contains(scope)) {
                                return Err("unproved link signal scope".into());
                            }
                            if !seen.contains(&token) {
                                if seen.len() == 64 {
                                    seen.pop_front();
                                }
                                seen.push_back(token);
                                if requests.len() >= 8 {
                                    return Err("link request queue quota".into());
                                }
                                // Handlers may hold the replica while awaiting storage. Poll
                                // signal work beside them instead of blocking their progress.
                                requests.push(Box::pin(async move {
                                    if self.signal_token(&scopes).await? != token {
                                        self.link_pending(peer);
                                    }
                                    Ok(())
                                }));
                            }
                        }
                        Message::Close {
                            reason: CloseReason::Authority | CloseReason::Busy,
                        } => return Err("peer closed link".into()),
                        Message::Close { reason } => return Ok::<_, Failure>(close_reason(reason)),
                        _ => return Err("invalid link direction".into()),
                    }
                }
            };
            // A dead peer surfaces as a failed read or write: the transport
            // keeps the connection alive and times it out.
            let send = async {
                changes.borrow_and_update();
                let mut initial = take_signal(&changed);
                loop {
                    if expires <= now()? {
                        return Err("link credentials expired".into());
                    }
                    tokio::select! {
                        biased;
                        Some(frame) = outgoing.recv() => {
                            let finished = if let Message::Reconcile { generation, started: false } = frame.message { Some(generation) } else { None };
                            opened.writer.send(frame).await?;
                            if let Some(generation) = finished { opened.channels.handle.flushed_reconcile(generation); }
                        }
                        _ = changes.changed() => {
                            changes.borrow_and_update();
                            let pending = take_signal(&changed);
                            if let Some(scopes) = pending {
                                opened.writer.send(Frame::new(direction, Message::Changed { token: self.signal_token(&scopes).await?, scopes })).await?;
                            }
                        }
                        _ = std::future::ready(()), if initial.is_some() => {
                            let scopes = initial.take().ok_or("initial link signal")?;
                            opened.writer.send(Frame::new(direction, Message::Changed { token: self.signal_token(&scopes).await?, scopes })).await?;
                        }
                    }
                }
            };
            tokio::pin!(receive, send);
            tokio::select! {
                biased;
                _ = authority.changed() => Exit::Close(CloseReason::Authority),
                _ = stage.wait_for(|stage| *stage != Stage::Open) => Exit::Close(CloseReason::Detached),
                reason = close.wait_for(|reason| reason.is_some()) => Exit::Close((*reason?).ok_or("link close reason")?),
                result = &mut receive => Exit::Finished(result),
                result = &mut send => Exit::Finished(result),
            }
        };
        match exit {
            Exit::Finished(result) => result,
            Exit::Close(reason) => {
                #[cfg(test)]
                service.live.closing.fetch_add(1, Ordering::SeqCst);
                // Cancellation also interrupts a blocked channel write.
                let _ = tokio::time::timeout(CLOSE, async {
                    opened
                        .writer
                        .send(Frame::new(direction, Message::Close { reason }))
                        .await?;
                    opened.writer.finish().await
                })
                .await;
                if reason == CloseReason::Authority {
                    self.reopen_link(peer);
                }
                Ok(close_reason(reason))
            }
        }
    }
}

// Taking a queued batch must clear it so later scopes cannot be hidden by it.
fn take_signal(changed: &watch::Sender<Option<Vec<ResourceScope>>>) -> Option<Vec<ResourceScope>> {
    let mut pending = None;
    changed.send_if_modified(|value| {
        pending = value.take();
        false
    });
    pending
}

fn close_reason(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Authority => "authority",
        CloseReason::Duplicate => "duplicate",
        CloseReason::NetworkingDisabled => "networking_disabled",
        CloseReason::Suspended => "suspended",
        CloseReason::WorkerDisabled => "worker_disabled",
        CloseReason::Detached => "detached",
        CloseReason::Busy => "busy",
    }
}
