//! Independent lane workers and reconciliation delivery barriers.
use super::*;

pub struct Reader {
    source: ReadSource,
}
enum ReadSource {
    Direct(FrameReader),
    Lanes(LaneReader),
}
struct LaneReader {
    incoming: tokio::sync::mpsc::Receiver<Result<(usize, Option<Frame>), Error>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    received: Vec<u64>,
    pending: std::collections::VecDeque<Frame>,
    ended: std::collections::BTreeSet<usize>,
    ending: Option<tokio::time::Instant>,
}
impl Drop for LaneReader {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
pub struct Writer {
    sink: WriteSink,
}
enum WriteSink {
    Direct(Box<FrameWriter>),
    Lanes(Vec<LaneWriter>),
}
struct LaneWriter {
    sender: tokio::sync::mpsc::Sender<Frame>,
    task: tokio::task::JoinHandle<Result<(), Error>>,
    sent: u64,
}
impl Drop for LaneWriter {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn lane(frame: &Frame) -> usize {
    // Optimistic row pushes share a bulk lane, leaving signals and control
    // on their own stream even when a pushed frame waits for delivery.
    if matches!(frame.message, Message::Push { .. }) {
        1
    } else {
        usize::from(frame.channel.saturating_sub(1))
    }
}
fn lane_binding(binding: Commitment, index: usize) -> Result<Commitment, Error> {
    Ok(rocky_core::commitment(
        "rocky.link-lane.v1",
        &encode(&(binding, index))?,
    ))
}
impl Link {
    /// Binds transport lanes to this authenticated link. Generic byte-stream
    /// transports keep their single ordered stream.
    pub fn attach_lanes(&mut self, lanes: Vec<Box<dyn SessionStream>>) -> Result<(), Error> {
        if lanes.is_empty() {
            return Ok(());
        }
        if lanes.len() != 4 {
            return Err(denied("link lane count"));
        }
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let ReadSource::Direct(reader) = &self.reader.source else {
            return Err(denied("link lanes already attached"));
        };
        let WriteSink::Direct(writer) = &self.writer.sink else {
            return Err(denied("link lanes already attached"));
        };
        for (index, stream) in lanes.into_iter().enumerate() {
            let (read, write) = tokio::io::split(stream);
            let binding = lane_binding(reader.signing, index + 1)?;
            reads.push(FrameReader {
                stream: read,
                peer: reader.peer,
                signing: binding,
                sequence: 0,
            });
            writes.push(FrameWriter {
                stream: write,
                local: writer.local.clone(),
                signing: binding,
                sequence: 0,
            });
        }
        // Swap through empty lane containers to transfer ownership of the control stream.
        let (sender, incoming) = tokio::sync::mpsc::channel(64);
        let empty = LaneReader {
            incoming,
            tasks: Vec::new(),
            received: Vec::new(),
            pending: Default::default(),
            ended: Default::default(),
            ending: None,
        };
        let ReadSource::Direct(control) =
            std::mem::replace(&mut self.reader.source, ReadSource::Lanes(empty))
        else {
            return Err(denied("link lanes already attached"));
        };
        reads.insert(0, control);
        let mut received = Vec::new();
        let mut tasks = Vec::new();
        for (index, mut reader) in reads.into_iter().enumerate() {
            received.push(reader.sequence);
            let sender = sender.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    let result = reader.receive().await.and_then(|frame| {
                        if lane(&frame) != index {
                            return Err(denied("link frame on wrong lane"));
                        }
                        Ok((index, Some(frame)))
                    });
                    let ended = matches!(&result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof);
                    let result = if ended { Ok((index, None)) } else { result };
                    let failed = ended || result.is_err();
                    if sender.send(result).await.is_err() || failed {
                        break;
                    }
                }
            }));
        }
        if let ReadSource::Lanes(reader) = &mut self.reader.source {
            reader.tasks = tasks;
            reader.received = received;
        }
        let WriteSink::Direct(control) =
            std::mem::replace(&mut self.writer.sink, WriteSink::Lanes(Vec::new()))
        else {
            return Err(denied("link lanes already attached"));
        };
        writes.insert(0, *control);
        let mut workers = Vec::new();
        for mut writer in writes {
            let (sender, mut outgoing) = tokio::sync::mpsc::channel(64);
            let sent = writer.sequence;
            let task = tokio::spawn(async move {
                while let Some(frame) = outgoing.recv().await {
                    writer.send(frame).await?;
                }
                writer.finish().await
            });
            workers.push(LaneWriter { sender, task, sent });
        }
        self.writer.sink = WriteSink::Lanes(workers);
        Ok(())
    }
}
impl Reader {
    pub(super) fn new(reader: FrameReader) -> Self {
        Self {
            source: ReadSource::Direct(reader),
        }
    }
    pub async fn receive(&mut self) -> Result<Frame, Error> {
        let reader = match &mut self.source {
            ReadSource::Direct(reader) => return reader.receive().await,
            ReadSource::Lanes(reader) => reader,
        };
        loop {
            if let Some(frame) = reader.pending.front()
                && frame
                    .through
                    .iter()
                    .zip(&reader.received)
                    .all(|(required, received)| received >= required)
            {
                return reader
                    .pending
                    .pop_front()
                    .ok_or_else(|| denied("link barrier disappeared"));
            }
            let next = match reader.ending {
                Some(deadline) => tokio::time::timeout_at(deadline, reader.incoming.recv())
                    .await
                    .map_err(|_| timed_out("link lane ended before link close"))?,
                None => reader.incoming.recv().await,
            };
            let (index, frame) = next.ok_or_else(|| Error::Io(closed_by_peer()))??;
            let Some(frame) = frame else {
                // FINs on independent streams may overtake the signed Close
                // and final request data. Drain them, with a bounded deadline.
                reader.ended.insert(index);
                reader
                    .ending
                    .get_or_insert_with(|| tokio::time::Instant::now() + TIMEOUT);
                if reader.ended.len() == 5 {
                    return Err(Error::Io(closed_by_peer()));
                }
                continue;
            };
            reader.received[index] = reader.received[index]
                .checked_add(1)
                .ok_or_else(|| denied("link sequence exhausted"))?;
            if !frame.through.is_empty()
                || (!reader.pending.is_empty()
                    && matches!(frame.message, Message::Reconcile { .. }))
            {
                if reader.pending.len() >= 4 {
                    return Err(denied("link barrier quota"));
                }
                reader.pending.push_back(frame);
            } else {
                return Ok(frame);
            }
        }
    }
}
impl Writer {
    pub(super) fn new(writer: FrameWriter) -> Self {
        Self {
            sink: WriteSink::Direct(Box::new(writer)),
        }
    }
    pub async fn send(&mut self, mut frame: Frame) -> Result<(), Error> {
        frame.validate()?;
        let writers = match &mut self.sink {
            WriteSink::Direct(writer) => return writer.send(frame).await,
            WriteSink::Lanes(writers) => writers,
        };
        if matches!(frame.message, Message::Reconcile { started: false, .. }) {
            frame.through = writers.iter().map(|writer| writer.sent).collect();
        }
        let writer = writers
            .get_mut(lane(&frame))
            .ok_or_else(|| denied("link writer ended"))?;
        timeout(TIMEOUT, writer.sender.send(frame))
            .await
            .map_err(|_| timed_out("link lane write timeout"))?
            .map_err(|_| denied("link lane writer ended"))?;
        writer.sent = writer
            .sent
            .checked_add(1)
            .ok_or_else(|| denied("link sequence exhausted"))?;
        Ok(())
    }
    pub async fn finish(&mut self) -> Result<(), Error> {
        match &mut self.sink {
            WriteSink::Direct(writer) => writer.finish().await,
            WriteSink::Lanes(writers) => {
                for writer in writers.iter_mut() {
                    let (placeholder, _) = tokio::sync::mpsc::channel(1);
                    drop(std::mem::replace(&mut writer.sender, placeholder));
                }
                for writer in writers.iter_mut() {
                    timeout(TIMEOUT, &mut writer.task)
                        .await
                        .map_err(|_| timed_out("link lane finish timeout"))?
                        .map_err(|error| Error::Application(Box::new(error)))??;
                }
                writers.clear();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_final_marker_waits_for_its_request_lanes_but_signals_pass() {
        let (send, incoming) = tokio::sync::mpsc::channel(8);
        let mut reader = Reader {
            source: ReadSource::Lanes(LaneReader {
                incoming,
                tasks: Vec::new(),
                received: vec![0; 5],
                pending: Default::default(),
                ended: Default::default(),
                ending: None,
            }),
        };
        let mut done = Frame::new(
            false,
            Message::Reconcile {
                generation: 0,
                started: false,
            },
        );
        done.through = vec![0, 1, 0, 0, 0];
        send.send(Ok((2, None))).await.unwrap();
        send.send(Ok((0, Some(done)))).await.unwrap();
        send.send(Ok((
            0,
            Some(Frame::new(false, Message::Ready { persistent: true })),
        )))
        .await
        .unwrap();
        assert!(matches!(
            reader.receive().await.unwrap().message,
            Message::Ready { .. }
        ));
        let data = Frame {
            channel: 2,
            through: Vec::new(),
            direction: false,
            message: Message::Channel(channels::Packet::End { generation: 0 }),
        };
        send.send(Ok((1, Some(data)))).await.unwrap();
        assert!(matches!(
            reader.receive().await.unwrap().message,
            Message::Channel(_)
        ));
        assert!(matches!(
            reader.receive().await.unwrap().message,
            Message::Reconcile { started: false, .. }
        ));
    }
}
