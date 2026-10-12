//! UI-independent retained plot state. Wire indices and server identities are distinct.

use std::collections::{HashMap, VecDeque};

use jgd_protocol::{FrameMessage, Plot};

/// Stable within one running server; never derived from an R replay number.
pub type PlotId = u64;

/// A retained drawing and its source replay identity.
#[derive(Debug, Clone)]
pub struct StoredPlot {
    pub id: PlotId,
    pub revision: u64,
    pub replay_number: Option<u32>,
    pub plot: Plot,
}

/// Maximum number of plots retained after their R connections disconnect.
/// Oldest disconnected sessions are evicted as a whole, preserving local indices.
pub const MAX_OFFLINE_PLOTS: usize = 256;

#[derive(Default)]
pub(crate) struct PlotHistory {
    pub(crate) sessions: HashMap<String, Vec<StoredPlot>>,
    next_id: PlotId,
    disconnected: VecDeque<String>,
}

impl PlotHistory {
    /// Apply an R frame, returning its legacy renderer-local history index.
    /// Unknown historical targets never fall back to the latest drawing.
    pub(crate) fn apply(&mut self, sid: &str, frame: &FrameMessage) -> Option<usize> {
        if frame.plot_index.is_some() && !self.sessions.contains_key(sid) {
            return None;
        }
        let plots = self.sessions.entry(sid.to_owned()).or_default();
        let target = if let Some(number) = frame.plot_index {
            plots.iter().position(|p| p.replay_number == Some(number))?
        } else if plots.is_empty() || frame.new_page == Some(true) {
            // A repeated source number is not a second page.
            if let Some(number) = frame.plot_number
                && plots.iter().any(|p| p.replay_number == Some(number))
            {
                return None;
            }
            self.next_id += 1;
            plots.push(StoredPlot {
                id: self.next_id,
                revision: 1,
                replay_number: frame.plot_number,
                plot: frame.plot.clone(),
            });
            return Some(plots.len() - 1);
        } else {
            plots.len() - 1
        };

        let stored = &mut plots[target];
        if frame.plot_index.is_none() {
            if let (Some(old), Some(new)) = (stored.replay_number, frame.plot_number)
                && old != new
            {
                return None;
            }
            if stored.replay_number.is_none() {
                stored.replay_number = frame.plot_number;
            }
        }
        if frame.incremental && frame.resize_replay != Some(true) {
            stored.plot.ops.extend(frame.plot.ops.iter().cloned());
            stored.plot.device = frame.plot.device.clone();
        } else {
            stored.plot = frame.plot.clone();
        }
        stored.revision += 1;
        Some(target)
    }

    pub(crate) fn get(&self, sid: &str, index: Option<usize>) -> Option<&StoredPlot> {
        let plots = self.sessions.get(sid)?;
        match index {
            Some(index) => plots.get(index),
            None => plots.last(),
        }
    }

    pub(crate) fn disconnect(&mut self, sid: String) {
        if self.sessions.contains_key(&sid) {
            self.disconnected.push_back(sid);
        }
        let mut offline_count: usize = self
            .disconnected
            .iter()
            .filter_map(|sid| self.sessions.get(sid))
            .map(Vec::len)
            .sum();
        while offline_count > MAX_OFFLINE_PLOTS {
            let Some(sid) = self.disconnected.pop_front() else {
                break;
            };
            if let Some(plots) = self.sessions.remove(&sid) {
                offline_count -= plots.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jgd_protocol::{DeviceInfo, DrawingOp};

    fn frame(number: Option<u32>, new_page: bool) -> FrameMessage {
        FrameMessage {
            plot: Plot {
                session_id: Some("s".into()),
                ops: vec![DrawingOp::Clip {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 100.0,
                }],
                device: DeviceInfo {
                    width: 800.0,
                    height: 600.0,
                    dpi: None,
                    bg: None,
                },
            },
            incremental: false,
            new_page: Some(new_page),
            resize_replay: None,
            plot_index: None,
            plot_number: number,
            ext: None,
        }
    }

    #[test]
    fn identity_revision_and_replay_number_are_independent() {
        let mut history = PlotHistory::default();
        assert_eq!(history.apply("s", &frame(Some(17), true)), Some(0));
        let first = history.get("s", Some(0)).unwrap().id;
        assert_eq!(history.apply("s", &frame(Some(23), true)), Some(1));
        let latest = history.get("s", None).unwrap().clone();
        assert_ne!(first, latest.id);

        let mut replay = frame(None, false);
        replay.plot_index = Some(17);
        replay.resize_replay = Some(true);
        replay.plot.device.width = 400.0;
        assert_eq!(history.apply("s", &replay), Some(0));
        let stored = history.get("s", Some(0)).unwrap();
        assert_eq!(stored.id, first);
        assert_eq!(stored.revision, 2);
        assert_eq!(stored.replay_number, Some(17));
        assert_eq!(stored.plot.device.width, 400.0);
        assert_eq!(history.get("s", None).unwrap().plot, latest.plot);
        assert_eq!(history.get("s", None).unwrap().revision, 1);
    }

    #[test]
    fn zero_replay_number_is_mapped_and_distinct_from_missing() {
        let mut history = PlotHistory::default();
        history.apply("s", &frame(None, true));
        assert_eq!(history.get("s", Some(0)).unwrap().replay_number, None);
        history.apply("s", &frame(Some(0), true));
        let id = history.get("s", Some(1)).unwrap().id;
        let mut replay = frame(None, false);
        replay.plot_index = Some(0);
        replay.resize_replay = Some(true);
        replay.plot.device.width = 400.0;
        assert_eq!(history.apply("s", &replay), Some(1));
        let stored = history.get("s", Some(1)).unwrap();
        assert_eq!(stored.id, id);
        assert_eq!(stored.replay_number, Some(0));
        assert_eq!(stored.revision, 2);
        assert_eq!(stored.plot.device.width, 400.0);
        assert_eq!(history.get("s", Some(0)).unwrap().revision, 1);
    }

    #[test]
    fn unknown_replays_and_conflicting_numbers_do_not_change_history() {
        let mut history = PlotHistory::default();
        history.apply("s", &frame(Some(10), true));
        let mut replay = frame(None, false);
        replay.plot_index = Some(0); // A local index is not an R replay number.
        replay.resize_replay = Some(true);
        assert_eq!(history.apply("s", &replay), None);
        assert_eq!(history.apply("s", &frame(Some(99), false)), None);
        assert_eq!(history.apply("s", &frame(Some(10), true)), None);
        assert_eq!(history.sessions["s"].len(), 1);
        assert_eq!(history.get("s", None).unwrap().revision, 1);
    }

    #[test]
    fn deltas_append_and_complete_frames_replace_without_changing_identity() {
        let mut history = PlotHistory::default();
        history.apply("s", &frame(None, true));
        let id = history.get("s", None).unwrap().id;
        let mut delta = frame(Some(8), false);
        delta.incremental = true;
        history.apply("s", &delta);
        let stored = history.get("s", None).unwrap();
        assert_eq!(stored.id, id);
        assert_eq!(stored.revision, 2);
        assert_eq!(stored.replay_number, Some(8));
        assert_eq!(stored.plot.ops.len(), 2);
        history.apply("s", &frame(Some(8), false));
        let stored = history.get("s", None).unwrap();
        assert_eq!(stored.id, id);
        assert_eq!(stored.revision, 3);
        assert_eq!(stored.plot.ops.len(), 1);
    }

    #[test]
    fn offline_limit_evicts_whole_oldest_sessions_but_not_live_plots() {
        let mut history = PlotHistory::default();
        history.apply("live", &frame(Some(0), true));
        for number in 0..MAX_OFFLINE_PLOTS as u32 {
            history.apply("old", &frame(Some(number), true));
        }
        history.disconnect("old".into());
        assert_eq!(history.sessions["old"].len(), MAX_OFFLINE_PLOTS);
        history.apply("new", &frame(Some(0), true));
        let new_id = history.get("new", None).unwrap().id;
        history.disconnect("new".into());
        assert!(!history.sessions.contains_key("old"));
        assert!(history.sessions.contains_key("live"));
        assert_eq!(history.get("new", None).unwrap().id, new_id);
        assert_eq!(history.sessions["new"].len(), 1);
    }

    #[test]
    fn oversized_disconnected_session_is_evicted_entirely() {
        let mut history = PlotHistory::default();
        for number in 0..=MAX_OFFLINE_PLOTS as u32 {
            history.apply("s", &frame(Some(number), true));
        }
        history.disconnect("s".into());
        assert!(history.sessions.is_empty());
        assert!(history.disconnected.is_empty());
    }
}
