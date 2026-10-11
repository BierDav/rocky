use super::*;
use std::collections::VecDeque;
use tokio::sync::mpsc;

const CLOSE: Duration = Duration::from_millis(100);
enum Exit {
    Finished(Result<(), Failure>),
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
    ) -> Result<(), Failure> {
        let peer = opened.peer;
        let direction = opened.direction;
        let expires = opened.expires;
        let mut stage = service.stage.subscribe();
        let mut changes = changed.subscribe();
        let exit = {
            let (controls, mut control) = mpsc::channel(4);
            let pongs = watch::channel(0u64).0;
            let mut pong = pongs.subscribe();
            let receive = async {
                let mut seen = VecDeque::new();
                loop {
                    let frame = opened.reader.receive().await?;
                    if expires <= now()? {
                        return Err("link credentials expired".into());
                    }
                    match frame.message {
                        Message::Changed { scopes, token } if frame.direction != direction => {
                            if scopes.iter().any(|scope| !opened.offered.contains(scope)) {
                                return Err("unproved link signal scope".into());
                            }
                            if !seen.contains(&token) {
                                if seen.len() == 64 {
                                    seen.pop_front();
                                }
                                seen.push_back(token);
                                if self.signal_token(&scopes).await? != token {
                                    self.link_pending(peer);
                                }
                            }
                        }
                        Message::Ping if frame.direction != direction => {
                            controls
                                .try_send(Frame::new(frame.direction, Message::Pong))
                                .map_err(|_| "link control quota")?;
                        }
                        Message::Pong if frame.direction == direction => {
                            #[cfg(test)]
                            service.live.pongs.fetch_add(1, Ordering::Relaxed);
                            pongs.send_modify(|count| *count = count.wrapping_add(1));
                        }
                        Message::Close {
                            reason: CloseReason::Duplicate,
                        } => return Ok::<_, Failure>(()),
                        Message::Close { .. } => return Err("peer closed link".into()),
                        _ => return Err("invalid link direction".into()),
                    }
                }
            };
            let send = async {
                let mut deadline = tokio::time::Instant::now() + PING;
                let mut missed = 0;
                changes.borrow_and_update();
                let mut initial = take_signal(&changed);
                loop {
                    if expires <= now()? {
                        return Err("link credentials expired".into());
                    }
                    tokio::select! {
                        biased;
                        _ = pong.changed() => { pong.borrow_and_update(); missed = 0; }
                        Some(frame) = control.recv() => { opened.writer.send(frame).await?; }
                        _ = tokio::time::sleep_until(deadline) => {
                            if missed == 3 { return Err("three missed link pongs".into()); }
                            opened.writer.send(Frame::new(direction, Message::Ping)).await?;
                            missed += 1;
                            deadline = tokio::time::Instant::now() + PING;
                        }
                        _ = changes.changed() => {
                            changes.borrow_and_update();
                            let pending = take_signal(&changed);
                            if let Some(scopes) = pending {
                                opened.writer.send(Frame::new(direction, Message::Changed { token: self.signal_token(&scopes).await?, scopes })).await?;
                            }
                            deadline = tokio::time::Instant::now() + PING;
                        }
                        _ = std::future::ready(()), if initial.is_some() => {
                            let scopes = initial.take().ok_or("initial link signal")?;
                            opened.writer.send(Frame::new(direction, Message::Changed { token: self.signal_token(&scopes).await?, scopes })).await?;
                            deadline = tokio::time::Instant::now() + PING;
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
                Ok(())
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
