//! Latest-state input transport: disk latency must not block host camera/input.
use super::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct Writer {
    pending: Option<Arc<Mutex<Option<Vec<u8>>>>>,
}
impl Writer {
    pub(super) fn publish(&mut self, path: PathBuf, packet: Vec<u8>) {
        if self.pending.is_none() {
            let pending = Arc::new(Mutex::new(None::<Vec<u8>>));
            let weak = Arc::downgrade(&pending);
            std::thread::spawn(move || {
                let mut file = None;
                loop {
                    let Some(slot) = weak.upgrade() else { break };
                    let packet = slot.lock().ok().and_then(|mut s| s.take());
                    drop(slot);
                    if let Some(packet) = packet {
                        if file.is_none() {
                            file = std::fs::OpenOptions::new().write(true).create(true).open(&path).ok();
                        }
                        if let Some(output) = file.as_mut() {
                            // Keep the existing invalid-header/commit-header wire contract.
                            let result = (|| -> std::io::Result<()> {
                                output.seek(SeekFrom::Start(0))?;
                                output.write_all(&[0; 16])?;
                                output.set_len(packet.len() as u64)?;
                                output.seek(SeekFrom::Start(16))?;
                                output.write_all(&packet[16..])?;
                                output.seek(SeekFrom::Start(0))?;
                                output.write_all(&packet[..16])
                            })();
                            if result.is_err() { file = None; }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            });
            self.pending = Some(pending);
        }
        if let Some(slot) = &self.pending {
            if let Ok(mut pending) = slot.lock() { *pending = Some(packet); }
        }
    }
}
