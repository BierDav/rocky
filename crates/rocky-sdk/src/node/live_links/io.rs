//! Transport workers own framing and channel routing. Only requests cross to
//! the database executor; a blocked replica cannot stop an active channel.
use super::*;
use rocky_secure::network::NativeSession;
use tokio::sync::{mpsc, oneshot};

pub(super) enum Incoming {
    Request(Box<NativeSession>),
    Signal(Frame),
}
struct ChannelOwner(link::channels::Handle);
impl Drop for ChannelOwner {
    fn drop(&mut self) {
        self.0.close();
    }
}
struct Write {
    frame: Option<Frame>,
    reply: oneshot::Sender<Result<(), Failure>>,
}
pub(super) struct Cancellation {
    pub(super) authority: watch::Receiver<u64>,
    pub(super) stage: watch::Receiver<Stage>,
    pub(super) close: watch::Receiver<Option<CloseReason>>,
}
impl Cancellation {
    async fn reason(mut self) -> CloseReason {
        tokio::select! {
            _ = self.authority.changed() => CloseReason::Authority,
            _ = self.stage.wait_for(|stage| *stage != Stage::Open) => CloseReason::Detached,
            reason = self.close.wait_for(|reason| reason.is_some()) => reason.ok().and_then(|reason| *reason).unwrap_or(CloseReason::Detached),
        }
    }
}
pub(super) struct Io {
    incoming: mpsc::Receiver<Result<Incoming, Failure>>,
    writes: mpsc::Sender<Write>,
    tasks: [tokio::task::JoinHandle<()>; 2],
}
impl Drop for Io {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
#[derive(Clone)]
pub(super) struct Writer {
    writes: mpsc::Sender<Write>,
}
impl Io {
    pub(super) fn writer(&self) -> Writer {
        Writer {
            writes: self.writes.clone(),
        }
    }

    pub(super) fn start(mut opened: link::Link, cancellation: Option<Cancellation>) -> Self {
        let (incoming, receiver) = mpsc::channel(32);
        let (writes, mut sending) = mpsc::channel::<Write>(32);
        let handle = opened.channels.handle.clone();
        let mut outgoing = std::mem::replace(&mut opened.channels.outgoing, mpsc::channel(1).1);
        let direction = opened.direction;
        let expires = opened.expires;
        let mut writer = opened.writer;
        let errors = incoming.clone();
        let owner = ChannelOwner(handle.clone());
        let send = tokio::spawn(async move {
            let _owner = owner;
            let cancelled = async move {
                match cancellation {
                    Some(cancellation) => cancellation.reason().await,
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(cancelled);
            let exit = {
                let send = async {
                    loop {
                        tokio::select! {
                            biased;
                            write = sending.recv() => {
                                let Some(write) = write else { return Ok(()) };
                                if expires <= now()? { return Err("link credentials expired".into()); }
                                let result = match write.frame {
                                    Some(frame) => writer.send(frame).await.map_err(|e| Box::new(e) as Failure),
                                    None => writer.finish().await.map_err(|e| Box::new(e) as Failure),
                                };
                                let failed = result.is_err();
                                let _ = write.reply.send(result);
                                if failed { return Err("link writer failed".into()); }
                            }
                            frame = outgoing.recv() => {
                                let Some(frame) = frame else { return Ok(()) };
                                if expires <= now()? { return Err("link credentials expired".into()); }
                                let finished = if let Message::Reconcile { generation, started: false } = frame.message { Some(generation) } else { None };
                                writer.send(frame).await?;
                                if let Some(generation) = finished { handle.flushed_reconcile(generation); }
                            }
                        }
                    }
                };
                tokio::pin!(send);
                tokio::select! {
                    biased;
                    reason = &mut cancelled => Err(reason),
                    result = &mut send => Ok(result),
                }
            };
            let result: Result<(), Failure> = match exit {
                Ok(result) => result,
                Err(reason) => {
                    let _ = tokio::time::timeout(liveness::CLOSE, async {
                        writer
                            .send(Frame::new(direction, Message::Close { reason }))
                            .await?;
                        writer.finish().await
                    })
                    .await;
                    Err("link cancelled".into())
                }
            };
            if let Err(error) = result {
                let _ = errors.send(Err(error)).await;
            }
        });
        let receive = tokio::spawn(async move {
            let result: Result<(), Failure> = async {
                loop {
                    let frame = opened.reader.receive().await?;
                    if opened.expires <= now()? {
                        return Err("link credentials expired".into());
                    }
                    let event = match frame.message {
                        Message::Channel(_) => {
                            let Some(session) = opened.channels.accept(frame)? else {
                                continue;
                            };
                            Incoming::Request(Box::new(session))
                        }
                        Message::Reconcile {
                            generation,
                            started,
                        } => {
                            opened
                                .channels
                                .reconcile(frame.direction, generation, started)?;
                            continue;
                        }
                        _ => Incoming::Signal(frame),
                    };
                    if incoming.send(Ok(event)).await.is_err() {
                        return Ok(());
                    }
                }
            }
            .await;
            if let Err(error) = result {
                let _ = incoming.send(Err(error)).await;
            }
        });
        Self {
            incoming: receiver,
            writes,
            tasks: [receive, send],
        }
    }
    pub(super) async fn receive(&mut self) -> Result<Incoming, Failure> {
        self.incoming
            .recv()
            .await
            .ok_or_else(|| Failure::from("link reader ended"))?
    }
    pub(super) async fn send(&self, frame: Frame) -> Result<(), Failure> {
        self.writer().send(frame).await
    }
    pub(super) async fn finish(&self) -> Result<(), Failure> {
        self.writer().finish().await
    }
}
impl Writer {
    pub(super) async fn send(&self, frame: Frame) -> Result<(), Failure> {
        self.write(Some(frame)).await
    }
    pub(super) async fn finish(&self) -> Result<(), Failure> {
        self.write(None).await
    }
    async fn write(&self, frame: Option<Frame>) -> Result<(), Failure> {
        let (reply, answer) = oneshot::channel();
        self.writes
            .send(Write { frame, reply })
            .await
            .map_err(|_| "link writer ended")?;
        answer.await.map_err(|_| "link writer ended")?
    }
}
