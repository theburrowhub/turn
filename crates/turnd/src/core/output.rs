//! Getting terminal output to the clients that are watching.
//!
//! One task per *watched* node, subscribed to that pty's broadcast channel. It exists
//! only while somebody is attached: an unwatched pane's output has already reached its
//! buffer, which is what a later attach replays from, so pushing it to nobody would be
//! work done to be thrown away. With thirty sessions open and one on screen, that is
//! the difference between a daemon that idles and one that does not.
//!
//! The pump coalesces. A pty hands over whatever the kernel had, which for an
//! interactive prompt is a keystroke's echo and for a build is a screenful; sending one
//! frame per read would put a JSON envelope around three bytes hundreds of times a
//! second. Batching for a few milliseconds costs latency nobody can perceive and turns
//! that into one frame.
//!
//! Coalescing matters more for cells than it did for bytes: one batch is one screen
//! diff, so a program printing a hundred lines inside the window produces a single
//! update carrying the rows that ended up different, rather than a hundred updates most
//! of which describe rows that have already scrolled away.

use super::command::Command;
use super::Core;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use turn_core::ids::NodeId;
use turn_pty::OutputChunk;

/// How long a pump gathers output before sending it on.
///
/// Under the threshold where a human notices echo latency, and long enough that a
/// process writing line by line produces one frame per screenful rather than one per
/// line.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(8);

impl Core {
    /// Starts pumping a node's output, if it is not already being pumped.
    ///
    /// The subscription is taken here, synchronously, rather than inside the spawned
    /// task: a broadcast receiver starts collecting from the moment it exists, so a
    /// caller that subscribes before taking a replay cannot lose the bytes in between.
    /// Deferring it to the task would leave a scheduling-sized hole in the stream.
    ///
    /// A second client attaching to a node that is already being pumped shares the pump
    /// and does not get a new subscription, so its replay and the live stream meet
    /// wherever the existing pump happens to be. That is what `pane_output_gap` and
    /// re-attaching are for.
    pub(crate) fn start_pump(&mut self, node: &NodeId) {
        if self.pumps.contains_key(node) {
            return;
        }
        let Some(process) = self.processes.get(node) else {
            return;
        };
        let receiver = process.pty.subscribe();
        let commands = self.commands.clone();
        let id = node.clone();
        let handle = tokio::spawn(pump(id, receiver, commands));
        self.pumps.insert(node.clone(), handle);
    }

    /// Stops pumping a node nobody is attached to any more, and lets go of the screen
    /// that was being kept for whoever was rendering it.
    pub(crate) fn stop_pump_if_unwatched(&mut self, node: &NodeId) {
        if self.is_watched(node) {
            return;
        }
        if let Some(pump) = self.pumps.remove(node) {
            pump.abort();
        }
        // The pty's own buffer is still authoritative and still there; what goes is the
        // copy that existed only to diff against. Thirty unwatched panes hold none.
        self.forget_screen(node);
    }
}

/// Reads a pty's broadcast, coalesces, and hands batches to the core task.
async fn pump(
    node: NodeId,
    mut receiver: broadcast::Receiver<OutputChunk>,
    commands: mpsc::Sender<Command>,
) {
    loop {
        let mut batch: Vec<u8> = Vec::new();
        let mut dropped: u64 = 0;

        // Block until there is something to send.
        match receiver.recv().await {
            Ok(chunk) => batch.extend_from_slice(&chunk),
            Err(RecvError::Lagged(missed)) => {
                // The channel is bounded on purpose: a subscriber that cannot keep up
                // is told so and re-synchronises from a replay, rather than being
                // allowed to grow a queue until the daemon falls over.
                dropped += missed;
            }
            Err(RecvError::Closed) => return,
        }

        // Gather whatever else arrives inside the window.
        let deadline = tokio::time::sleep(COALESCE_WINDOW);
        tokio::pin!(deadline);
        loop {
            if batch.len() >= turn_proto::MAX_OUTPUT_CHUNK_BYTES {
                break;
            }
            tokio::select! {
                biased;
                received = receiver.recv() => match received {
                    Ok(chunk) => batch.extend_from_slice(&chunk),
                    Err(RecvError::Lagged(missed)) => dropped += missed,
                    Err(RecvError::Closed) => break,
                },
                _ = &mut deadline => break,
            }
        }

        if batch.is_empty() && dropped == 0 {
            continue;
        }
        // An awaiting send, not a try_send: output is the one thing worth applying
        // backpressure for. Losing it here would mean a terminal that renders a
        // history it never received, and the pty's own buffer stays authoritative
        // while we wait.
        if commands
            .send(Command::Output {
                node: node.clone(),
                data: batch,
                dropped,
            })
            .await
            .is_err()
        {
            return;
        }
    }
}
