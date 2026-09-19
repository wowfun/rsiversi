//! Bounded snapshot and follower streams, independent of transcript retention.
use super::{
    Arc, Attachment, BTreeMap, Chunk, Duration, Follower, MAXIMUM_ATTACHMENTS,
    MAXIMUM_FOLLOWER_BYTES, MAXIMUM_OUTPUT_PAGE_BYTES, OutputPage, OwnedSemaphorePermit, Phase,
    PtyError, Result, Semaphore, Size, Snapshot, Term, TermState, Terminal, VecDeque, lock, native,
    reserve, screen_bytes, snapshot, snapshot_bytes, unavailable,
};
// A snapshot is shared only by followers within this locked terminal. A slow
// follower refreshes its shared group so the old reservation can be recycled.
struct SnapshotReplacement {
    followers: Vec<String>,
    permit: OwnedSemaphorePermit,
    maximum: usize,
}
impl SnapshotReplacement {
    fn reserve(
        state: &TermState,
        budget: &Arc<Semaphore>,
        size: Size,
        group: Option<*const Snapshot>,
    ) -> Result<Self> {
        let maximum = snapshot_bytes(size)?;
        let mut snapshots = BTreeMap::new();
        let mut followers = Vec::new();
        for (id, follower) in &state.followers {
            let key = Arc::as_ptr(&follower.snapshot);
            if group.is_some_and(|group| group != key) {
                continue;
            }
            follower
                .stream_epoch
                .checked_add(1)
                .ok_or(PtyError::Capacity)?;
            followers.push(id.clone());
            let entry = snapshots.entry(key).or_insert((&follower.snapshot, 0));
            entry.1 += 1;
        }
        let credit: usize = snapshots
            .values()
            .filter(|(snapshot, count)| Arc::strong_count(snapshot) == *count)
            .map(|(snapshot, _)| snapshot.permit.num_permits())
            .sum();
        let permit = reserve(budget, maximum.saturating_sub(credit))?;
        Ok(Self {
            followers,
            permit,
            maximum,
        })
    }
    fn replace(mut self, state: &mut TermState, budget: &Arc<Semaphore>) -> Result<()> {
        let empty = Arc::new(Snapshot {
            text: String::new(),
            permit: reserve(budget, 0)?,
        });
        for id in &self.followers {
            let follower = state.followers.get_mut(id).expect("reserved follower");
            let previous = std::mem::replace(&mut follower.snapshot, empty.clone());
            if let Ok(previous) = Arc::try_unwrap(previous) {
                drop(previous.text);
                self.permit.merge(previous.permit);
            }
            // All fallible epoch admission was checked before native resize or mutation.
            follower.reset(empty.clone())?;
        }
        let excess = self.permit.num_permits() - self.maximum;
        drop(self.permit.split(excess));
        let snapshot = snapshot(&state.parser, self.maximum, self.permit)?;
        for id in self.followers {
            let follower = state.followers.get_mut(&id).expect("reserved follower");
            follower.end = snapshot.text.len() as u64;
            follower.oldest = follower.end;
            follower.snapshot = snapshot.clone();
        }
        Ok(())
    }
}
impl Follower {
    pub(super) fn new(snapshot: Arc<Snapshot>, queue: OwnedSemaphorePermit, epoch: u64) -> Self {
        let end = snapshot.text.len() as u64;
        Self {
            last_read: std::time::Instant::now(),
            snapshot,
            stream_epoch: epoch,
            chunks: VecDeque::new(),
            oldest: end,
            end,
            bytes: 0,
            reading: Arc::new(Semaphore::new(1)),
            _queue: queue,
        }
    }
    fn reset(&mut self, snapshot: Arc<Snapshot>) -> Result<()> {
        self.stream_epoch = self.stream_epoch.checked_add(1).ok_or(PtyError::Capacity)?;
        self.end = snapshot.text.len() as u64;
        self.oldest = self.end;
        self.snapshot = snapshot;
        self.chunks.clear();
        self.bytes = 0;
        Ok(())
    }
    fn push(&mut self, text: Arc<str>) -> Result<()> {
        let start = self.end;
        self.end = self
            .end
            .checked_add(text.len() as u64)
            .ok_or(PtyError::Capacity)?;
        self.bytes += chunk_cost(text.len());
        self.chunks.push_back(Chunk { start, text });
        while self.bytes > MAXIMUM_FOLLOWER_BYTES {
            let chunk = self.chunks.pop_front().expect("nonempty retained output");
            self.bytes -= chunk_cost(chunk.text.len());
            self.oldest = chunk.start + chunk.text.len() as u64;
        }
        Ok(())
    }
    fn page(&self, cursor: u64) -> Result<String> {
        if cursor > self.end {
            return Err(PtyError::Invalid(
                "terminal output cursor exceeds the stream".into(),
            ));
        }
        if cursor < (self.snapshot.text.len() as u64) {
            return slice(
                &self.snapshot.text,
                usize::try_from(cursor).expect("cursor lies in an allocated snapshot"),
            );
        }
        if cursor < self.oldest {
            return Err(PtyError::Invalid("terminal output cursor expired".into()));
        }
        let start = self
            .chunks
            .partition_point(|chunk| chunk.start + chunk.text.len() as u64 <= cursor);
        let mut page = String::new();
        for chunk in self.chunks.iter().skip(start) {
            let offset = usize::try_from(cursor.saturating_sub(chunk.start))
                .expect("cursor lies in an allocated chunk");
            if !chunk.text.is_char_boundary(offset) {
                return Err(PtyError::Invalid("terminal cursor splits UTF-8".into()));
            }
            let mut end = chunk
                .text
                .len()
                .min(offset + MAXIMUM_OUTPUT_PAGE_BYTES - page.len());
            while !chunk.text.is_char_boundary(end) {
                end -= 1;
            }
            page.push_str(&chunk.text[offset..end]);
            if end < chunk.text.len() || page.len() == MAXIMUM_OUTPUT_PAGE_BYTES {
                break;
            }
        }
        Ok(page)
    }
}
fn slice(text: &str, start: usize) -> Result<String> {
    if !text.is_char_boundary(start) {
        return Err(PtyError::Invalid("terminal cursor splits UTF-8".into()));
    }
    let mut end = text.len().min(start + MAXIMUM_OUTPUT_PAGE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(text[start..end].to_owned())
}
impl Term {
    fn replace_snapshot(
        &self,
        state: &mut TermState,
        replacement: SnapshotReplacement,
    ) -> Result<()> {
        if let Err(error) = replacement.replace(state, &self.shared.snapshots) {
            // A violated parser bound must never expose a silently incomplete stream.
            state.status.phase = Phase::Failed;
            self.process.terminate();
            self.changed.notify_waiters();
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn feed(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > rsi_process::MAXIMUM_PTY_IO_BYTES {
            return Err(PtyError::Invalid(
                "native PTY output exceeds its bound".into(),
            ));
        }
        let mut state = lock(&self.inner);
        let bytes = state.filter.feed(bytes);
        if bytes.is_empty() {
            return Ok(());
        }
        state.parser.process(&bytes);
        let text: Arc<str> = String::from_utf8(bytes)
            .map_err(|_| PtyError::Io("filtered terminal output is not UTF-8".into()))?
            .into();
        for follower in state.followers.values_mut() {
            follower.push(text.clone())?;
        }
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub(super) fn attach(&self) -> Result<Attachment> {
        let mut reclaimed = false;
        loop {
            let mut state = lock(&self.inner);
            if state.followers.len() >= MAXIMUM_ATTACHMENTS {
                state.reclaim_followers(std::time::Instant::now());
                if state.followers.len() >= MAXIMUM_ATTACHMENTS {
                    return Err(PtyError::Capacity);
                }
            }
            let (queue, snapshot) = match self
                .shared
                .follower_resources(&state.parser, state.status.size)
            {
                Err(PtyError::Capacity) if !reclaimed => {
                    drop(state);
                    self.shared.reclaim_followers(std::time::Instant::now());
                    reclaimed = true;
                    continue;
                }
                result => result?,
            };
            let id = self.shared.id("view")?;
            state
                .followers
                .insert(id.clone(), Follower::new(snapshot, queue, 1));
            return Ok(Attachment {
                terminal: state.status.clone(),
                id,
                stream_epoch: 1,
            });
        }
    }
    fn page(&self, attachment: &str, epoch: u64, cursor: u64) -> Result<OutputPage> {
        let mut state = lock(&self.inner);
        let follower = state
            .followers
            .get_mut(attachment)
            .ok_or_else(unavailable)?;
        follower.last_read = std::time::Instant::now();
        let expired = epoch == follower.stream_epoch
            && cursor >= follower.snapshot.text.len() as u64
            && cursor < follower.oldest;
        if expired {
            let group = Arc::as_ptr(&follower.snapshot);
            let replacement = SnapshotReplacement::reserve(
                &state,
                &self.shared.snapshots,
                state.status.size,
                Some(group),
            )?;
            self.replace_snapshot(&mut state, replacement)?;
        }
        let follower = state.followers.get(attachment).expect("validated follower");
        let reset = epoch != follower.stream_epoch;
        let cursor = if reset { 0 } else { cursor };
        let text = follower.page(cursor)?;
        Ok(OutputPage {
            terminal: state.status.clone(),
            attachment: attachment.into(),
            stream_epoch: follower.stream_epoch,
            reset,
            cursor,
            next_cursor: cursor + text.len() as u64,
            text,
        })
    }
    pub(super) async fn read(
        &self,
        attachment: &str,
        epoch: u64,
        cursor: u64,
    ) -> Result<OutputPage> {
        let reading = {
            let state = lock(&self.inner);
            state
                .followers
                .get(attachment)
                .ok_or_else(unavailable)?
                .reading
                .clone()
        };
        let _permit = reading
            .try_acquire_owned()
            .map_err(|_| PtyError::Capacity)?;
        let changed = self.changed.notified();
        let page = self.page(attachment, epoch, cursor)?;
        if !page.text.is_empty() || page.reset || !matches!(page.terminal.phase, Phase::Running) {
            return Ok(page);
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), changed).await;
        self.page(attachment, epoch, cursor)
    }
    pub(super) fn resize(&self, attachment: &str, epoch: u64, size: Size) -> Result<Terminal> {
        size.validate()?;
        let mut state = lock(&self.inner);
        Self::controller(&state, attachment, epoch)?;
        let screen_size = Size {
            rows: state.screen_size.rows.max(size.rows),
            columns: state.screen_size.columns.max(size.columns),
        };
        let screen = reserve(
            &self.shared.screens,
            screen_bytes(screen_size)? - state.screen.num_permits(),
        )?;
        let replacement = if state.followers.is_empty() {
            None
        } else {
            Some(SnapshotReplacement::reserve(
                &state,
                &self.shared.snapshots,
                size,
                None,
            )?)
        };
        self.process.resize(size.native()).map_err(native)?;
        state.parser.screen_mut().set_size(size.rows, size.columns);
        state.status.size = size;
        state.screen.merge(screen);
        state.screen_size = screen_size;
        if let Some(replacement) = replacement {
            self.replace_snapshot(&mut state, replacement)?;
        }
        let status = state.status.clone();
        drop(state);
        self.changed.notify_waiters();
        Ok(status)
    }
}

fn chunk_cost(bytes: usize) -> usize {
    bytes + 2 * std::mem::size_of::<Chunk>() + 2 * std::mem::size_of::<usize>()
}
