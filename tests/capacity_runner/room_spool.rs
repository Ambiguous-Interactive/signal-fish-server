//! Bounded append-only storage for the capacity runner's room evidence.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::records::{GapEvent, ReceiptEvent, SentEvent};

// A 16-room capture offers about 246,000 frames/s. This bound can hold a
// measured 161 ms writer stall at that rate.
pub(super) const MAX_CAPTURE_QUEUE_FRAMES: usize = 65_536;
const FRAME_LIMIT: usize = 4096;
const HEADER_BYTES: u64 = 10;
const WRITER_BUFFER: usize = 16 * 1024;
#[cfg(target_os = "linux")]
const FILE_DESCRIPTOR_HEADROOM: u64 = 64;
const WRITER_WAKE_FRAMES: usize = 64;
const WRITER_POLL_INTERVAL: Duration = Duration::from_micros(500);
const FORMAT: u32 = 1;
const INDEX: &str = "rooms.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum RoomEvent<Name = crate::records::ReceiptName> {
    Sent(SentEvent<Name>),
    Receipt(ReceiptEvent<Name>),
    Gap(GapEvent<Name>),
}

impl<Name> RoomEvent<Name> {
    pub(super) fn raw_storage_bytes(&self) -> u64 {
        match self {
            Self::Sent(_) => std::mem::size_of::<SentEvent>() as u64,
            Self::Receipt(_) => std::mem::size_of::<ReceiptEvent>() as u64,
            Self::Gap(_) => std::mem::size_of::<GapEvent>() as u64,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct RoomFile {
    bytes: u64,
    rows: u64,
}

#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    rooms: Vec<RoomFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_queue: Option<CaptureQueue>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct CaptureQueue {
    capacity_frames: usize,
    /// Includes a frame reserved for submission. The writer's active frame
    /// leaves the count when it is dequeued.
    pending_high_water_frames: usize,
}

struct QueueDepth {
    capacity_frames: usize,
    submitted: Cell<usize>,
    high_water: Cell<usize>,
    dequeued: Arc<AtomicUsize>,
}

struct WriterProgress {
    dequeued: Arc<AtomicUsize>,
    count: usize,
}

impl WriterProgress {
    fn release(&mut self) {
        self.count = self.count.wrapping_add(1);
        self.dequeued.store(self.count, Ordering::Relaxed);
    }
}

impl QueueDepth {
    fn new(capacity_frames: usize) -> (Self, WriterProgress) {
        let dequeued = Arc::new(AtomicUsize::new(0));
        let writer = WriterProgress {
            dequeued: Arc::clone(&dequeued),
            count: 0,
        };
        (
            Self {
                capacity_frames,
                submitted: Cell::new(0),
                high_water: Cell::new(0),
                dequeued,
            },
            writer,
        )
    }

    fn reserve(&self) -> usize {
        let submitted = self.submitted.get().wrapping_add(1);
        let estimated = submitted.wrapping_sub(self.dequeued.load(Ordering::Relaxed));
        self.submitted.set(submitted);
        self.record_pending_peak(submitted, estimated);
        estimated
    }

    fn record_pending_peak(&self, submitted: usize, estimated: usize) {
        // An older dequeue count can only raise the estimate.
        let previous_peak = self.high_water.get();
        if estimated > previous_peak {
            // The RMW sees the latest published count in modification order.
            let dequeued = self.dequeued.fetch_add(0, Ordering::Relaxed);
            let pending = submitted.wrapping_sub(dequeued);
            self.high_water.set(previous_peak.max(pending));
        }
    }

    fn cancel_reservation(&self) {
        self.submitted.set(self.submitted.get().wrapping_sub(1));
    }

    fn snapshot(&self) -> CaptureQueue {
        CaptureQueue {
            capacity_frames: self.capacity_frames,
            pending_high_water_frames: self.high_water.get(),
        }
    }
}

struct Frame {
    room: usize,
    event: RoomEvent,
}

pub(super) struct RoomSpool {
    root: PathBuf,
    sender: SyncSender<Frame>,
    writer: JoinHandle<Result<Index, String>>,
    rejected: AtomicBool,
    queue: QueueDepth,
}

impl RoomSpool {
    pub(super) fn create(
        root: &Path,
        rooms: usize,
        queue_frames: usize,
        byte_budget: u64,
    ) -> Result<Self, String> {
        if rooms == 0
            || rooms > 999
            || queue_frames == 0
            || queue_frames > MAX_CAPTURE_QUEUE_FRAMES
            || byte_budget == 0
        {
            return Err("invalid room spool limits".into());
        }
        fs::create_dir(root).map_err(|error| format!("create room spool: {error}"))?;
        // Opening all the bounded room files here keeps the first room-wide
        // burst from competing with file creation on the single writer.
        let writers = open_writers(root, rooms)?;
        let (sender, receiver) = mpsc::sync_channel(queue_frames);
        let path = root.to_path_buf();
        let (queue, mut writer_progress) = QueueDepth::new(queue_frames);
        let writer = thread::Builder::new()
            .name("capacity-room-spool".into())
            .spawn(move || {
                // A blocking channel receive wakes the writer on isolated
                // frames. Wait for a group or a short interval instead.
                let frames = std::iter::from_fn(|| loop {
                    match receiver.try_recv() {
                        Ok(frame) => {
                            writer_progress.release();
                            return Some(frame);
                        }
                        Err(TryRecvError::Empty) => thread::park_timeout(WRITER_POLL_INTERVAL),
                        Err(TryRecvError::Disconnected) => return None,
                    }
                });
                write_rooms(&path, writers, byte_budget, frames)
            })
            .map_err(|error| format!("start room spool writer: {error}"))?;
        Ok(Self {
            root: root.to_path_buf(),
            sender,
            writer,
            rejected: AtomicBool::new(false),
            queue,
        })
    }

    /// The producer retains at most one frame if the writer cannot keep up.
    pub(super) fn push(&self, room: usize, event: RoomEvent) -> Result<(), String> {
        let result = self.send(room, event);
        if result.is_err() {
            self.rejected.store(true, Ordering::Relaxed);
        }
        result
    }

    fn send(&self, room: usize, event: RoomEvent) -> Result<(), String> {
        if self.rejected.load(Ordering::Relaxed) {
            return Err("room spool already rejected evidence".into());
        }
        // Sender and recipient names are the only variable-length fields.
        // Leave headroom for the tag, integers, and string prefixes.
        let name_bytes = match &event {
            RoomEvent::Sent(event) => event.sender.len(),
            RoomEvent::Receipt(event) => event.sender.len().saturating_add(event.recipient.len()),
            RoomEvent::Gap(event) => event.sender.len().saturating_add(event.recipient.len()),
        };
        if name_bytes > FRAME_LIMIT - 256 {
            return Err("room event identity exceeds frame budget".into());
        }
        let pending = self.queue.reserve();
        self.sender
            .try_send(Frame { room, event })
            .map(|()| {
                // Estimated depth gives a wake hint. The timed poll covers missed hints.
                if pending == WRITER_WAKE_FRAMES.min(self.queue.capacity_frames) {
                    self.writer.thread().unpark();
                }
            })
            .map_err(|error| {
                self.queue.cancel_reservation();
                match error {
                    TrySendError::Full(_) => "room spool queue exhausted".into(),
                    TrySendError::Disconnected(_) => "room spool writer unavailable".into(),
                }
            })
    }

    pub(super) fn capture_queue(&self) -> CaptureQueue {
        self.queue.snapshot()
    }

    pub(super) fn finish(self) -> Result<(), String> {
        drop(self.sender);
        self.writer.thread().unpark();
        let mut index = self
            .writer
            .join()
            .map_err(|_| "room spool writer panicked".to_string())??;
        if self.rejected.into_inner() {
            return Err("room spool producer rejected evidence".into());
        }
        index.capture_queue = Some(self.queue.snapshot());
        crate::artifacts::write_json(self.root.join("rooms.pending"), &index)?;
        fs::rename(self.root.join("rooms.pending"), self.root.join(INDEX))
            .map_err(|error| format!("commit room spool index: {error}"))
    }
}

/// Measure the smallest frames that can account for the offered schedule.
/// Roster names have at least four bytes. MessagePack integer widths grow
/// monotonically from zero, so these sentinel encodings give a storage floor.
pub(super) fn planned_disk_floor(config: &crate::config::RunConfig) -> Result<u64, String> {
    use crate::schedule::Phase;

    let records = config.validate_planned_record_budget()?;
    let sends = records
        .checked_div(u64::from(config.players_per_room))
        .ok_or_else(|| "room spool needs at least one player".to_string())?;
    let name = crate::records::ReceiptName::from("r0p0");
    let sent = |phase| {
        RoomEvent::Sent(SentEvent {
            sender: name.clone(),
            room: 0,
            seq: 0,
            epoch: 0,
            intended_us: 0,
            sent_us: 0,
            application_bytes: 0,
            encoded_frame_body_bytes: 0,
            phase,
        })
    };
    let received = RoomEvent::Receipt(ReceiptEvent {
        recipient: name.clone(),
        sender: name.clone(),
        seq: 0,
        epoch: 0,
        server_seq: 0,
        received_us: 0,
        application_bytes: 0,
        encoded_frame_body_bytes: 0,
    });
    let frame_bytes = |event: RoomEvent| {
        rmp_serde::to_vec(&event)
            .map(|bytes| HEADER_BYTES + bytes.len() as u64)
            .map_err(|error| format!("size room spool frame: {error}"))
    };
    let send_bytes = frame_bytes(sent(Phase::Warmup))?.min(frame_bytes(sent(Phase::Measured))?);
    let receipt_bytes = frame_bytes(received)?;
    sends
        .checked_mul(send_bytes)
        .and_then(|bytes| {
            (records - sends)
                .checked_mul(receipt_bytes)?
                .checked_add(bytes)
        })
        .ok_or_else(|| "minimum room spool bytes overflow".to_string())
}

/// Maximum replay rows for the entire capture and for bounded room evaluation.
/// The run-wide receive allowance can all land in a single room. The collector
/// also retains the first row that crosses its receive budget.
pub(super) fn row_limits(config: &crate::config::RunConfig) -> Result<(u64, u64), String> {
    let rooms = u64::from(config.rooms);
    let planned = config.validate_planned_record_budget()?;
    let per_room = planned
        .checked_div(rooms)
        .ok_or_else(|| "room capture needs at least one room".to_string())?;
    let extra = crate::records::extra_inbound_events(rooms * u64::from(config.players_per_room))?;
    let with_headroom = |rows: u64| {
        rows.checked_add(extra)
            .and_then(|rows| rows.checked_add(1))
            .ok_or_else(|| "room replay row count overflows".to_string())
    };
    Ok((with_headroom(planned)?, with_headroom(per_room)?))
}

/// Refuse an obviously unprovisioned capture before creating files or sockets.
pub(super) fn preflight(config: &crate::config::RunConfig, byte_budget: u64) -> Result<(), String> {
    let minimum = planned_disk_floor(config)?;
    if minimum > byte_budget {
        return Err(format!(
            "full offered workload needs at least {minimum} spool bytes, budget is {byte_budget}"
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let path = std::path::absolute(&config.output_dir)
            .map_err(|error| format!("resolve room spool path: {error}"))?;
        let parent = path
            .ancestors()
            .find(|candidate| candidate.exists())
            .ok_or_else(|| "cannot find room spool filesystem".to_string())?;
        let disk = std::process::Command::new("df")
            .args(["--output=avail", "-B1", "--"])
            .arg(parent)
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| format!("read room spool free space: {error}"))?;
        if !disk.status.success() {
            return Err(format!(
                "read room spool free space: {}",
                String::from_utf8_lossy(&disk.stderr)
            ));
        }
        let available = String::from_utf8_lossy(&disk.stdout)
            .split_whitespace()
            .last()
            .and_then(|bytes| bytes.parse::<u64>().ok())
            .ok_or_else(|| "room spool free-space report is invalid".to_string())?;
        if byte_budget > available {
            return Err(format!(
                "room spool byte budget {byte_budget} exceeds available disk bytes {available}"
            ));
        }

        let required = u64::from(config.rooms) * (u64::from(config.players_per_room) + 1)
            + FILE_DESCRIPTOR_HEADROOM;
        if let Some(limit) = linux_file_descriptor_limit()? {
            if required > limit {
                return Err(format!(
                    "room capture needs {required} file descriptors, soft limit is {limit}"
                ));
            }
        }
    }
    Ok(())
}

fn room_path(root: &Path, room: usize) -> PathBuf {
    root.join(format!("{room:03}.msgpack"))
}

fn open_writer(root: &Path, room: usize) -> Result<BufWriter<File>, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(room_path(root, room))
        .map(|file| BufWriter::with_capacity(WRITER_BUFFER, file))
        .map_err(|error| format!("open room {room} spool: {error}"))
}

#[cfg(target_os = "linux")]
fn linux_file_descriptor_limit() -> Result<Option<u64>, String> {
    let limits = fs::read_to_string("/proc/self/limits")
        .map_err(|error| format!("read room capture file descriptor limit: {error}"))?;
    let soft = limits
        .lines()
        .find_map(|line| {
            line.strip_prefix("Max open files")?
                .split_whitespace()
                .next()
        })
        .ok_or_else(|| "room capture file descriptor limit is absent".to_string())?;
    if soft == "unlimited" {
        Ok(None)
    } else {
        soft.parse()
            .map(Some)
            .map_err(|_| format!("room capture file descriptor soft limit is invalid: {soft}"))
    }
}

fn open_writers(root: &Path, rooms: usize) -> Result<Vec<Option<BufWriter<File>>>, String> {
    // Keep lazy opening available for sparse captures when the limit or
    // current usage cannot be established. The Linux runner preflight
    // separately checks the much larger sockets-plus-shards requirement.
    #[cfg(target_os = "linux")]
    let prepare = linux_file_descriptor_limit().is_ok_and(|limit| {
        limit.is_none_or(|limit| {
            fs::read_dir("/proc/self/fd").is_ok_and(|open| {
                open.count() as u64 + rooms as u64 + FILE_DESCRIPTOR_HEADROOM <= limit
            })
        })
    });
    #[cfg(not(target_os = "linux"))]
    let prepare = false;

    if prepare {
        (0..rooms)
            .map(|room| open_writer(root, room).map(Some))
            .collect()
    } else {
        Ok((0..rooms).map(|_| None).collect())
    }
}

fn write_rooms(
    root: &Path,
    mut writers: Vec<Option<BufWriter<File>>>,
    byte_budget: u64,
    frames: impl IntoIterator<Item = Frame>,
) -> Result<Index, String> {
    let mut index = Index {
        version: FORMAT,
        rooms: vec![RoomFile::default(); writers.len()],
        capture_queue: None,
    };
    let mut remaining = byte_budget;
    let mut buffer = [0; FRAME_LIMIT];
    for (ordinal, Frame { room, event }) in frames.into_iter().enumerate() {
        let file = index
            .rooms
            .get_mut(room)
            .ok_or_else(|| format!("room {room} is outside the spool"))?;
        let mut output = std::io::Cursor::new(buffer.as_mut_slice());
        rmp_serde::encode::write(&mut output, &event)
            .map_err(|error| format!("encode room {room} event: {error}"))?;
        let size = u16::try_from(output.position()).expect("cursor sits within the frame");
        let bytes = &buffer[..usize::from(size)];
        let framed = u64::from(size) + HEADER_BYTES;
        remaining = remaining
            .checked_sub(framed)
            .ok_or_else(|| format!("room spool byte budget {byte_budget} exhausted"))?;
        let slot = &mut writers[room];
        if slot.is_none() {
            *slot = Some(open_writer(root, room)?);
        }
        let writer = slot.as_mut().expect("open room spool writer");
        writer
            .write_all(&size.to_le_bytes())
            .and_then(|()| writer.write_all(&(ordinal as u64).to_le_bytes()))
            .and_then(|()| writer.write_all(bytes))
            .map_err(|error| format!("write room {room} spool: {error}"))?;
        file.bytes += framed;
        file.rows += 1;
    }
    for (room, writer) in writers.iter_mut().enumerate() {
        if let Some(writer) = writer {
            writer
                .flush()
                .map_err(|error| format!("flush room {room} spool: {error}"))?;
        }
    }
    Ok(index)
}

pub(super) struct RoomReplay {
    root: PathBuf,
    index: Index,
}

impl RoomReplay {
    pub(super) fn open(root: &Path, rooms: usize) -> Result<Self, String> {
        let file = File::open(root.join(INDEX))
            .map_err(|error| format!("read complete room spool index: {error}"))?;
        let bytes = file.metadata().map_err(|error| error.to_string())?.len();
        if bytes > 128 * 1024 {
            return Err("room spool index exceeds byte limit".into());
        }
        let index: Index = serde_json::from_reader(BufReader::new(file))
            .map_err(|error| format!("decode room spool index: {error}"))?;
        if index.version != FORMAT || index.rooms.len() != rooms || !(1..=999).contains(&rooms) {
            return Err(
                "room spool index does not match the supported format and room count".into(),
            );
        }
        Ok(Self {
            root: root.to_path_buf(),
            index,
        })
    }

    pub(super) fn visit(
        &self,
        room: usize,
        row_budget: u64,
        mut visit: impl FnMut(u64, RoomEvent<String>),
    ) -> Result<(), String> {
        let mut reader = self.reader(room, row_budget)?;
        let mut frame = [0; FRAME_LIMIT];
        while let Some((ordinal, event)) = reader.next(&mut frame)? {
            visit(ordinal, event);
        }
        Ok(())
    }

    /// Merge the room streams in capture order with one pending event per room.
    pub(super) fn visit_ordered(
        &self,
        row_budget: u64,
        visit: impl FnMut(RoomEvent<String>),
    ) -> Result<(), String> {
        let mut frame = [0; FRAME_LIMIT];
        self.visit_ordered_with(row_budget, |reader| reader.next(&mut frame), visit)
    }

    pub(super) fn validate_ordinals(&self, row_budget: u64) -> Result<(), String> {
        let mut frame = [0; FRAME_LIMIT];
        self.visit_ordered_with(
            row_budget,
            |reader| {
                Ok(reader
                    .next_frame(&mut frame)?
                    .map(|(ordinal, _)| (ordinal, ())))
            },
            |()| {},
        )
    }

    fn visit_ordered_with<T>(
        &self,
        row_budget: u64,
        mut next: impl FnMut(&mut ReplayRoom) -> Result<Option<(u64, T)>, String>,
        mut visit: impl FnMut(T),
    ) -> Result<(), String> {
        let mut rooms = Vec::with_capacity(self.index.rooms.len());
        let mut pending = BTreeMap::new();
        let mut remaining = row_budget;
        for (room, file) in self.index.rooms.iter().enumerate() {
            remaining = remaining
                .checked_sub(file.rows)
                .ok_or_else(|| format!("room spool exceeds replay row budget {row_budget}"))?;
            let mut reader = self.reader(room, file.rows)?;
            if let Some((ordinal, event)) = next(&mut reader)? {
                if pending.insert(ordinal, (room, event)).is_some() {
                    return Err(format!("room spool repeats ordinal {ordinal}"));
                }
            }
            rooms.push(reader);
        }
        let mut expected = 0;
        while let Some((ordinal, (room, event))) = pending.pop_first() {
            if ordinal != expected {
                return Err(format!(
                    "room spool expected ordinal {expected}, found {ordinal}"
                ));
            }
            visit(event);
            expected += 1;
            if let Some((ordinal, event)) = next(&mut rooms[room])? {
                if pending.insert(ordinal, (room, event)).is_some() {
                    return Err(format!("room spool repeats ordinal {ordinal}"));
                }
            }
        }
        if expected != row_budget - remaining {
            return Err("room spool is missing capture ordinals".into());
        }
        Ok(())
    }

    pub(super) fn fits_room_row_limit(&self, limit: u64) -> bool {
        self.index.rooms.iter().all(|room| room.rows <= limit)
    }

    pub(super) fn validate_limits(&self, mut rows: u64, mut bytes: u64) -> Result<(), String> {
        for file in &self.index.rooms {
            rows = rows
                .checked_sub(file.rows)
                .ok_or_else(|| "room spool exceeds replay record budget".to_string())?;
            bytes = bytes
                .checked_sub(file.bytes)
                .ok_or_else(|| "room spool exceeds replay byte budget".to_string())?;
        }
        Ok(())
    }

    pub(super) fn restore_hot_records(
        &self,
        records: &mut crate::records::RunRecords,
        row_budget: u64,
        raw_event_budget: u64,
    ) -> Result<(), String> {
        let mut names = crate::records::ReceiptNames::default();
        let mut remaining = Some(raw_event_budget);
        self.visit_ordered(row_budget, |event| {
            remaining = remaining.and_then(|bytes| bytes.checked_sub(event.raw_storage_bytes()));
            if remaining.is_none() {
                return;
            }
            match event {
                RoomEvent::Sent(event) => records.sent.push(names.share_sent(event)),
                RoomEvent::Receipt(event) => records.receipts.push(names.share(event)),
                RoomEvent::Gap(event) => records.gaps.push(names.share_gap(event)),
            }
        })?;
        if remaining.is_none() {
            return Err(format!(
                "captured raw events exceed replay byte budget {raw_event_budget}"
            ));
        }
        crate::records::sort_sent_events(&mut records.sent);
        Ok(())
    }

    fn reader(&self, room: usize, row_budget: u64) -> Result<ReplayRoom, String> {
        let RoomFile { bytes, rows } = *self
            .index
            .rooms
            .get(room)
            .ok_or_else(|| format!("room {room} is outside the spool"))?;
        if rows > row_budget {
            return Err(format!(
                "room {room} exceeds replay row budget {row_budget}"
            ));
        }
        let path = room_path(&self.root, room);
        let input = if rows == 0 && bytes == 0 && !path.exists() {
            None
        } else {
            let file =
                File::open(path).map_err(|error| format!("read room {room} spool: {error}"))?;
            if file.metadata().map_err(|error| error.to_string())?.len() != bytes {
                return Err(format!("room {room} spool byte count differs from index"));
            }
            Some(BufReader::new(file))
        };
        Ok(ReplayRoom {
            input,
            remaining: rows,
            previous: None,
        })
    }
}

struct ReplayRoom {
    input: Option<BufReader<File>>,
    remaining: u64,
    previous: Option<u64>,
}

impl ReplayRoom {
    fn next(
        &mut self,
        frame: &mut [u8; FRAME_LIMIT],
    ) -> Result<Option<(u64, RoomEvent<String>)>, String> {
        let Some((ordinal, bytes)) = self.next_frame(frame)? else {
            return Ok(None);
        };
        let mut deserializer = rmp_serde::Deserializer::new(std::io::Cursor::new(bytes));
        deserializer.set_max_depth(8);
        let event = RoomEvent::deserialize(&mut deserializer)
            .map_err(|error| format!("invalid frame: {error}"))?;
        if deserializer.position() != bytes.len() as u64 {
            return Err("trailing bytes in room event frame".into());
        }
        Ok(Some((ordinal, event)))
    }

    fn next_frame<'a>(
        &mut self,
        frame: &'a mut [u8; FRAME_LIMIT],
    ) -> Result<Option<(u64, &'a [u8])>, String> {
        let Some(reader) = &mut self.input else {
            return Ok(None);
        };
        if self.remaining == 0 {
            if reader
                .read(&mut frame[..1])
                .map_err(|error| error.to_string())?
                != 0
            {
                return Err("trailing rows in room spool".into());
            }
            self.input = None;
            return Ok(None);
        }
        let mut header = [0; 2];
        reader
            .read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        let mut ordinal = [0; 8];
        reader
            .read_exact(&mut ordinal)
            .map_err(|error| error.to_string())?;
        let ordinal = u64::from_le_bytes(ordinal);
        if self.previous.is_some_and(|last| ordinal <= last) {
            return Err("room event ordinals are out of order".into());
        }
        self.previous = Some(ordinal);
        let size = usize::from(u16::from_le_bytes(header));
        let bytes = frame
            .get_mut(..size)
            .ok_or_else(|| format!("frame exceeds {FRAME_LIMIT} bytes"))?;
        reader
            .read_exact(bytes)
            .map_err(|error| error.to_string())?;
        self.remaining -= 1;
        Ok(Some((ordinal, bytes)))
    }
}

#[cfg(test)]
mod tests;
